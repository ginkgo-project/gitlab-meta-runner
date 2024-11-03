use documented::DocumentedFields;
use serde_derive::{Deserialize, Serialize};
use struct_field_names_as_array::FieldNamesAsArray;

#[derive(Debug, DocumentedFields, FieldNamesAsArray, Deserialize, Serialize)]
pub struct Runner {
    /// Directory to use for builds, will be variable-expanded
    pub builds_dir: String,
    /// Directory to use for build caches, will be variable-expanded
    pub cache_dir: String,
    #[serde(flatten)]
    /// The executor to use for this runner
    pub executor: Executor,
    /// Additional environment variables, will be variable-expanded
    pub environment: Option<Vec<String>>,
}

#[derive(Debug, DocumentedFields, FieldNamesAsArray, Deserialize, Serialize)]
pub struct CustomExecutor {
    /// The executable to configure a job, will be template-expanded
    pub config_exec: String,
    /// The arguments to pass to config_exec, will be template-expanded
    pub config_args: Vec<String>,
    /// The executable to prepare a job, will be template-expanded
    pub prepare_exec: String,
    /// The arguments to pass to prepare_exec, will be template-expanded
    pub prepare_args: Vec<String>,
    /// The executable to run a job, will be template-expanded
    pub run_exec: String,
    /// The arguments to pass to run_exec, will be template-expanded
    pub run_args: Vec<String>,
    /// The executable to execute to clean up after a job, will be template-expanded
    pub cleanup_exec: String,
    /// The arguments to pass to cleanup_exec, will be template-expanded
    pub cleanup_args: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "executor")]
pub enum Executor {
    #[serde(rename = "custom")]
    Custom { custom: CustomExecutor },
    #[serde(rename = "shell")]
    Shell,
}

#[derive(Debug, Serialize)]
pub struct RegisteredRunner {
    /// The runner name
    pub name: String,
    /// The actual configuration
    #[serde(flatten)]
    pub config: Runner,
    /// The Gitlab instance URL
    pub url: String,
    /// The registration used for this configuration
    #[serde(flatten)]
    pub registration: RunnerRegistration,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RunnerRegistration {
    /// The runner ID
    pub id: u64,
    /// The runner API token
    pub token: String,
}
