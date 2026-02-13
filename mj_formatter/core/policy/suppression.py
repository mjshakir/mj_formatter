from __future__ import annotations

import re


class PolicySuppression:
    _directive_re = re.compile(r"//\s*mjf:(disable|enable|ignore)\s+([A-Za-z0-9_,*\- ]+)")

    def disabled_lines(self, text: str, policy_name: str) -> set[int]:
        policy = policy_name.strip().lower()
        disabled: set[int] = set()
        enabled_state = False
        lines = text.splitlines()
        for index, line in enumerate(lines, start=1):
            directives = list(self._directive_re.finditer(line))
            if enabled_state:
                disabled.add(index)

            for directive in directives:
                action = directive.group(1).strip().lower()
                targets = {
                    token.strip().lower()
                    for token in directive.group(2).split(",")
                    if token.strip()
                }
                if not targets:
                    continue
                if policy not in targets and "*" not in targets:
                    continue
                if action == "ignore":
                    disabled.add(index)
                    continue
                if action == "disable":
                    enabled_state = True
                    disabled.add(index)
                    continue
                if action == "enable":
                    enabled_state = False
        return disabled

