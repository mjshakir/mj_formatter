use crate::engine::fuzzy_inference;
use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::parser::ts_traversal;
use crate::policy::traits::Policy;
use crate::text_scan;

pub struct ClangFormatPolicy {
    command: String,
    style: String,
}

impl ClangFormatPolicy {
    pub fn new(command: String, style: String) -> Self {
        Self { command, style }
    }

    fn diff_lines(&self, before: &str, after: &str) -> Vec<Edit> {
        let before_lines = Self::split_keep_ends(before);
        let after_lines = Self::split_keep_ends(after);
        let common_len = before_lines.len().min(after_lines.len());
        let mut prefix = 0usize;
        while prefix < common_len && before_lines[prefix] == after_lines[prefix] {
            prefix = prefix.saturating_add(1);
        }

        let mut before_tail = before_lines.len();
        let mut after_tail = after_lines.len();
        while before_tail > prefix
            && after_tail > prefix
            && before_lines[before_tail - 1] == after_lines[after_tail - 1]
        {
            before_tail = before_tail.saturating_sub(1);
            after_tail = after_tail.saturating_sub(1);
        }

        let before_diff = &before_lines[prefix..before_tail];
        let after_diff = &after_lines[prefix..after_tail];
        let max_lines = before_diff.len().max(after_diff.len());
        let mut edits = Vec::new();

        for idx in 0..max_lines {
            let left = before_diff.get(idx).map(String::as_str).unwrap_or("");
            let right = after_diff.get(idx).map(String::as_str).unwrap_or("");
            if left != right {
                edits.push(Edit {
                    policy: self.name().into(),
                    line: prefix + idx + 1,
                    before: left.to_string(),
                    after: right.to_string(),
                });
            }
        }

        edits
    }

    fn split_keep_ends(text: &str) -> Vec<String> {
        text_scan::split_lines_keepends(text, false)
    }

    fn normalize_line_for_match(line: &str) -> String {
        line.chars()
            .filter(|ch| !ch.is_ascii_whitespace())
            .collect::<String>()
    }

    fn is_delete_default_function_line(line: &str) -> bool {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('#') {
            return false;
        }
        let compact = Self::normalize_line_for_match(trimmed);
        (compact.contains(")=delete;") || compact.contains(")=default;")) && compact.contains('(')
    }

    fn is_end_comment_line(line: &str) -> bool {
        let trimmed = line.trim_start();
        trimmed.contains("// end ") && trimmed.contains('}')
    }

    fn self_validate_tree_sitter(
        &self,
        before: &str,
        after: &str,
        before_error_count: usize,
        edits: Vec<Edit>,
    ) -> (String, Vec<Edit>, Vec<String>) {
        let after_stats = match ts_traversal::quick_error_stats_cpp(after) {
            Some(stats) => stats,
            None => return (after.to_string(), edits, Vec::new()),
        };
        if after_stats.error_nodes <= before_error_count {
            return (after.to_string(), edits, Vec::new());
        }
        let new_error_lines = &after_stats.error_lines;
        let safe_edits: Vec<Edit> = edits
            .into_iter()
            .filter(|edit| !new_error_lines.contains(&edit.line))
            .collect();
        if safe_edits.is_empty() {
            return (
                before.to_string(),
                Vec::new(),
                vec![format!(
                    "clang_format: self-validation reverted all edits ({} new parse error(s), {} → {})",
                    after_stats.error_nodes.saturating_sub(before_error_count),
                    before_error_count,
                    after_stats.error_nodes,
                )],
            );
        }
        let before_lines = Self::split_keep_ends(before);
        let after_lines = Self::split_keep_ends(after);
        let mut result_lines = before_lines.clone();
        for edit in &safe_edits {
            let idx = edit.line.saturating_sub(1);
            if idx < result_lines.len() && idx < after_lines.len() {
                result_lines[idx] = after_lines[idx].clone();
            }
        }
        let rebuilt = result_lines.concat();
        let rebuilt_stats = ts_traversal::quick_error_stats_cpp(&rebuilt);
        if let Some(stats) = &rebuilt_stats {
            if stats.error_nodes > before_error_count {
                return (
                    before.to_string(),
                    Vec::new(),
                    vec![format!(
                        "clang_format: self-validation line-granular fallback still worse ({} → {}); reverted all",
                        before_error_count, stats.error_nodes,
                    )],
                );
            }
        }
        let warnings = vec![format!(
            "clang_format: self-validation recovered {} of {} edits ({} error lines excluded)",
            safe_edits.len(),
            safe_edits.len() + new_error_lines.len(),
            new_error_lines.len(),
        )];
        (rebuilt, safe_edits, warnings)
    }

    fn preserve_sensitive_lines(before: &str, after: &str) -> String {
        let before_lines = Self::split_keep_ends(before);
        let mut after_lines = Self::split_keep_ends(after);
        if before_lines.is_empty() || after_lines.is_empty() {
            return after.to_string();
        }

        let after_keys: Vec<String> = after_lines
            .iter()
            .map(|l| Self::normalize_line_for_match(l.as_str()))
            .collect();
        let mut consumed = vec![false; after_lines.len()];
        for source in &before_lines {
            if !Self::is_delete_default_function_line(source) && !Self::is_end_comment_line(source)
            {
                continue;
            }
            let key = Self::normalize_line_for_match(source);
            if key.is_empty() {
                continue;
            }
            for idx in 0..after_lines.len() {
                if consumed[idx] {
                    continue;
                }
                if after_keys[idx] != key {
                    if Self::is_end_comment_line(source) {
                        let trimmed = after_lines[idx].trim_start();
                        if trimmed.contains("} // end ") {
                            let mut merged = after_lines[idx].clone();
                            let mut tail_indexes = Vec::new();
                            let mut scan = idx + 1;
                            while scan < after_lines.len() {
                                let next_trimmed = after_lines[scan].trim_start();
                                if !next_trimmed.starts_with("//") {
                                    break;
                                }
                                let continuation = next_trimmed
                                    .strip_prefix("//")
                                    .map(str::trim_start)
                                    .unwrap_or(next_trimmed);
                                merged.push(' ');
                                merged.push_str(continuation);
                                tail_indexes.push(scan);
                                scan += 1;
                            }
                            if Self::normalize_line_for_match(merged.as_str()) == key {
                                after_lines[idx] = source.clone();
                                consumed[idx] = true;
                                for tail in tail_indexes {
                                    after_lines[tail].clear();
                                    consumed[tail] = true;
                                }
                                break;
                            }
                        }
                    }
                    continue;
                }
                after_lines[idx] = source.clone();
                consumed[idx] = true;
                break;
            }
        }
        after_lines.concat()
    }
}

impl ClangFormatPolicy {
    fn run_clang_format(
        &self,
        text: &str,
        filename: &str,
        region: Option<(usize, usize)>,
    ) -> Result<String, String> {
        let service = crate::policy::clang_format_service::ClangFormatService::global()
            .map_err(|e| format!("clang_format service unavailable: {e}"))?;
        let handle = service
            .dispatch(
                text.to_string(),
                self.command.clone(),
                self.style.clone(),
                filename.to_string(),
                region,
            )
            .map_err(|e| format!("clang_format dispatch failed: {e}"))?;
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(30);
        handle.collect_deadline(deadline)
    }

    fn compute_line_regions(total_lines: usize, batch_size: usize) -> Vec<(usize, usize)> {
        if batch_size >= total_lines {
            return vec![(1, total_lines)];
        }
        let mut regions = Vec::new();
        let mut start = 1usize;
        while start <= total_lines {
            let end = (start + batch_size - 1).min(total_lines);
            regions.push((start, end));
            start = end + 1;
        }
        regions
    }
}

impl Policy for ClangFormatPolicy {
    fn name(&self) -> &str {
        "clang_format"
    }
    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        if let Some(parse) = context.clang_parse_result {
            let summary = parse.diagnostic_summary();
            if summary.fatal > 0 {
                return PolicyResult {
                    text: context.text.to_string(),
                    violations: Vec::new(),
                    edits: Vec::new(),
                    warnings: vec![format!(
                        "clang_format: skipped due fatal clang diagnostics (fatal={})",
                        summary.fatal
                    )],
                };
            }
        }
        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: vec![
                    "clang_format: semantic context unavailable; skipping heuristic formatting"
                        .to_string(),
                ],
            };
        }

        let before_error_count = context
            .tree_sitter_tree
            .map(|t| ts_traversal::tree_error_stats(t).error_nodes)
            .unwrap_or(0);

        let line_count = context.text.lines().count();
        let batch_size = context.forced_batch_size.unwrap_or_else(|| {
            context
                .policy_certainty
                .as_ref()
                .map(|c| fuzzy_inference::fuzzy_region_batch_lines(c, before_error_count))
                .unwrap_or(usize::MAX)
        });
        let regions = Self::compute_line_regions(line_count, batch_size);

        if regions.len() <= 1 {
            // Fast path: whole-file formatting (original behavior)
            let formatted = match self.run_clang_format(context.text, context.path_str(), None) {
                Ok(f) => f,
                Err(warning) => {
                    return PolicyResult {
                        text: context.text.to_string(),
                        violations: Vec::new(),
                        edits: Vec::new(),
                        warnings: vec![warning],
                    };
                }
            };

            let updated = Self::preserve_sensitive_lines(context.text, formatted.as_str());
            if updated == context.text {
                return PolicyResult {
                    text: context.text.to_string(),
                    violations: Vec::new(),
                    edits: Vec::new(),
                    warnings: Vec::new(),
                };
            }

            let edits = self.diff_lines(context.text, &updated);
            let (validated_text, validated_edits, warnings) =
                self.self_validate_tree_sitter(context.text, &updated, before_error_count, edits);
            if validated_edits.is_empty() {
                return PolicyResult {
                    text: context.text.to_string(),
                    violations: Vec::new(),
                    edits: Vec::new(),
                    warnings,
                };
            }
            PolicyResult {
                text: validated_text,
                violations: vec![Violation {
                    policy: self.name().into(),
                    message: "clang-format adjusted formatting".to_string(),
                    line: validated_edits.first().map(|item| item.line).unwrap_or(1),
                    column: Some(1),
                }],
                edits: validated_edits,
                warnings,
            }
        } else {
            // Region path: format each region via --lines, self-validate per region
            let mut current_text = context.text.to_string();
            let mut all_edits = Vec::new();
            let mut all_warnings = Vec::new();
            let filename = context.path_str();

            for (start, end) in &regions {
                let formatted = match self.run_clang_format(&current_text, filename, Some((*start, *end))) {
                    Ok(f) => f,
                    Err(warning) => {
                        all_warnings.push(warning);
                        continue;
                    }
                };

                let updated = Self::preserve_sensitive_lines(&current_text, formatted.as_str());
                if updated == current_text {
                    continue;
                }

                let region_edits = self.diff_lines(&current_text, &updated);
                let current_error_count = ts_traversal::quick_error_stats_cpp(&current_text)
                    .map(|s| s.error_nodes)
                    .unwrap_or(before_error_count);
                let (validated_text, validated_edits, warnings) =
                    self.self_validate_tree_sitter(&current_text, &updated, current_error_count, region_edits);

                if !validated_edits.is_empty() {
                    current_text = validated_text;
                    all_edits.extend(validated_edits);
                }
                all_warnings.extend(warnings);
            }

            if all_edits.is_empty() {
                return PolicyResult {
                    text: context.text.to_string(),
                    violations: Vec::new(),
                    edits: Vec::new(),
                    warnings: all_warnings,
                };
            }
            PolicyResult {
                text: current_text,
                violations: vec![Violation {
                    policy: self.name().into(),
                    message: "clang-format adjusted formatting".to_string(),
                    line: all_edits.first().map(|item| item.line).unwrap_or(1),
                    column: Some(1),
                }],
                edits: all_edits,
                warnings: all_warnings,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::ClangFormatPolicy;
    use crate::model::policy_context::PolicyContext;
    use crate::parser::clang_result::{
        ClangDiagnosticEntry, ClangDiagnosticSeverity, ClangDiagnosticSummary, ClangParseResult,
    };
    use crate::policy::traits::Policy;

    #[test]
    fn preserve_sensitive_lines_restores_delete_default_spacing() {
        let before = "    Hasher(void)                    = delete;\n    ~Hasher(void)                   = delete;\n";
        let after = "    Hasher(void) = delete;\n    ~Hasher(void) = delete;\n";
        let restored = ClangFormatPolicy::preserve_sensitive_lines(before, after);
        assert_eq!(restored, before);
    }

    #[test]
    fn preserve_sensitive_lines_restores_end_comment_line() {
        let before = "            } // end bool swap(const Key& key, std::shared_ptr<T> old_data, std::shared_ptr<T> new_data)\n";
        let after = "            } // end bool swap(const Key& key, std::shared_ptr<T> old_data, std::shared_ptr<T>\n              // new_data)\n";
        let restored = ClangFormatPolicy::preserve_sensitive_lines(before, after);
        assert_eq!(restored, before);
    }

    #[test]
    fn diff_lines_anchors_localized_insertions() {
        let policy = ClangFormatPolicy::new("clang-format".to_string(), "LLVM".to_string());
        let before = "#pragma once\n#include \"A.hpp\"\n";
        let after = "#pragma once\n\n#include \"A.hpp\"\n";
        let edits = policy.diff_lines(before, after);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].line, 2);
    }

    #[test]
    fn skips_when_clang_has_fatal_diagnostics() {
        let policy = ClangFormatPolicy::new("clang-format".to_string(), "LLVM".to_string());
        let parse = ClangParseResult::new(
            false,
            vec!["fatal".to_string()],
            Vec::new(),
            ClangDiagnosticSummary {
                fatal: 1,
                ..ClangDiagnosticSummary::default()
            },
            vec![ClangDiagnosticEntry {
                line: 1,
                column: 1,
                severity: ClangDiagnosticSeverity::Fatal,
            }],
        );
        let text = "int x=1;\n";
        let path = PathBuf::from("fatal.cpp");
        let context = PolicyContext::new(text, &path).with_clang_parse_result(Some(&parse));
        let result = policy.apply(&context);
        assert_eq!(result.text, text);
        assert!(result.edits.is_empty());
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("skipped due fatal clang diagnostics")));
    }
}
