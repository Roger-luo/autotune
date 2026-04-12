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
    /// Initialize experiment (run sanity tests and baseline benchmarks)
    Init {
        /// Override the experiment name from config
        #[arg(long)]
        name: Option<String>,
    },
    /// Run planning phase for a single iteration
    Plan {
        /// Experiment name
        #[arg(long)]
        experiment: String,
    },
    /// Run implementation phase for a single iteration
    Implement {
        /// Experiment name
        #[arg(long)]
        experiment: String,
    },
    /// Run test phase for a single iteration
    Test {
        /// Experiment name
        #[arg(long)]
        experiment: String,
    },
    /// Run benchmark phase for a single iteration
    Benchmark {
        /// Experiment name
        #[arg(long)]
        experiment: String,
    },
    /// Score and record iteration results
    Record {
        /// Experiment name
        #[arg(long)]
        experiment: String,
    },
    /// Apply best result (cherry-pick onto canonical branch)
    Apply {
        /// Experiment name
        #[arg(long)]
        experiment: String,
    },
    /// Manage global user config
    #[command(subcommand)]
    Config(ConfigCommands),
    /// Export experiment data to a JSON file
    Export {
        /// Experiment name
        #[arg(long)]
        experiment: String,
        /// Output file path
        #[arg(long)]
        output: String,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Get a config value
    Get {
        /// Dotted key path (e.g. agent.init.model)
        key: String,
    },
    /// Set a config value
    Set {
        /// Dotted key path (e.g. agent.init.model)
        key: String,
        /// Value to set
        value: String,
    },
    /// Remove a config value
    Unset {
        /// Dotted key path (e.g. agent.init.model)
        key: String,
    },
    /// Show all config (merged system + user)
    List,
    /// Open user config in $EDITOR
    Edit,
}

#[derive(Clone, ValueEnum)]
pub enum ReportFormat {
    Json,
    Table,
}
