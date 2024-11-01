use log::warn;
use std::{
    collections::HashMap,
    fs::read_to_string,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{de::Error, Deserializer, Serializer};
use serde_derive::{Deserialize, Serialize};

use crate::{cli, gitlab_config};

pub const CONFIG_FILE_NAME: &str = "gitlab-meta-runner.toml";
pub const DATA_DIR_NAME: &str = "gitlab-meta-runner";

pub fn get_default_config_file_path() -> PathBuf {
    dirs::config_dir().unwrap().join(CONFIG_FILE_NAME)
}

pub fn get_default_data_dir() -> PathBuf {
    dirs::data_local_dir().unwrap().join(DATA_DIR_NAME)
}

pub fn get_tokens_file_path(data_dir: &PathBuf, meta_runner_name: &String) -> PathBuf {
    data_dir.join(format!("{}.tokens", meta_runner_name))
}

pub fn get_generated_config_file_path(paths: &cli::Paths, meta_runner_name: &String) -> PathBuf {
    paths
        .generated_config_file
        .as_ref()
        .unwrap_or(
            &paths
                .data_dir
                .join(format!("{}.gitlab-config.toml", meta_runner_name)),
        )
        .to_owned()
}

pub fn get_token_placeholder() -> String {
    "enter-your-token-here".into()
}

// workaround for serde issues related to default values
fn false_bool_or_string() -> BoolOrString {
    BoolOrString::Bool(false)
}

fn one() -> usize {
    1
}

/// Used for bools that can be variable-expanded
#[derive(Debug)]
pub enum BoolOrString {
    Bool(bool),
    String(String),
}

impl serde::Serialize for BoolOrString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            BoolOrString::Bool(b) => serializer.serialize_bool(*b),
            BoolOrString::String(s) => serializer.serialize_str(s),
        }
    }
}

impl<'de> serde::Deserialize<'de> for BoolOrString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = toml::Value::deserialize(deserializer)?;
        match value {
            toml::Value::Boolean(b) => Ok(BoolOrString::Bool(b)),
            toml::Value::String(s) => Ok(BoolOrString::String(s)),
            _ => Err(D::Error::custom("Expected string or boolean")),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GitLabRunnerInstance {
    /// Tags whose associated jobs will be run by this runner
    pub tags: Vec<String>,
    /// Variables to be expanded in the template instantiation
    /// Naming to avoid confusing with environment variables
    pub config_variables: HashMap<String, String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GitLabLaunchConfig {
    /// Executable name or path, will NOT be variable-expanded
    pub executable: String,
    /// Arguments to pass to the executable, they will be variable-expanded
    pub args: Vec<String>,
    /// Working directory for the executable, this will be variable-expanded
    pub workdir: Option<String>,
    /// The input to pass to the executable via stdin, this will be variable-expanded
    pub stdin: Option<String>,
    /// The time to wait (in seconds) for each launch command to finish, will NOT be variable-expanded
    pub timeout: Option<u32>,
    #[serde(default = "one")]
    /// The number of jobs to launch in a single launch command, will NOT be variable-expanded
    pub group_size: usize,
}

#[derive(Debug, Copy, Clone, Deserialize, Serialize)]
pub enum GitLabExecutorPullPolicy {
    #[serde(rename = "always")]
    /// Always pull an image, regardless of whether its file is present
    Always,
    #[serde(rename = "if-not-present")]
    /// Only pull an image if the image file is not present
    IfNotPresent,
    #[serde(rename = "never")]
    /// Never pull an image
    Never,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GitLabCustomExecutorConfigTemplate {
    /// Path to store the image files in, will be variable-expanded
    pub image_dir: String,
    /// Path to use for caching image layers, will be variable-expanded
    pub image_cache_dir: Option<String>,
    /// Path to use for temporary files during pull, will be variable-expanded
    pub image_tmp_dir: Option<String>,
    /// Pull policy to use for images, will NOT be variable-expanded
    pub pull_policy: GitLabExecutorPullPolicy,
    /// Path to the apptainer executable (may be relative to workdir or $PATH), will NOT be variable-expanded
    pub apptainer_executable: PathBuf,
    #[serde(default = "false_bool_or_string")]
    /// Mount AMD GPU devices, will be variable-expanded
    pub gpu_amd: BoolOrString,
    #[serde(default = "false_bool_or_string")]
    /// Mount NVIDIA GPU devices, will be variable-expanded
    pub gpu_nvidia: BoolOrString,
    #[serde(default = "Vec::new")]
    /// Additional bind mounts to use in the container, every individual entry will be variable-expanded
    pub mount: Vec<String>,
}

/// GitLabCustomExcutorConfigTemplate after variable expansion
#[derive(Debug, Serialize)]
pub struct GitLabCustomExecutorConfig {
    pub image_dir: PathBuf,
    pub image_cache_dir: Option<PathBuf>,
    pub image_tmp_dir: Option<PathBuf>,
    pub pull_policy: GitLabExecutorPullPolicy,
    pub apptainer_executable: PathBuf,
    pub gpu_amd: bool,
    pub gpu_nvidia: bool,
    pub mount: Vec<String>,
    pub builds_dir: PathBuf,
    pub cache_dir: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GitLabPollConfig {
    /// Interval (in seconds) for polling for new jobs
    pub interval: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GitLabRunnersConfig {
    /// Unique name for the meta-runner
    pub name: String,
    /// GitLab Project name for the meta-runner
    pub project: String,
    /// GitLab hostname for the meta-runner
    pub hostname: String,
    /// GitLab project token with read_api, create_runner, manage_runner permissions
    pub management_token: String,
    /// Array of runner instances - each runner instance will be registered as a gitlab-runner,
    /// and all variable values specified will be used for expansion of the configuration template
    pub runners: HashMap<String, GitLabRunnerInstance>,
    /// Configuration for polling for new jobs
    pub poll: GitLabPollConfig,
    /// Configuration for launching ephemeral runners
    /// Some of the configuration variables allow variable expansion from the runner instance variables
    pub launch: Option<GitLabLaunchConfig>,
    /// Configuration for the custom executor
    /// Some of the configuration variables allow variable expansion from the runner instance variables
    pub executor: Option<GitLabCustomExecutorConfigTemplate>,
    /// Configuration template for gitlab-runner config file
    /// It will be instantiated for every runner in the runners array,
    /// expanding occurrences of the runner instance variables into their values
    pub runner: gitlab_config::Runner,
}

fn strs_to_strings(strs: &[&str]) -> Vec<String> {
    strs.iter().map(|&s| s.into()).collect()
}

pub fn get_default_config() -> GitLabRunnersConfig {
    GitLabRunnersConfig {
        name: "meta-runner".into(),
        project: "gitlab-org/gitlab".into(),
        hostname: "gitlab.com".into(),
        management_token: get_token_placeholder(),
        runner: gitlab_config::Runner {
            builds_dir: "$HOME/builds/$NAME/".into(),
            cache_dir: "$HOME/cache/".into(),
            executor: gitlab_config::Executor::Custom {
                custom: gitlab_config::CustomExecutor {
                    config_exec: "$THIS".into(),
                    config_args: strs_to_strings(&["executor", "$NAME", "config"]),
                    prepare_exec: "$THIS".into(),
                    prepare_args: strs_to_strings(&["executor", "$NAME", "prepare"]),
                    run_exec: "$THIS".into(),
                    run_args: strs_to_strings(&["executor", "$NAME", "run"]),
                    cleanup_exec: "$THIS".into(),
                    cleanup_args: strs_to_strings(&["executor", "$NAME", "cleanup"]),
                },
            },
            environment: None,
        },
        launch: Some(GitLabLaunchConfig {
            executable: "sbatch".into(),
            args: [].into_iter().map(str::to_string).collect(),
            timeout: None,
            stdin: Some(
                "#!/bin/bash\ngitlab-runner run-single --config $CONFIG --runner $NAME --max-builds $NUM_JOBS --wait-timeout 1\n".into(),
            ),
            workdir: None,
            group_size: 1,
        }),
        poll: GitLabPollConfig { interval: 30 },
        runners: [(
            "test-runner".to_owned(),
            GitLabRunnerInstance {
                tags: vec!["tag-1".to_owned(), "tag-2".to_owned()],
                config_variables: [("VARIABLE", "value")]
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .into_iter()
                    .collect(),
            },
        )]
        .into_iter()
        .collect(),
        executor: Some(GitLabCustomExecutorConfigTemplate {
            image_dir: "$HOME/images".into(),
            image_cache_dir: None,
            image_tmp_dir: None,
            pull_policy: GitLabExecutorPullPolicy::IfNotPresent,
            apptainer_executable: "apptainer".into(),
            gpu_amd: BoolOrString::Bool(false),
            gpu_nvidia: BoolOrString::Bool(false),
            mount: Vec::new(),
        }),
    }
}

pub fn read_config(filename: &Path) -> anyhow::Result<GitLabRunnersConfig> {
    let content = read_to_string(filename)?;
    let parsed: GitLabRunnersConfig = toml::from_str(&content)?;
    if parsed.management_token == get_token_placeholder() {
        warn!("management_token uses placeholder value, API operations will fail")
    }
    Ok(parsed)
}

pub fn get_default_config_str() -> String {
    toml::to_string_pretty(&get_default_config()).unwrap()
}

pub fn write_default_config(filename: &Path) -> anyhow::Result<()> {
    let mut file = std::fs::File::create_new(filename)?;
    file.write_all(get_default_config_str().as_bytes())?;
    Ok(())
}

pub fn read_tokens(
    filename: &Path,
) -> anyhow::Result<HashMap<String, gitlab_config::RunnerRegistration>> {
    let content = match read_to_string(filename) {
        Ok(str) => str,
        Err(e) => match e.kind() {
            // no token file means no registered runners
            std::io::ErrorKind::NotFound => String::new(),
            // everything else is a true error
            _ => Err(e)?,
        },
    };
    Ok(toml::from_str(&content)?)
}

pub fn write_tokens(
    filename: &Path,
    tokens: &HashMap<String, gitlab_config::RunnerRegistration>,
) -> anyhow::Result<()> {
    let mut file = std::fs::File::create(filename)?;
    file.write_all(
        format!(
            "# autogenerated by gitlab-meta-runner\n{}",
            toml::to_string(tokens)?
        )
        .as_bytes(),
    )?;
    Ok(())
}

pub fn write_gitlab_runner_configurations(
    filename: &PathBuf,
    runners: &Vec<gitlab_config::RegisteredRunner>,
) -> anyhow::Result<()> {
    let root: HashMap<_, _> = [("runners".to_owned(), runners)].into_iter().collect();
    let mut file = std::fs::File::create(filename)?;
    file.write_all(
        format!(
            "# autogenerated by gitlab-meta-runner\n{}",
            toml::to_string(&root)?
        )
        .as_bytes(),
    )?;
    Ok(())
}
