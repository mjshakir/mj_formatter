from __future__ import annotations

from pathlib import Path
import logging
import re
from dataclasses import dataclass

from ..core.edit import Edit
from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy


class LuaPolicy(Policy):
    name = "lua_policy"
    description = "Lua-defined policy"
    parse_mode = "text"

    def __init__(self, config: dict[str, object]) -> None:
        super().__init__(config)
        self._script = str(self._config.get("script", ""))
        self._function = str(self._config.get("function", "apply"))
        self._sandbox = bool(self._config.get("sandbox", True))
        if not self._script:
            raise ValueError("Lua policy requires 'script' path in config")
        self._lua = self._load_lua()
        self._install_helpers()
        self._fn = self._lua.globals()[self._function]
        if self._fn is None:
            raise ValueError(f"Lua policy function not found: {self._function}")

    def apply(self, context: ParseContext) -> PolicyResult:
        result = self._fn(context.text, context.path)
        if result is None:
            return PolicyResult(text=context.text, violations=[], edits=[])
        if isinstance(result, str):
            new_text = result
            violations: list[Violation] = []
        else:
            new_text, violations = self._decode_result(result)
        if new_text == context.text:
            return PolicyResult(text=context.text, violations=[], edits=[])
        if not violations:
            violations = [
                Violation(
                    policy=self.name,
                    message="Lua policy updated content",
                    line=1,
                    column=1,
                )
            ]
        edits = self._diff_edits(context.text, new_text)
        return PolicyResult(text=new_text, violations=violations, edits=edits)

    def _load_lua(self):
        try:
            from lupa import LuaRuntime  # type: ignore
        except Exception as exc:  # pragma: no cover
            raise RuntimeError("Lua support requires 'lupa' installed") from exc
        runtime = LuaRuntime(unpack_returned_tuples=True)
        script_path = Path(self._script)
        code = script_path.read_text(encoding="utf-8")
        runtime.execute(code)
        if self._sandbox:
            runtime.execute(
                "os=nil; io=nil; package=nil; debug=nil; dofile=nil; loadfile=nil; load=nil; require=nil"
            )
        return runtime

    def _install_helpers(self) -> None:
        helpers = self._lua.table()
        helpers["regex_replace"] = self._regex_replace_lua
        helpers["split_lines"] = lambda text: self._lua.table_from(text.splitlines(True))
        helpers["join_lines"] = self._join_lines
        helpers["log"] = self._log
        self._lua.globals()["mj"] = helpers
        self._lua.globals()["mj_config"] = self._config

    def _join_lines(self, lines: object) -> str:
        if isinstance(lines, list):
            return "".join(lines)
        try:
            length = len(lines)  # type: ignore[arg-type]
            items = [lines[i] for i in range(1, length + 1)]  # type: ignore[index]
            return "".join(items)
        except Exception:
            return "".join(list(lines))  # type: ignore[arg-type]

    @dataclass(frozen=True)
    class RegexReplaceArgs:
        text: str
        pattern: str
        repl: str
        flags: str | None = None

    def _regex_replace_lua(self, *args) -> str:
        if len(args) == 1:
            arg = args[0]
            try:
                text = arg["text"]
                pattern = arg["pattern"]
                repl = arg["repl"]
                flags = arg.get("flags") if hasattr(arg, "get") else arg["flags"]
                return self._regex_replace(
                    LuaPolicy.RegexReplaceArgs(
                        text=text,
                        pattern=pattern,
                        repl=repl,
                        flags=flags,
                    )
                )
            except Exception:
                pass
        if len(args) >= 3:
            return self._regex_replace(
                LuaPolicy.RegexReplaceArgs(
                    text=args[0],
                    pattern=args[1],
                    repl=args[2],
                    flags=args[3] if len(args) > 3 else None,
                )
            )
        return args[0] if args else ""

    def _regex_replace(self, args: "LuaPolicy.RegexReplaceArgs") -> str:
        flag_value = 0
        if args.flags:
            for part in args.flags.split("|"):
                part = part.strip().upper()
                if part == "IGNORECASE":
                    flag_value |= re.IGNORECASE
                elif part == "MULTILINE":
                    flag_value |= re.MULTILINE
                elif part == "DOTALL":
                    flag_value |= re.DOTALL
        return re.sub(args.pattern, args.repl, args.text, flags=flag_value)

    def _log(self, level: str, message: str) -> None:
        logger = logging.getLogger("mj_formatter")
        level = str(level).upper()
        if level == "DEBUG":
            logger.debug("%s", message)
        elif level == "WARNING":
            logger.warning("%s", message)
        elif level == "ERROR":
            logger.error("%s", message)
        else:
            logger.info("%s", message)

    def _decode_result(self, result: object) -> tuple[str, list[Violation]]:
        try:
            text = result["text"]  # type: ignore[index]
        except Exception:
            raise ValueError("Lua policy must return string or table with 'text'")
        if not isinstance(text, str):
            raise ValueError("Lua policy table must include 'text' string")
        violations: list[Violation] = []
        try:
            raw_violations = result["violations"]  # type: ignore[index]
        except Exception:
            raw_violations = None
        if raw_violations:
            for item in raw_violations:
                try:
                    message = str(
                        self._table_get(
                            LuaPolicy.TableGetArgs(
                                table=item,
                                key="message",
                                default="Lua policy violation",
                            )
                        )
                    )
                    line = int(
                        self._table_get(
                            LuaPolicy.TableGetArgs(
                                table=item,
                                key="line",
                                default=1,
                            )
                        )
                    )
                    column = self._table_get(
                        LuaPolicy.TableGetArgs(
                            table=item,
                            key="column",
                            default=None,
                        )
                    )
                    column = int(column) if column is not None else None
                except Exception:
                    continue
                violations.append(
                    Violation(
                        policy=self.name,
                        message=message,
                        line=line,
                        column=column,
                    )
                )
        return text, violations

    def _diff_edits(self, before: str, after: str) -> list[Edit]:
        before_lines = before.splitlines()
        after_lines = after.splitlines()
        edits: list[Edit] = []
        for idx, (b, a) in enumerate(zip(before_lines, after_lines)):
            if b != a:
                edits.append(Edit(policy=self.name, line=idx + 1, before=b, after=a))
        if len(after_lines) > len(before_lines):
            for idx in range(len(before_lines), len(after_lines)):
                edits.append(Edit(policy=self.name, line=idx + 1, before="", after=after_lines[idx]))
        return edits

    @dataclass(frozen=True)
    class TableGetArgs:
        table: object
        key: str
        default: object

    def _table_get(self, args: "LuaPolicy.TableGetArgs") -> object:
        if isinstance(args.table, dict):
            return args.table.get(args.key, args.default)
        try:
            value = args.table[args.key]  # type: ignore[index]
        except Exception:
            return args.default
        return value if value is not None else args.default
