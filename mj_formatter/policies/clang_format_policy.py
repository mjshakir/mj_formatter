from __future__ import annotations

from dataclasses import dataclass
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

    @dataclass(frozen=True)
    class RunArgs:
        command: str
        style: str
        assume_filename: str
        extra_args: tuple[str, ...]
        timeout_seconds: float
        text: str

    def apply(self, context: ParseContext) -> PolicyResult:
        command = str(self._config.get("command", "clang-format")).strip() or "clang-format"
        resolved_command = self._resolve_command(command)
        if not resolved_command:
            warn_once(
                "clang_format_missing",
                "clang_format: binary not found; install clang-format or disable policy",
            )
            return PolicyResult(text=context.text, violations=[], edits=[])
        style = str(self._config.get("style", "file")).strip() or "file"
        timeout_seconds = float(self._config.get("timeout_seconds", 10.0))
        extra_args = tuple(str(item) for item in (self._config.get("extra_args", []) or []))
        result = self._run(
            ClangFormatPolicy.RunArgs(
                command=resolved_command,
                style=style,
                assume_filename=context.path,
                extra_args=extra_args,
                timeout_seconds=max(0.1, timeout_seconds),
                text=context.text,
            )
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

    def _run(self, args: "ClangFormatPolicy.RunArgs") -> str | None:
        cmd: list[str] = [args.command, f"-style={args.style}", f"-assume-filename={args.assume_filename}"]
        cmd.extend(args.extra_args)
        try:
            completed = subprocess.run(
                cmd,
                input=args.text,
                text=True,
                capture_output=True,
                check=False,
                timeout=args.timeout_seconds,
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
        direct = shutil.which(configured)
        if direct:
            return direct
        candidates = [
            "clang-format",
            "clang-format-19",
            "clang-format-18",
            "clang-format-17",
            "clang-format-16",
            "clang-format-15",
            "clang-format-14",
        ]
        seen = set()
        for item in [configured, *candidates]:
            if not item or item in seen:
                continue
            seen.add(item)
            resolved = shutil.which(item)
            if resolved:
                return resolved
        return None

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
