#[derive(Clone, Debug, Default)]
pub struct RunSummary {
    pub files_processed: usize,
    pub files_changed: usize,
    pub violations: usize,
    pub errors: usize,
    pub warnings: usize,
}

impl RunSummary {
    pub fn merge_file(
        &mut self,
        changed: bool,
        violation_count: usize,
        has_error: bool,
        warning_count: usize,
    ) {
        self.files_processed += 1;
        if changed {
            self.files_changed += 1;
        }
        self.violations += violation_count;
        if has_error {
            self.errors += 1;
        }
        self.warnings += warning_count;
    }
}
