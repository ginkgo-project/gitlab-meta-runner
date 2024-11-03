use async_std::future;
use itertools::{Either, Itertools};
use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
    ops::Deref,
    time::Duration,
    u32,
};
use tokio_util::sync::CancellationToken;

use async_process::{Command, Stdio};
use futures::{future::join_all, select, AsyncReadExt, AsyncWriteExt, FutureExt};
use gitlab::AsyncGitlab;
use log::{debug, error, info};
use tokio::{
    signal,
    time::{self, MissedTickBehavior},
};

use crate::{
    check_config, cli,
    config::{read_config, GitLabLaunchConfig, GitLabRunnerInstance, GitLabRunnersConfig},
    gitlab_wrap::{fetch_pending_project_jobs, fetch_project, init_client, Job, Project},
    template::expand_launch_config_template,
};

use anyhow::{anyhow, Context};

struct MetaRunnerState {
    config: GitLabRunnersConfig,
    client: AsyncGitlab,
    project: Project,
    successful_job_ids: HashSet<u64>,
}

async fn initialize(paths: &cli::Paths) -> anyhow::Result<MetaRunnerState> {
    let config = read_config(&paths.config_file).context(format!(
        "Failed reading configuration {:?}",
        paths.config_file
    ))?;
    let client = init_client(&config.hostname, &config.management_token)
        .await
        .context("Failed configuring GitLab API client")?;
    let project = fetch_project(&client, &config.project).await?;
    Ok(MetaRunnerState {
        config,
        client,
        project,
        successful_job_ids: HashSet::new(),
    })
}

/// find the runner instance that has the correct tags with the smallest number of non-matching tags
fn find_match<'a>(
    instances: &'a HashMap<String, GitLabRunnerInstance>,
    job: &Job,
) -> Option<(&'a String, &'a GitLabRunnerInstance)> {
    let requested_tags: HashSet<_> = job.tags.iter().collect();
    instances
        .iter()
        .filter(|i| {
            let available_tags: HashSet<_> = i.1.tags.iter().collect();
            requested_tags.intersection(&available_tags).count() == requested_tags.len()
        })
        .min_by_key(|i| i.1.tags.len())
        .or_else(|| {
            info!("Could not find a suitable runner for pending job {:?}", job);
            None
        })
}

async fn check_jobs<'a>(
    state: &'a MetaRunnerState,
) -> anyhow::Result<Vec<(&'a String, &'a GitLabRunnerInstance, Job)>> {
    let jobs = fetch_pending_project_jobs(&state.client, &state.project).await?;
    Ok(jobs
        .into_iter()
        .filter(|job| !state.successful_job_ids.contains(&job.id))
        .filter_map(|job| match find_match(&state.config.runners, &job) {
            None => None,
            Some((name, instance)) => Some((name, instance, job)),
        })
        .collect())
}

async fn launch_runner(config: &GitLabLaunchConfig) -> anyhow::Result<()> {
    let mut command: Command = Command::new(&config.executable);
    if let Some(workdir) = &config.workdir {
        command.current_dir(workdir);
    }
    command.args(config.args.iter());
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    // if writing to stdin fails, we can't do much to fix it, so kill the process
    command.kill_on_drop(true);
    debug!("Spawning process {:?}", command);
    let mut child = command
        .spawn()
        .context(format!("Failed spawning process {:?}", command))?;
    debug!("Spawned process {:?}", child);
    {
        debug!(
            "Writing {} to stdin of {:?}",
            config.stdin.as_ref().unwrap_or(&String::new()),
            child
        );
        let stdin = child.stdin.as_mut().unwrap();
        let stdin_bytes: &[u8] = config.stdin.as_ref().map_or(&[], String::as_bytes);
        stdin
            .write_all(stdin_bytes)
            .await
            .context(format!("Failed writing to stdin of process {:?}", child))?;
    }
    debug!("Waiting for process {} to finish", child.id());
    let timeout_sec = config.timeout.unwrap_or(u32::MAX) as u64;
    let status = time::timeout(time::Duration::from_secs(timeout_sec), child.status())
        .await
        .context(format!("Process {} timed out", child.id()))?
        .context(format!(
            "Failed retrieving status for process {}",
            child.id()
        ))?;
    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    debug!("Fetching stdout and stderr for process {:?}", child);
    let stdout = child.stdout.as_mut().unwrap();
    let stderr = child.stderr.as_mut().unwrap();
    let read_stdout_future = stdout.read_to_end(&mut stdout_buf);
    let read_stderr_future = stderr.read_to_end(&mut stderr_buf);
    let (read_stdout, read_stderr) = futures::join!(read_stdout_future, read_stderr_future);
    read_stdout.context(format!("Failed reading from stdout of process {:?}", child))?;
    read_stderr.context(format!("Failed reading from stderr of process {:?}", child))?;
    let exit_status = status;
    let stdout = String::from_utf8_lossy(stdout_buf.as_slice());
    let stderr = String::from_utf8_lossy(stderr_buf.as_slice());
    debug!(
        "Runner launch with configuration {:?} produced output\nstdout:\n{}\nstderr:\n{}",
        config, stdout, stderr
    );
    if exit_status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "Runner launch with configuration {:?} failed with exit code {}\nstdout:\n{}\nstderr:\n{}",
            config,
            exit_status,
            stdout, stderr,
        ))
    }
}

struct PrintableJobVec<'a> {
    jobs: &'a Vec<&'a Job>,
}

impl Display for PrintableJobVec<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.jobs.len() >= 1 {
            let first = self.jobs.first().unwrap();
            write!(f, "{} ({})", first.name, first.id)?;
            for job in self.jobs.iter().skip(1) {
                write!(f, ", {} ({})", job.name, job.id)?;
            }
        }
        Ok(())
    }
}

async fn run_impl(paths: &cli::Paths, state: &MetaRunnerState) -> anyhow::Result<Vec<u64>> {
    let matched_jobs = check_jobs(state).await?;
    // Group jobs by runner instance
    let mut grouped_matched_jobs = HashMap::new();
    for (name, instance, job) in matched_jobs.iter() {
        if let None = grouped_matched_jobs.get(name) {
            grouped_matched_jobs.insert(name, (instance, Vec::new()));
        }
        grouped_matched_jobs.get_mut(name).unwrap().1.push(job);
    }
    let mut grouped_matched_jobs: Vec<_> = grouped_matched_jobs.into_iter().collect();
    grouped_matched_jobs.sort_by_key(|(_, (instance, _))| instance.launch_order);
    // Dispatch jobs
    let mut queue = Vec::new();
    // this unwrap can't fail because we ran check_config::check
    let group_size = state.config.launch.as_ref().unwrap().group_size;
    for (name, (instance, jobs)) in &grouped_matched_jobs {
        debug!(
            "Using runner {} {:?} to dispatch jobs {}",
            name,
            instance,
            PrintableJobVec { jobs }
        );
        let instantiated_config =
            expand_launch_config_template(paths, &state.config, name, instance, group_size)
                .unwrap(); // this can't fail because we ran check_config::check
        queue.push(async move {
            join_all(
                (0..jobs.len())
                    .into_iter()
                    .chunks(group_size as usize)
                    .into_iter()
                    .map(|_| async { launch_runner(&instantiated_config).await }),
            )
            .await
            .into_iter()
            .collect()
        });
    }
    // Collect results from dispatch
    let launch_results: Vec<Vec<anyhow::Result<_>>> = join_all(queue.into_iter()).await;
    let mut successful = Vec::new();
    for ((name, (_, jobs)), result) in grouped_matched_jobs.iter().zip(launch_results.iter()) {
        let job_chunks = jobs.into_iter().chunks(group_size as usize);
        let (success, failure): (Vec<_>, Vec<_>) = job_chunks
            .into_iter()
            .zip(result.into_iter())
            .partition_map(|(job_chunk, result)| match result {
                Ok(_) => Either::Left(job_chunk),
                Err(e) => Either::Right((job_chunk, e)),
            });
        if success.len() > 0 {
            let success_vec = success.into_iter().flatten().map(Deref::deref).collect();
            info!(
                "Launched runner {} for jobs {} successfully",
                name,
                PrintableJobVec { jobs: &success_vec }
            );
            successful.extend(success_vec.into_iter().map(|job| job.id));
        }
        for f in failure {
            error!(
                "Failed launching runner {} for jobs {}, error message: {:?}",
                name,
                PrintableJobVec {
                    jobs: &f.0.into_iter().map(Deref::deref).collect()
                },
                f.1
            )
        }
    }
    Ok(successful)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
pub async fn run(paths: cli::Paths) -> anyhow::Result<()> {
    check_config::check(&paths)?;
    let mut state = initialize(&paths).await?;
    let cancel_token = CancellationToken::new();
    let job_cancel_token = cancel_token.clone();

    let task = tokio::spawn(async move {
        let poll_duration = Duration::from_secs(state.config.poll.interval as u64);
        let mut timer = time::interval(poll_duration);
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            // Handle cancellation
            select! {
                _ = timer.tick().fuse() => (),
                _ =  job_cancel_token.cancelled().fuse() => {
                    info!("Poll task shutting down");
                    break
                }
            };
            // Actual poll loop
            info!("Polling for jobs...");
            let result = future::timeout(poll_duration, run_impl(&paths, &state)).await;
            match result {
                Ok(Ok(new_successful_jobs)) => state
                    .successful_job_ids
                    .extend(new_successful_jobs.into_iter()),
                Ok(Err(e)) => error!("Failed poll: {:?}", e),
                Err(_) => error!("Poll timed out"),
            };
        }
    });

    match signal::ctrl_c().await {
        Ok(()) => info!("Received shutdown signal (Ctrl+C), cancelling poll task"),
        Err(_) => error!("Failed to listen for shutdown signal, shutting down anyways."),
    }

    // the result of the shutdown signal send doesn't matter, since if it fails, the task already hung up
    cancel_token.cancel();
    task.await
        .context("Failed waiting for poll task to finish")?;

    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
pub async fn run_single(paths: &cli::Paths) -> anyhow::Result<()> {
    check_config::check(paths)?;
    let state = initialize(paths).await?;
    run_impl(paths, &state).await?;
    Ok(())
}
