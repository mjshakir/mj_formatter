from __future__ import annotations

import subprocess
import shutil

from ..core.types import Edit
from ..core.types import ParseContext
from ..core.types import PolicyResult
from ..core.types import Violation
from ..core.utilities import warn_once
from .policy_base import Policy


class ClangFormatPolicy(Policy):
    name = "clang_format"
    description = "Run clang-format on source text"
    parse_mode = "clang"

    def __init__(self, config: dict[str, object]) -> None:
        super().__init__(config)
        self._command = self._required_str("command")
        self._style = self._required_str("style")
        self._timeout_seconds = self._required_float("timeout_seconds")
        self._extra_args = self._required_str_tuple("extra_args")

    def apply(self, context: ParseContext) -> PolicyResult:
        resolved_command = self._resolve_command(self._command)
        if not resolved_command:
            warn_once(
                "clang_format_missing",
                f"clang_format: configured command not found: {self._command}",
            )
            return PolicyResult(text=context.text, violations=[], edits=[])
        result = self._run(
            command=resolved_command,
            style=self._style,
            assume_filename=context.path,
            extra_args=self._extra_args,
            timeout_seconds=max(0.1, self._timeout_seconds),
            text=context.text,
        )
        if result is None:
            return PolicyResult(text=context.text, violations=[], edits=[])
        updated = result
        if updated == context.text:
            return PolicyResult(text=context.text, violations=[], edits=[])

        edits = self._line_edits(before=context.text, after=updated)
        violations = [
            Violation(
                policy=self.name,
                message="clang-format adjusted formatting",
                line=1,
                column=1,
            )
        ]
        return PolicyResult(text=updated, violations=violations, edits=edits)

    def _run(
        self,
        *,
        command: str,
        style: str,
        assume_filename: str,
        extra_args: tuple[str, ...],
        timeout_seconds: float,
        text: str,
    ) -> str | None:
        cmd: list[str] = [command, f"-style={style}", f"-assume-filename={assume_filename}"]
        cmd.extend(extra_args)
        try:
            completed = subprocess.run(
                cmd,
                input=text,
                text=True,
                capture_output=True,
                check=False,
                timeout=timeout_seconds,
            )
        except FileNotFoundError:
            warn_once(
                "clang_format_missing",
                "clang_format: binary not found; install clang-format or disable policy",
            )
            return None
        except Exception as exc:
            warn_once(
                "clang_format_failed",
                f"clang_format: execution failed: {exc}",
            )
            return None

        if completed.returncode != 0:
            message = completed.stderr.strip() or completed.stdout.strip() or f"exit {completed.returncode}"
            warn_once(
                "clang_format_nonzero",
                f"clang_format: execution failed: {message}",
            )
            return None
        return completed.stdout

    def _resolve_command(self, configured: str) -> str | None:
        return shutil.which(configured)

    def _line_edits(self, before: str, after: str) -> list[Edit]:
        edits: list[Edit] = []
        before_lines = before.splitlines(keepends=True)
        after_lines = after.splitlines(keepends=True)
        shared = min(len(before_lines), len(after_lines))
        for idx in range(shared):
            if before_lines[idx] == after_lines[idx]:
                continue
            edits.append(
                Edit(
                    policy=self.name,
                    line=idx + 1,
                    before=before_lines[idx].rstrip("\r\n"),
                    after=after_lines[idx].rstrip("\r\n"),
                )
            )
        if len(before_lines) == len(after_lines):
            return edits
        tail_before = before_lines[shared:]
        tail_after = after_lines[shared:]
        tail_count = max(len(tail_before), len(tail_after))
        for offset in range(tail_count):
            before_line = tail_before[offset] if offset < len(tail_before) else ""
            after_line = tail_after[offset] if offset < len(tail_after) else ""
            edits.append(
                Edit(
                    policy=self.name,
                    line=shared + offset + 1,
                    before=before_line.rstrip("\r\n"),
                    after=after_line.rstrip("\r\n"),
                )
            )
        return edits

    def _required_str(self, key: str) -> str:
        value = self._config.get(key)
        if value is None:
            raise ValueError(f"clang_format: missing required config key '{key}'")
        text = str(value).strip()
        if not text:
            raise ValueError(f"clang_format: empty required config key '{key}'")
        return text

    def _required_float(self, key: str) -> float:
        value = self._config.get(key)
        if value is None:
            raise ValueError(f"clang_format: missing required config key '{key}'")
        try:
            parsed = float(value)
        except Exception as exc:
            raise ValueError(f"clang_format: invalid float for '{key}': {value!r}") from exc
        if parsed <= 0.0:
            raise ValueError(f"clang_format: '{key}' must be > 0")
        return parsed

    def _required_str_tuple(self, key: str) -> tuple[str, ...]:
        value = self._config.get(key)
        if not isinstance(value, (list, tuple)):
            raise ValueError(f"clang_format: missing required list config key '{key}'")
        return tuple(str(item) for item in value)
