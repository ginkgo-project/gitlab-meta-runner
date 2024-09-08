use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use clap_verbosity_flag::{InfoLevel, Verbosity};

use crate::config;

#[derive(Debug, Args)]
pub struct Paths {
    /// Configuration file for the meta-runner
    #[arg(long, default_value = config::get_default_config_file_path().into_os_string())]
    pub config_file: PathBuf,
    /// Directory used to store meta-runner data (registered runners, their tokens and generated gitlab-runner config files)
    /// The files in this directory will be prefixed by the meta-runner's name
    #[arg(long, default_value = config::get_default_data_dir().into_os_string(), verbatim_doc_comment)]
    pub data_dir: PathBuf,
    /// Path for the generated gitlab-runner configuration file.
    /// Only use this if you don't want to use the default location in `data_dir`
    #[arg(long, verbatim_doc_comment)]
    pub generated_config_file: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum ExecutorCommand {
    // Run the config step of the custom executor
    Config,
    // Run the prepare step of the custom executor
    Prepare,
    // Run the run step of the custom executor
    Run {
        /// The script to be executed
        script_name: PathBuf,
        /// The step to be executed in the script
        step_name: String,
    },
    // Run the cleanup step of the custom executor
    Cleanup,
}

#[derive(Debug, Args)]
pub struct ExecutorOptions {
    /// The name of the runner configuration to use
    pub runner_name: String,
    #[command(subcommand)]
    pub command: ExecutorCommand,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Creates an example configuration file
    CreateExampleConfig,
    /// Prints the example configuration
    ShowExampleConfig,
    /// Checks the configuration for validity
    CheckConfig,
    /// Show the configuration instantiated for each runner
    ShowConfig,
    /// Updates runner registrations and gitlab-runner config files
    Configure,
    /// Run the custom executor
    Executor(ExecutorOptions),
    /// Run the meta-runner a single time to dispatch runners for all currently pending jobs
    RunSingle,
    /// Run the meta-runner continuously to dispatch runners at regular intervals
    Run,
}

#[derive(Parser, Debug)]
pub struct CliOptions {
    #[command(subcommand)]
    pub command: Command,
    /// Config file paths
    #[command(flatten)]
    pub paths: Paths,
    #[command(flatten)]
    pub verbose: Verbosity<InfoLevel>,
}
