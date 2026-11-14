use anyhow::{anyhow, Context};
use colored::Colorize;
use log::info;

use crate::{
    cli,
    config::{read_config, GitLabRunnersConfig},
    template::{
        expand_executor_config_template, expand_launch_config_template,
        expand_runner_config_template,
    },
};

pub fn check(paths: &cli::Paths) -> anyhow::Result<()> {
    let config = read_config(&paths.config_file).context(format!(
        "Failed reading config file {:?}",
        paths.config_file
    ))?;
    validate(&config)?;
    let num_jobs = config.launch.as_ref().map_or(1, |v| v.group_size);
    for (instance_name, instance) in &config.runners {
        expand_runner_config_template(&config.runner, instance_name, instance).context(format!(
            "Failed expanding [runner] for instance {}",
            instance_name
        ))?;
        expand_executor_config_template(&config, instance_name, instance).context(format!(
            "Failed expanding [executor] for instance {}",
            instance_name
        ))?;
        expand_launch_config_template(paths, &config, instance_name, instance, num_jobs).context(
            format!("Failed expanding [launch] for instance {}", instance_name),
        )?;
    }
    info!("Config check successful, no errors found");
    Ok(())
}

/// Checks consistency between config sections that template expansion can't detect
pub fn validate(config: &GitLabRunnersConfig) -> anyhow::Result<()> {
    let has_count = config
        .launch
        .as_ref()
        .is_some_and(|launch| launch.count.is_some());
    if !has_count {
        let mut limited: Vec<_> = config
            .runners
            .iter()
            .filter(|(_, instance)| instance.max_launches.is_some())
            .map(|(name, _)| name.as_str())
            .collect();
        if !limited.is_empty() {
            limited.sort();
            return Err(anyhow!(
                "Runners {} set max_launches, which requires a [launch.count] section",
                limited.join(", ")
            ));
        }
    }
    Ok(())
}

pub fn show(paths: &cli::Paths) -> anyhow::Result<()> {
    let config = read_config(&paths.config_file).context(format!(
        "Failed reading config file {:?}",
        paths.config_file
    ))?;
    info!("{}", "Full configuration".green());
    println!(
        "{}",
        toml::to_string_pretty(&config).context("Failed printing config")?
    );
    let num_jobs = config.launch.as_ref().map_or(1, |v| v.group_size);
    for (instance_name, instance) in &config.runners {
        println!(
            "{}",
            format!("gitlab-runner configuration for runner {}", instance_name).green()
        );
        println!(
            "{}",
            toml::to_string_pretty(
                &expand_runner_config_template(&config.runner, instance_name, instance).context(
                    format!("Failed expanding [runner] for instance {}", instance_name)
                )?
            )
            .context("Failed printing config")?
        );
        println!(
            "{}",
            format!("executor configuration for runner {}", instance_name).green()
        );
        println!(
            "{}",
            toml::to_string_pretty(
                &expand_executor_config_template(&config, instance_name, instance).context(
                    format!("Failed expanding [executor] for instance {}", instance_name)
                )?
            )
            .context("Failed printing config")?
        );
        println!(
            "{}",
            format!("launch configuration for runner {}", instance_name).green()
        );
        println!(
            "{}",
            toml::to_string_pretty(
                &expand_launch_config_template(paths, &config, instance_name, instance, num_jobs)
                    .context(format!(
                    "Failed expanding [launch] for instance {}",
                    instance_name
                ),)?
            )
            .context("Failed printing config")?
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::get_example_config;

    #[test]
    fn validate_rejects_limit_without_count() {
        let mut config = get_example_config();
        config.launch.as_mut().unwrap().count = None;
        for runner in config.runners.values_mut() {
            runner.max_launches = Some(2);
        }
        let result = validate(&config);
        assert!(result.is_err());
        assert!(
            format!("{:?}", result.unwrap_err()).contains("launch.count"),
            "error should point at the missing [launch.count] section"
        );
    }

    #[test]
    fn validate_accepts_limit_with_count() {
        let config = get_example_config();
        assert!(config.runners.values().any(|r| r.max_launches.is_some()));
        assert!(config.launch.as_ref().unwrap().count.is_some());
        let result = validate(&config);
        assert!(result.is_ok(), "{:?}", result);
    }

    #[test]
    fn validate_accepts_no_limits() {
        let mut config = get_example_config();
        config.launch.as_mut().unwrap().count = None;
        for runner in config.runners.values_mut() {
            runner.max_launches = None;
        }
        let result = validate(&config);
        assert!(result.is_ok(), "{:?}", result);
    }
}
