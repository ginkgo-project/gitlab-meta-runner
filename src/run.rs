use async_std::future;
use itertools::{Either, Itertools};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fmt::Display,
    time::{Duration, Instant},
    u32,
};
use tokio_util::sync::CancellationToken;

use async_process::{Command, Stdio};
use futures::{future::join_all, select, AsyncReadExt, AsyncWriteExt, FutureExt};
use gitlab::AsyncGitlab;
use log::{debug, error, info, warn};
use tokio::{
    signal,
    task::JoinHandle,
    time::{self, MissedTickBehavior},
};

use crate::{
    assign::{assign_jobs, candidates, free_slots, Assignment},
    check_config, cli,
    config::{read_config, GitLabLaunchConfig, GitLabLaunchCountConfig, GitLabRunnersConfig},
    failure::{
        is_access_failure, retry_delay, AccessFailureTracker, PermanentError, ACCESS_FAILURE_GRACE,
    },
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

fn read_runner_config(paths: &cli::Paths) -> anyhow::Result<GitLabRunnersConfig> {
    read_config(&paths.config_file).context(format!(
        "Failed reading configuration {:?}",
        paths.config_file
    ))
}

/// Connects to GitLab and looks up the configured project
async fn connect(config: &GitLabRunnersConfig) -> anyhow::Result<(AsyncGitlab, Project)> {
    let client = init_client(&config.hostname, &config.management_token)
        .await
        .context("Failed configuring GitLab API client")?;
    let project = fetch_project(&client, &config.project)
        .await
        .context(format!("Failed fetching project {}", config.project))?;
    Ok((client, project))
}

fn new_state(
    config: GitLabRunnersConfig,
    client: AsyncGitlab,
    project: Project,
) -> MetaRunnerState {
    MetaRunnerState {
        config,
        client,
        project,
        successful_job_ids: HashSet::new(),
    }
}

/// The error to report once GitLab has been refusing access for longer than ACCESS_FAILURE_GRACE
fn access_refused() -> PermanentError {
    PermanentError(format!(
        "GitLab refused access for {} minutes, check the management token and its project access",
        ACCESS_FAILURE_GRACE.as_secs() / 60
    ))
}

async fn initialize(paths: &cli::Paths) -> anyhow::Result<MetaRunnerState> {
    let config = read_runner_config(paths)?;
    let (client, project) = connect(&config).await?;
    Ok(new_state(config, client, project))
}

/// Like `initialize`, but retries connecting to GitLab with increasing delays.
/// Gives up with a permanent error if GitLab keeps refusing access for ACCESS_FAILURE_GRACE.
async fn initialize_with_retry(paths: &cli::Paths) -> anyhow::Result<MetaRunnerState> {
    let config =
        read_runner_config(paths).context(PermanentError("Invalid configuration".into()))?;
    let mut access_failures = AccessFailureTracker::new(ACCESS_FAILURE_GRACE);
    let mut attempt = 0;
    loop {
        match connect(&config).await {
            Ok((client, project)) => return Ok(new_state(config, client, project)),
            Err(e) => {
                if is_access_failure(&e) && access_failures.record_failure(Instant::now()) {
                    return Err(e.context(access_refused()));
                }
                let delay = retry_delay(attempt);
                warn!(
                    "Failed connecting to GitLab, retrying in {} s: {:?}",
                    delay.as_secs(),
                    e
                );
                time::sleep(delay).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

/// Waits until either `shutdown` completes, then cancels the poll task and waits for it,
/// or the poll task ends on its own, which is always an error: the task's own error, a panic, or an unexpected stop.
async fn supervise<F: std::future::Future<Output = ()>>(
    mut task: JoinHandle<anyhow::Result<()>>,
    shutdown: F,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    tokio::select! {
        _ = shutdown => {
            cancel.cancel();
            task.await.context("Failed waiting for poll task to finish")?
        }
        result = &mut task => match result {
            Ok(Ok(())) => Err(anyhow!("Poll task stopped unexpectedly")),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(anyhow::Error::new(e).context("Poll task panicked")),
        },
    }
}

/// Timeout (in seconds) for count commands without an explicit timeout
const DEFAULT_COUNT_TIMEOUT: u32 = 10;

/// Number of active launches reported by a count command: one per non-blank line
fn parse_count_output(stdout: &str) -> usize {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

/// Runs an (already expanded) count command and returns the number of active launches it reports
async fn count_active_launches(
    count: &GitLabLaunchCountConfig,
    workdir: Option<&str>,
) -> anyhow::Result<usize> {
    let mut command = Command::new(&count.executable);
    if let Some(workdir) = workdir {
        command.current_dir(workdir);
    }
    command.args(count.args.iter());
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.kill_on_drop(true);
    let timeout_sec = count.timeout.unwrap_or(DEFAULT_COUNT_TIMEOUT) as u64;
    let output = time::timeout(Duration::from_secs(timeout_sec), command.output())
        .await
        .context(format!("Count command {:?} timed out", command))?
        .context(format!("Failed running count command {:?}", command))?;
    if !output.status.success() {
        return Err(anyhow!(
            "Count command {:?} failed with exit code {}\nstderr:\n{}",
            command,
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(parse_count_output(&String::from_utf8_lossy(&output.stdout)))
}

/// Free job slots of every runner instance with a launch limit that could take one of the jobs.
/// Runner instances whose active launches can't be counted get no free slots.
async fn count_free_slots(
    paths: &cli::Paths,
    state: &MetaRunnerState,
    jobs: &[Job],
    group_size: usize,
) -> HashMap<String, usize> {
    let limited: BTreeSet<&String> = jobs
        .iter()
        .flat_map(|job| candidates(&state.config.runners, job))
        .filter(|(_, instance)| instance.max_launches.is_some())
        .map(|(name, _)| name)
        .collect();
    join_all(limited.into_iter().map(|name| async move {
        let instance = &state.config.runners[name];
        let max_launches = instance.max_launches.unwrap_or(0);
        let active =
            match expand_launch_config_template(paths, &state.config, name, instance, group_size) {
                Ok(GitLabLaunchConfig {
                    count: Some(count),
                    workdir,
                    ..
                }) => count_active_launches(&count, workdir.as_deref()).await,
                Ok(_) => Err(anyhow!("Missing [launch.count] configuration")),
                Err(e) => Err(e),
            };
        let free = match active {
            Ok(active) => {
                debug!(
                    "Runner {} has {} of {} launches active",
                    name, active, max_launches
                );
                free_slots(max_launches, active, group_size)
            }
            Err(e) => {
                error!(
                    "Failed counting active launches of runner {}, treating it as full: {:?}",
                    name, e
                );
                0
            }
        };
        (name.clone(), free)
    }))
    .await
    .into_iter()
    .collect()
}

/// Job IDs that should not be dispatched again: successfully launched and unmatched jobs.
/// Deferred jobs are left out, so they are retried on the next poll.
fn handled_job_ids(launched: impl IntoIterator<Item = u64>, unmatched: &[Job]) -> Vec<u64> {
    launched
        .into_iter()
        .chain(unmatched.iter().map(|job| job.id))
        .collect()
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
    let jobs: Vec<Job> = fetch_pending_project_jobs(&state.client, &state.project)
        .await?
        .into_iter()
        .filter(|job| !state.successful_job_ids.contains(&job.id))
        .collect();
    // this unwrap can't fail because we ran check_config::check
    let group_size = state.config.launch.as_ref().unwrap().group_size;
    let free = count_free_slots(paths, state, &jobs, group_size).await;
    let Assignment {
        assigned,
        deferred,
        unmatched,
    } = assign_jobs(&state.config.runners, jobs, &free);
    for job in &unmatched {
        debug!("Could not find a suitable runner for pending job {:?}", job);
    }
    if !deferred.is_empty() {
        info!(
            "All matching runners are at their launch limit, deferring jobs {}",
            PrintableJobVec {
                jobs: &deferred.iter().collect()
            }
        );
    }
    // sort descending by priority
    let mut grouped_matched_jobs = assigned;
    grouped_matched_jobs.sort_by_key(|(_, instance, _)| instance.launch_priority);
    grouped_matched_jobs.reverse();
    // Dispatch jobs
    let mut queue = Vec::new();
    for (name, instance, jobs) in &grouped_matched_jobs {
        debug!(
            "Using runner {} {:?} to dispatch jobs {}",
            name,
            instance,
            PrintableJobVec {
                jobs: &jobs.iter().collect()
            }
        );
        queue.push(async move {
            join_all(
                (0..jobs.len())
                    .into_iter()
                    .chunks(group_size as usize)
                    .into_iter()
                    .map(|chunk| {
                        let count = chunk.count();
                        async move {
                            let instantiated_config = expand_launch_config_template(
                                paths,
                                &state.config,
                                name,
                                instance,
                                count,
                            )
                            .unwrap(); // this can't fail because we ran check_config::check
                            launch_runner(&instantiated_config).await
                        }
                    }),
            )
            .await
            .into_iter()
            .collect()
        });
    }
    // Collect results from dispatch
    let launch_results: Vec<Vec<anyhow::Result<_>>> = join_all(queue.into_iter()).await;
    let mut successful = Vec::new();
    for ((name, _, jobs), result) in grouped_matched_jobs.iter().zip(launch_results.iter()) {
        let job_chunks = jobs.iter().chunks(group_size as usize);
        let (success, failure): (Vec<_>, Vec<_>) = job_chunks
            .into_iter()
            .zip(result.into_iter())
            .partition_map(|(job_chunk, result)| match result {
                Ok(_) => Either::Left(job_chunk),
                Err(e) => Either::Right((job_chunk, e)),
            });
        if success.len() > 0 {
            let success_vec = success.into_iter().flatten().collect();
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
                    jobs: &f.0.into_iter().collect()
                },
                f.1
            )
        }
    }
    // ignore any jobs that we couldn't find a runner for, but retry deferred jobs
    Ok(handled_job_ids(successful, &unmatched))
}

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
pub async fn run(paths: cli::Paths) -> anyhow::Result<()> {
    check_config::check(&paths).context(PermanentError("Invalid configuration".into()))?;
    let mut state = initialize_with_retry(&paths).await?;
    let cancel_token = CancellationToken::new();
    let job_cancel_token = cancel_token.clone();

    let task = tokio::spawn(async move {
        let poll_duration = Duration::from_secs(state.config.poll.interval as u64);
        let mut timer = time::interval(poll_duration);
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut access_failures = AccessFailureTracker::new(ACCESS_FAILURE_GRACE);
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
                Ok(Ok(new_successful_jobs)) => {
                    access_failures.record_success();
                    state
                        .successful_job_ids
                        .extend(new_successful_jobs.into_iter())
                }
                Ok(Err(e)) => {
                    if is_access_failure(&e) && access_failures.record_failure(Instant::now()) {
                        return Err(e.context(access_refused()));
                    }
                    error!("Failed poll: {:?}", e)
                }
                Err(_) => error!("Poll timed out"),
            };
        }
        Ok(())
    });

    let shutdown = async {
        match signal::ctrl_c().await {
            Ok(()) => info!("Received shutdown signal (Ctrl+C), cancelling poll task"),
            Err(_) => error!("Failed to listen for shutdown signal, shutting down anyways."),
        }
    };
    supervise(task, shutdown, cancel_token).await
}

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
pub async fn run_single(paths: &cli::Paths) -> anyhow::Result<()> {
    check_config::check(paths)?;
    let state = initialize(paths).await?;
    run_impl(paths, &state).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GitLabLaunchCountConfig;
    use crate::failure::{exit_code, PermanentError, EXIT_PERMANENT};
    use std::future::pending;
    use std::time::Instant;

    fn count_config(
        executable: &str,
        args: &[&str],
        timeout: Option<u32>,
    ) -> GitLabLaunchCountConfig {
        GitLabLaunchCountConfig {
            executable: executable.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            timeout,
        }
    }

    #[test]
    fn parse_count_output_ignores_blank_lines() {
        assert_eq!(parse_count_output("1\n\n2\n  \n"), 2);
        assert_eq!(parse_count_output(""), 0);
        assert_eq!(parse_count_output("631475\n"), 1);
    }

    #[tokio::test]
    async fn count_runs_command() {
        let count = count_config("sh", &["-c", "echo a; echo b; echo"], None);
        let result = count_active_launches(&count, None).await;
        assert_eq!(result.unwrap(), 2);
    }

    #[tokio::test]
    async fn count_fails_on_error_exit() {
        let count = count_config("false", &[], None);
        assert!(count_active_launches(&count, None).await.is_err());
    }

    #[tokio::test]
    async fn count_fails_on_missing_executable() {
        let count = count_config("/nonexistent/squeue", &[], None);
        assert!(count_active_launches(&count, None).await.is_err());
    }

    #[tokio::test]
    async fn count_times_out() {
        let count = count_config("sleep", &["5"], Some(1));
        let start = Instant::now();
        assert!(count_active_launches(&count, None).await.is_err());
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn deferred_jobs_not_handled() {
        let unmatched = vec![Job {
            id: 3,
            name: "status".into(),
            tags: vec!["status-jobs".into()],
        }];
        // job 4 was deferred: it is neither launched nor unmatched, so it must stay pending
        let handled = handled_job_ids(vec![1, 2], &unmatched);
        assert_eq!(handled, vec![1, 2, 3]);
        assert!(!handled.contains(&4));
    }

    #[tokio::test]
    async fn supervise_returns_task_error() {
        let task = tokio::spawn(async {
            Err(anyhow!("401 Unauthorized").context(PermanentError("token rejected".into())))
        });
        let result = supervise(task, pending::<()>(), CancellationToken::new()).await;
        assert_eq!(exit_code(&result.unwrap_err()), EXIT_PERMANENT);
    }

    #[tokio::test]
    async fn supervise_reports_panic() {
        let task = tokio::spawn(async {
            if true {
                panic!("bug in the poll task");
            }
            Ok(())
        });
        let result = supervise(task, pending::<()>(), CancellationToken::new()).await;
        assert_eq!(exit_code(&result.unwrap_err()), 1);
    }

    #[tokio::test]
    async fn supervise_reports_unexpected_end() {
        let task = tokio::spawn(async { Ok(()) });
        let result = supervise(task, pending::<()>(), CancellationToken::new()).await;
        assert_eq!(exit_code(&result.unwrap_err()), 1);
    }

    #[tokio::test]
    async fn supervise_cancels_on_shutdown() {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            task_cancel.cancelled().await;
            Ok(())
        });
        let result = supervise(task, async {}, cancel.clone()).await;
        assert!(result.is_ok(), "{:?}", result);
        assert!(cancel.is_cancelled());
    }
}
