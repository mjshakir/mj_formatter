use rustc_hash::{FxHashMap, FxHashSet};

use crate::model::edit::Edit;
use crate::model::violation::Violation;

pub struct PolicyConflictDetector {
    enabled: bool,
    touch_threshold: usize,
    line_history: FxHashMap<usize, Vec<Edit>>,
    reported_reverts: FxHashSet<(usize, String, String)>,
    reported_touches: FxHashSet<(usize, String)>,
}

impl PolicyConflictDetector {
    pub fn new(enabled: bool, touch_threshold: usize) -> Self {
        Self {
            enabled,
            touch_threshold: touch_threshold.max(2),
            line_history: FxHashMap::default(),
            reported_reverts: FxHashSet::default(),
            reported_touches: FxHashSet::default(),
        }
    }

    pub fn observe(&mut self, policy_name: &str, edits: &[Edit]) -> Vec<Violation> {
        if !self.enabled || edits.is_empty() {
            return Vec::new();
        }

        let mut violations = Vec::<Violation>::new();
        for edit in edits {
            if edit.line == 0 {
                continue;
            }

            let history = self.line_history.entry(edit.line).or_default();
            if let Some(previous) = history.last() {
                if previous.policy != policy_name
                    && edit.after == previous.before
                    && edit.before == previous.after
                {
                    let key = (
                        edit.line,
                        previous.policy.to_string(),
                        policy_name.to_string(),
                    );
                    if self.reported_reverts.insert(key.clone()) {
                        violations.push(Violation {
                            policy: "policy_conflict_detector".into(),
                            message: format!(
                                "Line {}: policy '{}' appears to revert policy '{}'",
                                edit.line, policy_name, previous.policy
                            ),
                            line: edit.line,
                            column: Some(1),
                        });
                    }
                }

                let mut touched = history
                    .iter()
                    .map(|item| item.policy.as_str())
                    .collect::<Vec<_>>();
                touched.push(policy_name);
                touched.sort_unstable();
                touched.dedup();

                if touched.len() >= self.touch_threshold {
                    let touched_key = touched.join("|");
                    let key = (edit.line, touched_key.clone());
                    if self.reported_touches.insert(key) {
                        violations.push(Violation {
                            policy: "policy_conflict_detector".into(),
                            message: format!(
                                "Line {}: touched by multiple policies ({}), verify style rule compatibility",
                                edit.line,
                                touched.join(", ")
                            ),
                            line: edit.line,
                            column: Some(1),
                        });
                    }
                }
            }

            history.push(edit.clone());
        }

        violations
    }
}

#[cfg(test)]
mod tests {
    use crate::model::edit::Edit;

    use super::PolicyConflictDetector;

    #[test]
    fn reports_revert_conflict() {
        let mut detector = PolicyConflictDetector::new(true, 3);
        let first = Edit {
            policy: "p1".into(),
            line: 10,
            before: "A".to_string(),
            after: "B".to_string(),
        };
        let second = Edit {
            policy: "p2".into(),
            line: 10,
            before: "B".to_string(),
            after: "A".to_string(),
        };
        assert!(detector.observe("p1", &[first]).is_empty());
        let violations = detector.observe("p2", &[second]);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].message.contains("appears to revert"));
    }
}
