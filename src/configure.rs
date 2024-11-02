use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use anyhow::Context;
use futures::future::join_all;
use gitlab::{api::ApiError, RestError};
use http::StatusCode;
use log::{error, warn};

use crate::{
    cli::Paths,
    config::{
        get_generated_config_file_path, get_tokens_file_path, read_config, read_tokens,
        write_gitlab_runner_configurations, write_tokens, GitLabRunnersConfig,
    },
    gitlab_config::{RegisteredRunner, RunnerRegistration},
    gitlab_wrap::{
        add_project_runner, delete_runner, fetch_project, init_client, update_runner,
        RunnerParameters,
    },
    template::expand_runner_config_template,
};

fn runner_name_to_description(config: &GitLabRunnersConfig, name: &str) -> String {
    format!("{}-{}", config.name, name)
}

fn instantiate_gitlab_runner_configurations(
    config: &GitLabRunnersConfig,
    registrations: &HashMap<String, RunnerRegistration>,
) -> anyhow::Result<Vec<RegisteredRunner>> {
    let runners = &config.runners;
    runners
        .iter()
        .map(|(name, instance)| {
            Ok(RegisteredRunner {
                name: name.clone(),
                config: expand_runner_config_template(&config.runner, name, instance)
                    .context(name.clone())?,
                url: format!("https://{}", config.hostname),
                registration: registrations.get(name).unwrap().clone(),
            })
        })
        .collect()
}

pub fn configure(paths: &Paths) -> anyhow::Result<()> {
    let config = read_config(&paths.config_file).context(format!(
        "Failed reading config file {:?}",
        paths.config_file
    ))?;
    std::fs::create_dir_all(&paths.data_dir).context("Creating data dir failed")?;
    let token_file_path = get_tokens_file_path(&paths.data_dir, &config.name);
    let runner_config_file_path = get_generated_config_file_path(&paths, &config.name);
    let tokens = update_registrations(&config, &token_file_path).context(format!(
        "Failed updating runner registrations at {:?}",
        token_file_path
    ))?;
    let instantiated_configs = instantiate_gitlab_runner_configurations(&config, &tokens)
        .context("Failed instantiating runner config entries")?;
    write_gitlab_runner_configurations(&runner_config_file_path, &instantiated_configs).context(
        format!(
            "Failed writing runner configuration file {:?}",
            runner_config_file_path
        ),
    )?;
    eprintln!(
        "Wrote gitlab-runner configuration file {:?}",
        runner_config_file_path
    );
    Ok(())
}

fn is_error_not_found<T>(v: &Result<T, ApiError<RestError>>) -> bool {
    match v {
        Ok(_) => false,
        Err(ApiError::GitlabService {
            status: http::StatusCode::NOT_FOUND,
            data: _,
        }) => true,
        Err(ApiError::GitlabWithStatus { status, msg: _ }) => *status == StatusCode::NOT_FOUND,
        Err(_) => false,
    }
}

#[tokio::main]
async fn update_registrations(
    config: &GitLabRunnersConfig,
    token_file: &PathBuf,
) -> anyhow::Result<HashMap<String, RunnerRegistration>> {
    let tokens = read_tokens(&token_file).context(format!(
        "Failed reading registration tokens {:?}",
        token_file
    ))?;
    let client = init_client(&config.hostname, &config.management_token)
        .await
        .context("Failed initializing GitLab client")?;
    let project = fetch_project(&client, &config.project)
        .await
        .context("Failed fetching project information")?;
    let mut current_keys: HashSet<String> = tokens.keys().cloned().collect();
    let mut new_keys: HashSet<String> = config.runners.keys().cloned().collect();
    // submit update requests for all already registered runners
    let to_update: Vec<_> = current_keys.intersection(&new_keys).cloned().collect();
    let update_count = to_update.len();
    let update_futures = to_update.iter().map(|key| {
        let runner = config.runners.get(key).unwrap();
        let runner_id = tokens.get(key).unwrap().id;
        let params = RunnerParameters {
            description: runner_name_to_description(config, key),
            tags: runner.tags.clone(),
        };
        update_runner(&client, runner_id, params)
    });
    let update_results = join_all(update_futures).await;
    let mut new_tokens = HashMap::new();
    let mut errors = Vec::new();
    // first handle all updated runners, any 404 means we need to move it to new_keys
    for (key, result) in to_update.into_iter().zip(update_results.into_iter()) {
        if is_error_not_found(&result) {
            warn!("Runner {} is missing, will recreate it", key);
            current_keys.remove(&key);
            new_keys.insert(key);
        } else {
            new_tokens.insert(key.clone(), tokens[&key].clone());
            if let Err(e) = result {
                error!("Update of runner {} failed, keeping it in the list", key);
                errors.push(e);
            }
        }
    }
    // then add and delete runners
    let to_add: Vec<_> = new_keys.difference(&current_keys).collect();
    let to_delete: Vec<_> = current_keys.difference(&new_keys).collect();
    let add_count = to_add.len();
    let del_count = to_delete.len();
    let add_futures = to_add.iter().map(|new_key| {
        let runner = config.runners.get(*new_key).unwrap();
        let params = RunnerParameters {
            description: runner_name_to_description(config, new_key),
            tags: runner.tags.clone(),
        };
        add_project_runner(&client, &project, params)
    });
    let delete_futures = to_delete.iter().map(|old_key| {
        let runner_id = tokens.get(*old_key).unwrap().id;
        delete_runner(&client, runner_id)
    });
    // first wait for all futures to finish
    let add_results = join_all(add_futures).await;
    let delete_results = join_all(delete_futures).await;
    // then add all successfully registered runners to the file
    for (key, result) in to_add.into_iter().zip(add_results.into_iter()) {
        match result {
            Ok(registration) => {
                new_tokens.insert(key.clone(), registration.clone());
            }
            Err(e) => {
                error!("Registration of runner {} failed", key);
                errors.push(e);
            }
        };
    }
    // then check if there were any non 404 errors during deletion
    for (key, result) in to_delete.into_iter().zip(delete_results.into_iter()) {
        if is_error_not_found(&result) {
            warn!("Runner {} is missing, removing from token list", key);
        } else if let Err(e) = result {
            error!("Deletion of runner {} failed, keeping it in the list", key);
            errors.push(e);
        }
    }
    write_tokens(&token_file, &new_tokens).context("Writing runner registration tokens")?;
    eprintln!(
        "API requests done, {} runners added, {} runners updated, {} runners deleted",
        add_count, update_count, del_count
    );
    // report the first error we found
    if let Some(err) = errors.into_iter().next() {
        Err(err)?
    }
    Ok(new_tokens)
}
