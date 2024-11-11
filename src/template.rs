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
use log::warn;

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
            .transpose()
            .context("environment")?,
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
        apptainer_executable: string_expand(&executor.apptainer_executable)
            .context("apptainer_executable")?
            .into(),
        gpu_amd: expand_to_bool(&executor.gpu_amd).context("gpu_amd")?,
        gpu_nvidia: expand_to_bool(&executor.gpu_nvidia).context("gpu_nvidia")?,
        mount: executor
            .mount
            .iter()
            .map(|v| string_expand(v))
            .collect::<anyhow::Result<Vec<_>>>()
            .context("mount")?,
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
        // This one needs to be infallible to handle check-config
        description: executor.description.as_ref().map(|v| {
            string_expand(v)
                .map_err(|e| warn!("Custom executor description could not be expanded\n(this is not necessarily an error if you use environment variables that are only available at runner execution in there): {:?}", e))
                .unwrap_or(v.clone())
        }),
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
        executable: string_expand(&launch.executable)
            .context("executable")?
            .into(),
        args: string_array_expand(&launch.args).context("args")?,
        workdir: optional_string_expand(&launch.workdir).context("workdir")?,
        stdin: optional_string_expand(&launch.stdin).context("stdin")?,
        timeout: launch.timeout,
        group_size: launch.group_size,
    })
}

#[cfg(test)]
mod tests {
    use crate::{
        config::{GitLabCustomExecutorConfigTemplate, GitLabExecutorPullPolicy, GitLabPollConfig},
        gitlab_config,
    };

    use super::*;

    fn get_test_paths() -> (String, String, String) {
        let home = dirs::home_dir().unwrap();
        let home = home.to_str().unwrap().to_owned();
        let exe = std::env::current_exe().unwrap();
        let exe = exe.to_str().unwrap().to_owned();
        let workdir = std::env::current_dir().unwrap();
        let workdir = workdir.to_str().unwrap().to_owned();
        (home, exe, workdir)
    }

    #[test]
    fn string_expand() {
        let (home, exe, workdir) = get_test_paths();
        let text = "~/ a $HOME of $NAME is $THIS for $ME at $PWD when $SOMETHING happens";
        let result = string_expand_impl(
            text,
            "name",
            &GitLabRunnerInstance {
                tags: Vec::new(),
                launch_priority: None,
                config_variables: [("ME".to_owned(), "me".to_owned())].into_iter().collect(),
            },
            &|v| match v {
                "SOMETHING" => Some("something"),
                _ => None,
            },
        );
        assert!(result.is_ok(), "{:?}", result);
        assert_eq!(
            result.unwrap(),
            format!(
                "{}/ a {} of name is {} for me at {} when something happens",
                home, home, exe, workdir
            )
        );
    }

    #[test]
    fn runner_expand() {
        let (home, exe, workdir) = get_test_paths();
        let config = gitlab_config::Runner {
            builds_dir: "~/$FOO/$NAME".into(),
            cache_dir: "$PWD/$BAR".into(),
            executor: gitlab_config::Executor::Custom {
                custom: gitlab_config::CustomExecutor {
                    config_exec: "$THIS".into(),
                    config_args: vec!["$A1".to_owned()],
                    prepare_exec: "$A2".into(),
                    prepare_args: vec!["$A3".to_owned(), "$A4".to_owned()],
                    run_exec: "$A5".into(),
                    run_args: vec!["$A6".to_owned()],
                    cleanup_exec: "$A7".into(),
                    cleanup_args: vec!["$NAME".to_owned()],
                },
            },
            environment: Some(vec!["$BAZ".to_owned()]),
        };
        let expanded = expand_runner_config_template(
            &config,
            "name",
            &GitLabRunnerInstance {
                tags: Vec::new(),
                launch_priority: None,
                config_variables: [
                    ("FOO", "foo"),
                    ("BAR", "bar"),
                    ("BAZ", "baz"),
                    ("A1", "a1"),
                    ("A2", "a2"),
                    ("A3", "a3"),
                    ("A4", "a4"),
                    ("A5", "a5"),
                    ("A6", "a6"),
                    ("A7", "a7"),
                    ("A8", "a8"),
                ]
                .into_iter()
                .map(|(a, b)| (a.to_owned(), b.to_owned()))
                .collect(),
            },
        );
        assert!(expanded.is_ok(), "{:?}", expanded);
        let expanded = expanded.unwrap();
        assert_eq!(expanded.builds_dir, format!("{}/foo/name", home));
        assert_eq!(expanded.cache_dir, format!("{}/bar", workdir));
        assert_eq!(expanded.environment, Some(vec!["baz".to_owned()]));
        match expanded.executor {
            Executor::Custom { custom } => {
                assert_eq!(custom.config_exec, exe);
                assert_eq!(custom.config_args, vec!["a1".to_owned()]);
                assert_eq!(custom.prepare_exec, "a2");
                assert_eq!(custom.prepare_args, vec!["a3".to_owned(), "a4".into()]);
                assert_eq!(custom.run_exec, "a5");
                assert_eq!(custom.run_args, vec!["a6".to_owned()]);
                assert_eq!(custom.cleanup_exec, "a7");
                assert_eq!(custom.cleanup_args, vec!["name".to_owned()]);
            }
            Executor::Shell => panic!("Invalid executor"),
        }
    }

    fn build_dummy_config_executor(
        config: GitLabCustomExecutorConfigTemplate,
        builds_dir: String,
    ) -> GitLabRunnersConfig {
        GitLabRunnersConfig {
            executor: Some(config),
            name: "".into(),
            project: "".into(),
            hostname: "".into(),
            management_token: "".into(),
            runners: HashMap::new(),
            poll: GitLabPollConfig { interval: 1 },
            launch: None,
            runner: Runner {
                builds_dir,
                cache_dir: "".into(),
                executor: Executor::Custom {
                    custom: CustomExecutor {
                        config_exec: "".into(),
                        config_args: Vec::new(),
                        prepare_exec: "".into(),
                        prepare_args: Vec::new(),
                        run_exec: "".into(),
                        run_args: Vec::new(),
                        cleanup_exec: "".into(),
                        cleanup_args: Vec::new(),
                    },
                },
                environment: Some(Vec::new()),
            },
        }
    }

    #[test]
    fn executor_expand_none() {
        let (home, exe, workdir) = get_test_paths();
        let config = build_dummy_config_executor(
            GitLabCustomExecutorConfigTemplate {
                builds_dir: None,
                image_dir: "$PWD/$FOO".into(),
                image_cache_dir: None,
                image_tmp_dir: None,
                pull_policy: GitLabExecutorPullPolicy::Always,
                apptainer_executable: "~/bin/apptainer".into(),
                gpu_amd: BoolOrString::Bool(false),
                gpu_nvidia: BoolOrString::Bool(true),
                mount: vec!["$BAR".to_owned(), "$THIS".into()],
                description: None,
            },
            "$HOME/builds".into(),
        );
        let expanded = expand_executor_config_template(
            &config,
            "name",
            &GitLabRunnerInstance {
                tags: Vec::new(),
                launch_priority: None,
                config_variables: [("FOO", "foo"), ("BAR", "bar"), ("BAZ", "baz")]
                    .into_iter()
                    .map(|(a, b)| (a.to_owned(), b.to_owned()))
                    .collect(),
            },
        );
        assert!(expanded.is_ok(), "{:?}", expanded);
        let expanded = expanded.unwrap();
        assert_eq!(
            expanded.builds_dir.to_str().unwrap(),
            format!("{}/builds", home)
        );
        assert_eq!(
            expanded.image_dir.to_str().unwrap(),
            format!("{}/foo", workdir)
        );
        assert_eq!(
            expanded.image_dir.to_str().unwrap(),
            format!("{}/foo", workdir)
        );
        assert_eq!(expanded.image_cache_dir, None);
        assert_eq!(expanded.image_tmp_dir, None);
        assert_eq!(expanded.pull_policy, GitLabExecutorPullPolicy::Always);
        assert_eq!(
            expanded.apptainer_executable.to_str().unwrap(),
            format!("{}/bin/apptainer", home)
        );
        assert_eq!(expanded.gpu_amd, false);
        assert_eq!(expanded.gpu_nvidia, true);
        assert_eq!(expanded.mount, vec!["bar".to_owned(), exe]);
        assert_eq!(expanded.description, None);
    }

    #[test]
    fn executor_expand_some() {
        let (home, exe, workdir) = get_test_paths();
        let config = build_dummy_config_executor(
            GitLabCustomExecutorConfigTemplate {
                builds_dir: Some("$HOME/builds2".into()),
                image_dir: "$PWD/$FOO".into(),
                image_cache_dir: Some("$HOME/cache".into()),
                image_tmp_dir: Some("~/tmp".into()),
                pull_policy: GitLabExecutorPullPolicy::Never,
                apptainer_executable: "~/bin/apptainer".into(),
                gpu_amd: BoolOrString::String("$TRUE".into()),
                gpu_nvidia: BoolOrString::String("$FALSE".into()),
                mount: vec!["$BAR".to_owned(), "$THIS".into()],
                description: Some("$BAZ".into()),
            },
            "$HOME/builds".into(),
        );
        let expanded = expand_executor_config_template(
            &config,
            "name",
            &GitLabRunnerInstance {
                tags: Vec::new(),
                launch_priority: None,
                config_variables: [
                    ("FOO", "foo"),
                    ("BAR", "bar"),
                    ("BAZ", "baz"),
                    ("TRUE", "true"),
                    ("FALSE", "false"),
                ]
                .into_iter()
                .map(|(a, b)| (a.to_owned(), b.to_owned()))
                .collect(),
            },
        );
        assert!(expanded.is_ok(), "{:?}", expanded);
        let expanded = expanded.unwrap();
        assert_eq!(
            expanded.builds_dir.to_str().unwrap(),
            format!("{}/builds2", home)
        );
        assert_eq!(
            expanded.image_dir.to_str().unwrap(),
            format!("{}/foo", workdir)
        );
        assert_eq!(
            expanded.image_dir.to_str().unwrap(),
            format!("{}/foo", workdir)
        );
        assert_eq!(
            expanded.image_cache_dir.unwrap().to_str().unwrap(),
            format!("{}/cache", home)
        );
        assert_eq!(
            expanded.image_tmp_dir.unwrap().to_str().unwrap(),
            format!("{}/tmp", home)
        );
        assert_eq!(expanded.pull_policy, GitLabExecutorPullPolicy::Never);
        assert_eq!(
            expanded.apptainer_executable.to_str().unwrap(),
            format!("{}/bin/apptainer", home)
        );
        assert_eq!(expanded.gpu_amd, true);
        assert_eq!(expanded.gpu_nvidia, false);
        assert_eq!(expanded.mount, vec!["bar".to_owned(), exe]);
        assert_eq!(expanded.description, Some("baz".into()));
    }

    fn build_dummy_config_launch(config: GitLabLaunchConfig) -> GitLabRunnersConfig {
        GitLabRunnersConfig {
            executor: None,
            name: "".into(),
            project: "".into(),
            hostname: "".into(),
            management_token: "".into(),
            runners: HashMap::new(),
            poll: GitLabPollConfig { interval: 1 },
            launch: Some(config),
            runner: Runner {
                builds_dir: "".into(),
                cache_dir: "".into(),
                executor: Executor::Custom {
                    custom: CustomExecutor {
                        config_exec: "".into(),
                        config_args: Vec::new(),
                        prepare_exec: "".into(),
                        prepare_args: Vec::new(),
                        run_exec: "".into(),
                        run_args: Vec::new(),
                        cleanup_exec: "".into(),
                        cleanup_args: Vec::new(),
                    },
                },
                environment: Some(Vec::new()),
            },
        }
    }

    #[test]
    fn launch_expand_none() {
        let (home, exe, workdir) = get_test_paths();
        let paths = Paths {
            config_file: "config-path".into(),
            data_dir: "data-path".into(),
            generated_config_file: Some("generated-config-path".into()),
        };
        let config = build_dummy_config_launch(GitLabLaunchConfig {
            executable: "~/bin/$FOO".into(),
            args: vec![
                "$PWD/$BAR".to_owned(),
                "~/".into(),
                "$THIS".into(),
                "$CONFIG-$NUM_JOBS".into(),
            ],
            workdir: None,
            stdin: None,
            timeout: None,
            group_size: 43,
        });
        let expanded = expand_launch_config_template(
            &paths,
            &config,
            "name",
            &GitLabRunnerInstance {
                tags: Vec::new(),
                launch_priority: None,
                config_variables: [("FOO", "foo"), ("BAR", "bar"), ("BAZ", "baz")]
                    .into_iter()
                    .map(|(a, b)| (a.to_owned(), b.to_owned()))
                    .collect(),
            },
            42,
        );
        assert!(expanded.is_ok(), "{:?}", expanded);
        let expanded = expanded.unwrap();
        assert_eq!(expanded.executable, format!("{}/bin/foo", home));
        assert_eq!(
            expanded.args,
            vec![
                format!("{}/bar", workdir),
                format!("{}/", home),
                exe,
                "generated-config-path-42".into()
            ]
        );
        assert_eq!(expanded.workdir, None);
        assert_eq!(expanded.stdin, None);
        assert_eq!(expanded.timeout, None);
        assert_eq!(expanded.group_size, 43);
    }

    #[test]
    fn launch_expand_some() {
        let (home, exe, workdir) = get_test_paths();
        let paths = Paths {
            config_file: "config-path".into(),
            data_dir: "data-path".into(),
            generated_config_file: Some("generated-config-path".into()),
        };
        let config = build_dummy_config_launch(GitLabLaunchConfig {
            executable: "~/bin/$FOO".into(),
            args: vec![
                "$PWD/$BAR".to_owned(),
                "~/".into(),
                "$THIS".into(),
                "$CONFIG-$NUM_JOBS".into(),
            ],
            workdir: Some("$FOO".into()),
            stdin: Some("$FOO $BAR $BAZ".into()),
            timeout: Some(1),
            group_size: 43,
        });
        let expanded = expand_launch_config_template(
            &paths,
            &config,
            "name",
            &GitLabRunnerInstance {
                tags: Vec::new(),
                launch_priority: None,
                config_variables: [("FOO", "foo"), ("BAR", "bar"), ("BAZ", "baz")]
                    .into_iter()
                    .map(|(a, b)| (a.to_owned(), b.to_owned()))
                    .collect(),
            },
            42,
        );
        assert!(expanded.is_ok(), "{:?}", expanded);
        let expanded = expanded.unwrap();
        assert_eq!(expanded.executable, format!("{}/bin/foo", home));
        assert_eq!(
            expanded.args,
            vec![
                format!("{}/bar", workdir),
                format!("{}/", home),
                exe,
                "generated-config-path-42".into()
            ]
        );
        assert_eq!(expanded.workdir, Some("foo".into()));
        assert_eq!(expanded.stdin, Some("foo bar baz".into()));
        assert_eq!(expanded.timeout, Some(1));
        assert_eq!(expanded.group_size, 43);
    }
}
