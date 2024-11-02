use std::collections::HashMap;

use crate::cli::Paths;
use crate::config::get_generated_config_file_path;
use crate::config::BoolOrString;
use crate::config::GitLabCustomExecutorConfig;
use crate::config::GitLabLaunchConfig;
use crate::config::GitLabRunnerInstance;
use crate::config::GitLabRunnersConfig;
use crate::gitlab_config::CustomExecutor;
use crate::gitlab_config::Executor;
use crate::gitlab_config::Runner;
use anyhow::anyhow;
use anyhow::Context;

fn string_expand_impl<'a, F: Fn(&str) -> Option<&'a str>>(
    string: &str,
    instance_name: &str,
    instance: &GitLabRunnerInstance,
    additional_vars: &'a F,
) -> anyhow::Result<String> {
    let current_exe = std::env::current_exe()?;
    let current_exe_str = current_exe.to_str().ok_or(anyhow!(
        "Application binary path {:?} can't be converted to string",
        current_exe
    ))?;
    let home_dir = dirs::home_dir()
        .ok_or(anyhow!("Can't determine home directory"))?
        .to_str()
        .ok_or(anyhow!("Home directory path can't be converted to string"))?
        .to_owned();
    let env_vars: HashMap<_, String> = std::env::vars().collect();
    shellexpand::full_with_context(
        string,
        || Some(&home_dir),
        |v| {
            match v {
                // special case: NAME expands to the runner name
                "NAME" => Ok(Some(instance_name)),
                // special case: THIS expands to the binary path of this application
                "THIS" => Ok(Some(current_exe_str)),
                v => {
                    if let Some(s) = additional_vars(v) {
                        return Ok(Some(s));
                    }
                    let variable = instance.config_variables.get(v);
                    let env_variable = env_vars.get(v);
                    match (variable, env_variable) {
                        // Local variables take precedence over environment variables
                        (Some(v), _) => Ok(Some(v)),
                        (None, Some(v)) => Ok(Some(v)),
                        _ => Err(anyhow!("Undefined variable")),
                    }
                }
            }
        },
    )
    .map_err(|v| anyhow!(v))
    .map(|v| v.to_string())
}

pub fn expand_runner_config_template(
    config: &Runner,
    instance_name: &str,
    instance: &GitLabRunnerInstance,
) -> anyhow::Result<Runner> {
    let string_expand = |s: &str| string_expand_impl(s, instance_name, instance, &|_| None);
    let string_array_expand = |v: &Vec<String>| -> anyhow::Result<Vec<String>> {
        v.into_iter().map(|s| string_expand(s)).collect()
    };
    Ok(Runner {
        builds_dir: string_expand(&config.builds_dir).context("builds_dir")?,
        cache_dir: string_expand(&config.cache_dir).context("cache_dir")?,
        environment: config
            .environment
            .as_ref()
            .map(|m| m.iter().map(|v| string_expand(v)).collect())
            .transpose()?,
        executor: match &config.executor {
            Executor::Custom {
                custom:
                    CustomExecutor {
                        config_exec,
                        config_args,
                        prepare_exec,
                        prepare_args,
                        run_exec,
                        run_args,
                        cleanup_exec,
                        cleanup_args,
                    },
            } => Executor::Custom {
                custom: CustomExecutor {
                    config_exec: string_expand(config_exec).context("config_exec")?,
                    config_args: string_array_expand(&config_args).context("config_args")?,
                    prepare_exec: string_expand(prepare_exec).context("prepare_exec")?,
                    prepare_args: string_array_expand(&prepare_args).context("prepare_args")?,
                    run_exec: string_expand(run_exec).context("run_exec")?,
                    run_args: string_array_expand(&run_args).context("run_args")?,
                    cleanup_exec: string_expand(cleanup_exec).context("cleanup_exec")?,
                    cleanup_args: string_array_expand(&cleanup_args).context("cleanup_args")?,
                },
            },
            Executor::Shell => Executor::Shell,
        },
    })
}

pub fn expand_executor_config_template(
    config: &GitLabRunnersConfig,
    instance_name: &str,
    instance: &GitLabRunnerInstance,
) -> anyhow::Result<GitLabCustomExecutorConfig> {
    let executor = config
        .executor
        .as_ref()
        .ok_or(anyhow!("Missing custom executor configuration"))?;
    let string_expand = |s: &str| string_expand_impl(s, instance_name, instance, &|_| None);
    let expand_to_bool = |v: &BoolOrString| match v {
        BoolOrString::Bool(b) => Ok(*b),
        BoolOrString::String(s) => match string_expand(s)?.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            s => Err(anyhow!("Expected true or false, got '{}'", s)),
        },
    };
    Ok(GitLabCustomExecutorConfig {
        image_dir: string_expand(&executor.image_dir)
            .context("image_dir")?
            .into(),
        image_cache_dir: executor
            .image_cache_dir
            .as_ref()
            // expand string if possible, turn it into PathBuf if successful
            .map(|v| string_expand(&v).map(|s| s.into()))
            .transpose()
            .context("image_cache_dir")?,
        image_tmp_dir: executor
            .image_tmp_dir
            .as_ref()
            // expand string if possible, turn it into PathBuf if successful
            .map(|v| string_expand(&v).map(|s| s.into()))
            .transpose()
            .context("image_tmp_dir")?,
        pull_policy: executor.pull_policy,
        apptainer_executable: executor.apptainer_executable.clone(),
        gpu_amd: expand_to_bool(&executor.gpu_amd).context("gpu_amd")?,
        gpu_nvidia: expand_to_bool(&executor.gpu_nvidia).context("gpu_nvidia")?,
        mount: executor
            .mount
            .iter()
            .map(|v| string_expand(v))
            .collect::<anyhow::Result<Vec<_>>>()?,
        builds_dir: string_expand(
            executor
                .builds_dir
                .as_ref()
                .unwrap_or(&config.runner.builds_dir),
        )
        .context("builds_dir")?
        .into(),
        cache_dir: string_expand(&config.runner.cache_dir)
            .context("cache_dir")?
            .into(),
    })
}

pub fn expand_launch_config_template(
    paths: &Paths,
    config: &GitLabRunnersConfig,
    instance_name: &str,
    instance: &GitLabRunnerInstance,
    num_jobs: usize,
) -> anyhow::Result<GitLabLaunchConfig> {
    let launch = config
        .launch
        .as_ref()
        .ok_or(anyhow!("Missing launch configuration"))?;
    let generated_config_file_path = get_generated_config_file_path(paths, &config.name);
    let generated_config_file_path_str = generated_config_file_path.to_str().ok_or(anyhow!(
        "Generated config file path {:?} can't be converted to string",
        generated_config_file_path
    ))?;
    let num_jobs_str = format!("{}", num_jobs);
    let string_expand = |s: &str| {
        string_expand_impl(s, instance_name, instance, &|s| match s {
            "CONFIG" => Some(&generated_config_file_path_str),
            "NUM_JOBS" => Some(&num_jobs_str),
            _ => None,
        })
    };
    let optional_string_expand =
        |o: &Option<String>| o.as_ref().map(|s| string_expand(s)).transpose();
    let string_array_expand = |v: &Vec<String>| -> anyhow::Result<Vec<String>> {
        v.into_iter().map(|s| string_expand(s)).collect()
    };
    Ok(GitLabLaunchConfig {
        executable: launch.executable.clone(),
        args: string_array_expand(&launch.args).context("args")?,
        workdir: optional_string_expand(&launch.workdir).context("workdir")?,
        stdin: optional_string_expand(&launch.stdin).context("stdin")?,
        timeout: launch.timeout,
        group_size: launch.group_size,
    })
}
