use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "autotune",
    about = "Automated performance tuning via AI agents"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start a fresh experiment
    Run {
        /// Override the experiment name from config
        #[arg(long)]
        experiment: Option<String>,
    },
    /// Resume an existing experiment
    Resume {
        /// Experiment name to resume
        #[arg(long)]
        experiment: String,
        /// Override max iterations stop condition
        #[arg(long)]
        max_iterations: Option<u64>,
        /// Override max duration stop condition (e.g. "1h", "30m")
        #[arg(long)]
        max_duration: Option<String>,
        /// Override target improvement stop condition
        #[arg(long)]
        target_improvement: Option<f64>,
    },
    /// Show experiment progress
    Report {
        /// Experiment name
        #[arg(long)]
        experiment: Option<String>,
        /// Output format
        #[arg(long, default_value = "table")]
        format: ReportFormat,
    },
    /// List all experiments
    List,
    /// Initialize experiment (stub)
    Init,
    /// Run planning phase (stub)
    Plan,
    /// Run implementation phase (stub)
    Implement,
    /// Run test phase (stub)
    Test,
    /// Run benchmark phase (stub)
    Benchmark,
    /// Record iteration (stub)
    Record,
    /// Apply best result (stub)
    Apply,
    /// Export experiment data (stub)
    Export,
}

#[derive(Clone, ValueEnum)]
pub enum ReportFormat {
    Json,
    Table,
}
