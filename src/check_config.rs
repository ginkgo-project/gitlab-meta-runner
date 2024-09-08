use anyhow::Context;
use colored::Colorize;
use log::info;

use crate::{
    cli,
    config::read_config,
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
