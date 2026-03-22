use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SuppressionAction {
    Disable,
    Enable,
    Ignore,
}

pub struct PolicySuppression;

impl PolicySuppression {
    pub fn disabled_lines(text: &str, policy_name: &str) -> BTreeSet<usize> {
        let policy = policy_name.trim().to_lowercase();
        let mut disabled = BTreeSet::<usize>::new();
        let mut enabled_state = false;

        for (index, line) in text.lines().enumerate() {
            let line_no = index + 1;
            if enabled_state {
                disabled.insert(line_no);
            }
            for (action, targets) in Self::directives_in_line(line) {
                if targets.is_empty() {
                    continue;
                }
                if !targets.iter().any(|item| item == "*" || item == &policy) {
                    continue;
                }
                match action {
                    SuppressionAction::Ignore => {
                        disabled.insert(line_no);
                    }
                    SuppressionAction::Disable => {
                        enabled_state = true;
                        disabled.insert(line_no);
                    }
                    SuppressionAction::Enable => {
                        enabled_state = false;
                    }
                }
            }
        }
        disabled
    }

    fn directives_in_line(line: &str) -> Vec<(SuppressionAction, Vec<String>)> {
        let mut directives = Vec::new();
        let mut cursor = 0usize;
        let marker = "mjf:";
        while let Some(pos) = line[cursor..].find(marker) {
            let start = cursor + pos + marker.len();
            let tail = &line[start..];
            let parsed = Self::parse_directive_tail(tail);
            if let Some(item) = parsed {
                directives.push(item);
            }
            cursor = start;
            if cursor >= line.len() {
                break;
            }
        }
        directives
    }

    fn parse_directive_tail(tail: &str) -> Option<(SuppressionAction, Vec<String>)> {
        let trimmed = tail.trim_start();
        if trimmed.is_empty() {
            return None;
        }
        let mut action_end = 0usize;
        for (idx, ch) in trimmed.char_indices() {
            if ch.is_ascii_alphabetic() {
                action_end = idx + ch.len_utf8();
            } else {
                break;
            }
        }
        if action_end == 0 {
            return None;
        }
        let action = match &trimmed[..action_end].to_ascii_lowercase()[..] {
            "disable" => SuppressionAction::Disable,
            "enable" => SuppressionAction::Enable,
            "ignore" => SuppressionAction::Ignore,
            _ => return None,
        };

        let targets_raw = trimmed[action_end..].trim();
        if targets_raw.is_empty() {
            return Some((action, Vec::new()));
        }
        let targets = targets_raw
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(|item| item.to_lowercase())
            .collect::<Vec<_>>();
        Some((action, targets))
    }
}

#[cfg(test)]
mod tests {
    use super::PolicySuppression;

    #[test]
    fn supports_disable_ranges() {
        let source = "\
// mjf:ignore naming_conventions
int BadName = 0;
// mjf:disable naming_conventions
int AlsoBad = 0;
// mjf:enable naming_conventions
int GoodName = 0;
";
        let lines = PolicySuppression::disabled_lines(source, "naming_conventions");
        assert!(lines.contains(&1));
        assert!(lines.contains(&3));
        assert!(lines.contains(&4));
        assert!(!lines.contains(&6));
    }
}
