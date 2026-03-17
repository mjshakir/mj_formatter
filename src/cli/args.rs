use clap::Parser;

#[derive(Clone, Debug, Parser)]
#[command(name = "formatter")]
#[command(about = "C/C++ formatter pipeline")]
pub struct CliArgs {
    #[arg(long)]
    pub config: Option<String>,

    #[arg(long)]
    pub style: Option<String>,

    #[arg(long)]
    pub root: Option<String>,

    #[arg(long = "include")]
    pub include: Vec<String>,

    #[arg(long = "exclude")]
    pub exclude: Vec<String>,

    #[arg(long = "enable")]
    pub enable: Vec<String>,

    #[arg(long = "disable")]
    pub disable: Vec<String>,

    #[arg(long)]
    pub jobs: Option<usize>,

    /// Number of worker processes. Use "max" to auto-detect all CPU cores.
    #[arg(long)]
    pub processes: Option<String>,

    /// Threads per worker process. Overrides --jobs when specified (total = processes × N).
    #[arg(long = "threads-per-process")]
    pub threads_per_process: Option<usize>,

    #[arg(long)]
    pub check: bool,

    /// Restore all files modified by the most recent formatter run from their backups.
    /// Requires that backups were enabled (default: on). Use --undo-run <RUN_ID> to
    /// restore a specific run instead of the most recent one.
    #[arg(long)]
    pub undo: bool,

    /// Restore files from a specific backup run ID (timestamp string).
    /// If omitted with --undo, the most recent run is used.
    #[arg(long = "undo-run")]
    pub undo_run: Option<String>,

    #[arg(long)]
    pub verbose: bool,

    #[arg(long = "list-policies")]
    pub list_policies: bool,

    /// Worker process timeout in seconds (overrides config.toml).
    #[arg(long = "worker-timeout")]
    pub worker_timeout: Option<u64>,

    #[arg(long, hide = true)]
    pub benchmark_only: bool,

    #[arg(long, hide = true)]
    pub clang_parse_helper: bool,

    #[arg(long, hide = true)]
    pub worker_pool: bool,

    #[arg(long, hide = true)]
    pub worker_manifest: Option<String>,

    #[arg(long, hide = true)]
    pub worker_result: Option<String>,

    #[arg(long, hide = true)]
    pub reporter: bool,

    #[arg(long = "reporter-path", hide = true)]
    pub reporter_path: Option<String>,
}
