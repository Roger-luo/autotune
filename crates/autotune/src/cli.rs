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
    /// Start a fresh task
    Run {
        /// Override the task name from config
        #[arg(long)]
        task: Option<String>,
    },
    /// Resume an existing task
    Resume {
        /// Task name to resume
        #[arg(long)]
        task: String,
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
    /// Show task progress
    Report {
        /// Task name
        #[arg(long)]
        task: Option<String>,
        /// Output format
        #[arg(long, default_value = "table")]
        format: ReportFormat,
    },
    /// List all tasks
    List,
    /// Initialize task (run sanity tests and baseline measures)
    Init {
        /// Override the task name from config
        #[arg(long)]
        name: Option<String>,
    },
    /// Run planning phase for a single iteration
    Plan {
        /// Task name
        #[arg(long)]
        task: String,
    },
    /// Run implementation phase for a single iteration
    Implement {
        /// Task name
        #[arg(long)]
        task: String,
    },
    /// Run test phase for a single iteration
    Test {
        /// Task name
        #[arg(long)]
        task: String,
    },
    /// Run measurement phase for a single iteration
    Measure {
        /// Task name
        #[arg(long)]
        task: String,
    },
    /// Score and record iteration results
    Record {
        /// Task name
        #[arg(long)]
        task: String,
    },
    /// Apply best result (cherry-pick onto canonical branch)
    Apply {
        /// Task name
        #[arg(long)]
        task: String,
    },
    /// Fast-forward canonical branch to the advancing branch, then clean up
    Ff {
        /// Task name (defaults to the task name in .autotune.toml)
        #[arg(long)]
        task: Option<String>,
    },
    /// Manage global user config
    #[command(subcommand)]
    Config(ConfigCommands),
    /// Export task data to a JSON file
    Export {
        /// Task name
        #[arg(long)]
        task: String,
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
