use clap::Parser;

/// Tool to check configuration validity
mod check_config;
/// All CLI arguments
mod cli;
/// All config structs that are not directly written to gitlab-runner config files
mod config;
/// Implementation of runner registration and instantiated config file generation
mod configure;
/// Implementation of a custom executor
mod executor;
/// All config structs that will be used to write gitlab-runner config files
mod gitlab_config;
/// All functions related to the GitLab API
mod gitlab_wrap;
/// Implementation of the meta-runner for dispatching gitlab-runner run-single tasks
mod run;
/// All functions related to template instantiation/variable expansion
mod template;

fn main() -> anyhow::Result<()> {
    let cli = cli::CliOptions::parse();
    simple_logger::SimpleLogger::new()
        .with_level(cli.verbose.log_level_filter())
        .init()
        .unwrap();
    match cli.command {
        cli::Command::CreateExampleConfig => config::write_default_config(&cli.paths.config_file),
        cli::Command::ShowExampleConfig => Ok(println!("{}", config::get_default_config_str())),
        cli::Command::CheckConfig => check_config::check(&cli.paths),
        cli::Command::ShowConfig => check_config::show(&cli.paths),
        cli::Command::Configure => configure::configure(&cli.paths),
        cli::Command::Executor(options) => executor::exec(&cli.paths, &options),
        cli::Command::RunSingle => run::run_single(&cli.paths),
        cli::Command::Run => run::run(cli.paths),
    }
}
