use anyhow::Context;
use documented::DocumentedFields;
use inkjet::{
    formatter::Terminal,
    theme::{vendored, Theme},
    Highlighter, Language,
};
use itertools::Itertools;
use log::warn;
use std::{
    collections::HashMap,
    fs::read_to_string,
    io::Write,
    path::{Path, PathBuf},
};
use struct_field_names_as_array::FieldNamesAsArray;
use termcolor::{ColorChoice, StandardStream};
use toml_edit::{DocumentMut, RawString};

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

#[derive(Debug, DocumentedFields, FieldNamesAsArray, Deserialize, Serialize)]
pub struct GitLabRunnerInstance {
    /// Tags whose associated jobs will be run by this runner
    pub tags: Vec<String>,
    /// Priority in which the instances' launch processes should be executed, higher priority means earlier launch.
    /// All jobs without a priority will be launched last.
    pub launch_priority: Option<u32>,
    /// Variables to be expanded in the template instantiation.
    /// Each value needs to be a string!
    // Naming to avoid confusing with environment variables
    pub config_variables: HashMap<String, String>,
}

#[derive(Debug, DocumentedFields, FieldNamesAsArray, Deserialize, Serialize)]
pub struct GitLabLaunchConfig {
    /// Executable name or path, will be variable-expanded
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

#[derive(Debug, Copy, Clone, Deserialize, Serialize, PartialEq)]
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

#[derive(Debug, DocumentedFields, FieldNamesAsArray, Deserialize, Serialize)]
pub struct GitLabCustomExecutorConfigTemplate {
    /// Override builds_dir provided by gitlab-runner config, will be variable-expanded
    pub builds_dir: Option<String>,
    /// Path to store the image files in, will be variable-expanded
    pub image_dir: String,
    /// Path to use for caching image layers, will be variable-expanded
    pub image_cache_dir: Option<String>,
    /// Path to use for temporary files during pull, will be variable-expanded
    pub image_tmp_dir: Option<String>,
    /// Pull policy to use for images, will NOT be variable-expanded
    pub pull_policy: GitLabExecutorPullPolicy,
    /// Path to the apptainer executable (may be relative to workdir or $PATH), will be variable-expanded
    pub apptainer_executable: String,
    #[serde(default = "false_bool_or_string")]
    /// Mount AMD GPU devices, will be variable-expanded
    pub gpu_amd: BoolOrString,
    #[serde(default = "false_bool_or_string")]
    /// Mount NVIDIA GPU devices, will be variable-expanded
    pub gpu_nvidia: BoolOrString,
    #[serde(default = "Vec::new")]
    /// Additional bind mounts to use in the container, every individual entry will be variable-expanded
    pub mount: Vec<String>,
    /// Custom string whose variable-expanded value will be reported in the driver name in the config stage
    pub description: Option<String>,
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
    pub description: Option<String>,
}

#[derive(Debug, DocumentedFields, FieldNamesAsArray, Deserialize, Serialize)]
pub struct GitLabPollConfig {
    /// Interval (in seconds) for polling for new jobs
    pub interval: u32,
}

#[derive(Debug, DocumentedFields, FieldNamesAsArray, Deserialize, Serialize)]
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
    /// Available variables are (in order of precedence)
    /// - $NAME for the runner instance name, to be passed to `gitlab-runner run-single --runner-name $NAME``
    /// - $THIS for the path to this executable
    /// - $CONFIG for the path to the generated gitlab-runner config file, to be passed to `gitlab-runner --config $CONFIG`
    /// - $NUM_JOBS for the number of jobs that were grouped together for this launch, to be passed to `gitlab-runner run-single --max-builds 1`
    /// - Any variables defined in runners.<runner_name>.config_variables
    /// - Any environment variables provided by gitlab-runner to this custom executor
    pub launch: Option<GitLabLaunchConfig>,
    /// Configuration for the custom executor
    /// Some of the configuration variables allow variable expansion from the runner instance variables
    /// Available variables are (in order of precedence)
    /// - $NAME for the runner instance name
    /// - $THIS for the path to this executable
    /// - Any variables defined in runners.<runner_name>.config_variables
    /// - Any environment variables provided by gitlab-runner to this custom executor
    pub executor: Option<GitLabCustomExecutorConfigTemplate>,
    /// Configuration template for gitlab-runner config file
    /// It will be instantiated for every runner in the runners array,
    /// expanding occurrences of the runner instance variables into their values
    /// Available variables are (in order of precedence)
    /// - $NAME for the runner instance name
    /// - $THIS for the path to this executable
    /// - Any variables defined in runners.<runner_name>.config_variables
    /// - Any environment variables available when calling `gitlab-meta-runner (configure|show-config)`
    pub runner: gitlab_config::Runner,
}

fn strs_to_strings(strs: &[&str]) -> Vec<String> {
    strs.iter().map(|&s| s.into()).collect()
}

pub fn get_example_config() -> GitLabRunnersConfig {
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
            environment: Some(vec!["ENV_VARIABLE=value".into()]),
        },
        launch: Some(GitLabLaunchConfig {
            executable: "sbatch".into(),
            args: [].into_iter().map(str::to_string).collect(),
            timeout: Some(300),
            stdin: Some(
                "#!/bin/bash\ngitlab-runner run-single --config $CONFIG --runner $NAME --max-builds $NUM_JOBS --wait-timeout 1\n".into(),
            ),
            workdir: Some("$HOME/launch".into()),
            group_size: 1,
        }),
        poll: GitLabPollConfig { interval: 30 },
        runners: [(
            "test-runner".to_owned(),
            GitLabRunnerInstance {
                tags: vec!["tag-1".to_owned(), "tag-2".to_owned()],
                launch_priority: Some(10),
                config_variables: [("VARIABLE", "value")]
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .into_iter()
                    .collect(),
            },
        )]
        .into_iter()
        .collect(),
        executor: Some(GitLabCustomExecutorConfigTemplate {
            builds_dir: Some("$HOME/builds".into()),
            image_dir: "$HOME/images".into(),
            image_cache_dir: Some("$HOME/image_cache".into()),
            image_tmp_dir: Some("$HOME/image_tmp".into()),
            pull_policy: GitLabExecutorPullPolicy::IfNotPresent,
            apptainer_executable: "apptainer".into(),
            gpu_amd: BoolOrString::Bool(false),
            gpu_nvidia: BoolOrString::Bool(false),
            mount: Vec::new(),
            description: Some("Slurm job $SLURM_JOB_ID".into()),
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

fn annotate_toml_table<T: DocumentedFields>(table: &mut toml_edit::Table) {
    for (mut key, value) in table.iter_mut() {
        let key_name = key.get().to_owned();
        let comments = T::get_field_docs(key_name).map_or("".into(), |comment| {
            format!("# {}\n", comment.lines().join("\n# "))
        });
        match value {
            toml_edit::Item::None => (),
            toml_edit::Item::Value(_) => {
                key.leaf_decor_mut().set_prefix(comments);
            }
            toml_edit::Item::Table(table) => {
                let original_decor = table
                    .decor()
                    .prefix()
                    .map_or(RawString::default(), |v| v.to_owned());
                table.decor_mut().set_prefix(format!(
                    "{}{}",
                    original_decor.as_str().unwrap_or(""),
                    comments
                ));
            }
            // doesn't appear in our configuration
            toml_edit::Item::ArrayOfTables(_) => todo!(),
        };
    }
}

pub fn get_example_config_str() -> String {
    let config = get_example_config();
    let mut document = toml::to_string_pretty(&config)
        .unwrap()
        .parse::<DocumentMut>()
        .unwrap();
    annotate_toml_table::<GitLabRunnersConfig>(document.as_table_mut());
    {
        let runners = document.get_mut("runners").unwrap();
        for (name, _) in &config.runners {
            annotate_toml_table::<GitLabRunnerInstance>(
                runners.get_mut(name).unwrap().as_table_mut().unwrap(),
            );
        }
    }
    annotate_toml_table::<GitLabPollConfig>(
        document.get_mut("poll").unwrap().as_table_mut().unwrap(),
    );
    annotate_toml_table::<GitLabLaunchConfig>(
        document.get_mut("launch").unwrap().as_table_mut().unwrap(),
    );
    annotate_toml_table::<GitLabCustomExecutorConfigTemplate>(
        document
            .get_mut("executor")
            .unwrap()
            .as_table_mut()
            .unwrap(),
    );
    let runner = document.get_mut("runner").unwrap().as_table_mut().unwrap();
    annotate_toml_table::<gitlab_config::Runner>(runner);
    annotate_toml_table::<gitlab_config::CustomExecutor>(
        runner.get_mut("custom").unwrap().as_table_mut().unwrap(),
    );
    document.to_string()
}

pub fn print_example_config_highlighted() {
    let config = get_example_config_str();
    let mut highlighter = Highlighter::new();
    let language = Language::Toml;
    let theme: Theme = Theme::from_helix(vendored::BASE16_TERMINAL).unwrap();
    let stream = StandardStream::stdout(ColorChoice::Auto);
    let formatter = Terminal::new(theme, stream);
    highlighter
        .highlight_to_writer(language, &formatter, &config, &mut std::io::sink())
        .unwrap();
    println!();
}

pub fn write_example_config(filename: &Path) -> anyhow::Result<()> {
    let mut file = std::fs::File::create_new(filename)
        .context(format!("Failed creating config file {:?}", filename))?;
    file.write_all(get_example_config_str().as_bytes())?;
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
