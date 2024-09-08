use futures::TryFutureExt;
use gitlab::{
    api::{ignore, paged, projects, runners, users, ApiError, AsyncQuery, Pagination},
    AsyncGitlab, Gitlab, GitlabError, RestError,
};
use log::debug;
use serde::{Deserialize, Serialize};

use crate::gitlab_config::RunnerRegistration;

type ApiResult<T> = Result<T, ApiError<RestError>>;

#[derive(Debug, Deserialize)]
pub struct Project {
    pub id: u64,
}

#[derive(Debug, Deserialize)]
pub struct Job {
    pub id: u64,
    pub name: String,
    #[serde(rename = "tag_list")]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RunnerParameters {
    pub description: String,
    #[serde(rename = "tag_list")]
    pub tags: Vec<String>,
}

pub async fn init_client(host: &str, token: &str) -> Result<AsyncGitlab, GitlabError> {
    Ok(Gitlab::builder(host, token).build_async().await?)
}

pub async fn fetch_project(client: &AsyncGitlab, project: &str) -> ApiResult<Project> {
    let endpoint = projects::Project::builder()
        .project(project)
        .build()
        .unwrap();
    Ok(endpoint
        .query_async(client)
        .and_then(|v| async move {
            debug!("Fetched project {}: {:?}", project, v);
            Ok(v)
        })
        .or_else(|e| async move {
            debug!("Failed fetching project {}: {:?}", project, e);
            Err(e)
        })
        .await?)
}

pub async fn fetch_pending_project_jobs(
    client: &AsyncGitlab,
    project: &Project,
) -> ApiResult<Vec<Job>> {
    let endpoint = projects::jobs::Jobs::builder()
        .project(project.id)
        .scope(projects::jobs::JobScope::Pending)
        .build()
        .unwrap();
    Ok(paged(endpoint, Pagination::All)
        .query_async(client)
        .and_then(|v| async move {
            debug!("Fetched project jobs for {}: {:?}", project.id, v);
            Ok(v)
        })
        .or_else(|e| async move {
            debug!("Failed project jobs for {}: {:?}", project.id, e);
            Err(e)
        })
        .await?)
}

pub async fn add_project_runner(
    client: &AsyncGitlab,
    project: &Project,
    runner: RunnerParameters,
) -> ApiResult<RunnerRegistration> {
    let endpoint = users::CreateRunner::builder()
        .project(project.id)
        .description(runner.description.clone())
        .tags(runner.tags.iter())
        .paused(false)
        .locked(true)
        .run_untagged(false)
        .build()
        .unwrap();
    Ok(endpoint
        .query_async(client)
        .and_then(|v| async move {
            debug!("Added project runner to {}: {:?}", project.id, v);
            Ok(v)
        })
        .or_else(|e| async move {
            debug!("Failed adding project runner to {}: {:?}", project.id, e);
            Err(e)
        })
        .await?)
}

pub async fn update_runner(
    client: &AsyncGitlab,
    runner_id: u64,
    params: RunnerParameters,
) -> ApiResult<()> {
    let success_params = params.clone();
    let error_params = params.clone();
    let endpoint = runners::EditRunner::builder()
        .runner(runner_id)
        .paused(false)
        .locked(true)
        .run_untagged(false)
        .description(params.description.clone())
        .tags(params.tags.iter())
        .build()
        .unwrap();
    Ok(ignore(endpoint)
        .query_async(client)
        .and_then(|v| async move {
            debug!("Updated runner {}: {:?}", runner_id, success_params);
            Ok(v)
        })
        .or_else(|e| async move {
            debug!(
                "Failed updating runner {} with {:?}: {:?}",
                runner_id, error_params, e
            );
            Err(e)
        })
        .await?)
}

pub async fn delete_runner(client: &AsyncGitlab, runner_id: u64) -> ApiResult<()> {
    let endpoint = runners::DeleteRunner::builder()
        .runner(runner_id)
        .build()
        .unwrap();
    Ok(ignore(endpoint)
        .query_async(client)
        .and_then(|()| async move {
            debug!("Deleted runner {}", runner_id);
            Ok(())
        })
        .or_else(|e| async move {
            debug!("Failed deleting runner {}: {:?}", runner_id, e);
            Err(e)
        })
        .await?)
}
