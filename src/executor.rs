use anyhow::{anyhow, Context};
use log::{debug, info};
use std::{ffi::OsStr, fs, path::PathBuf, process::Stdio, process::Command};
use reqwest::blocking;
use serde_json::Value;
use chrono::{DateTime, Utc};

use serde_json::{json, to_string_pretty};

use crate::{
    cli,
    config::{read_config, GitLabCustomExecutorConfig, GitLabExecutorPullPolicy},
    template::expand_executor_config_template,
};

#[derive(Debug)]
struct JobEnv {
    job_id: String,
    builds_dir: PathBuf,
    image: String,
}

struct JobContext {
    runner_name: String,
    env: JobEnv,
    config: GitLabCustomExecutorConfig,
}

fn get_env_var(name: &str) -> anyhow::Result<String> {
    std::env::var(name).context(format!("Missing environment variable {}", name))
}

fn get_env() -> anyhow::Result<JobEnv> {
    // For reference: https://docs.gitlab.com/ee/ci/variables/predefined_variables.html
    Ok(JobEnv {
        job_id: get_env_var("CUSTOM_ENV_CI_JOB_ID")?,
        builds_dir: get_env_var("CUSTOM_ENV_CI_BUILDS_DIR")?.into(),
        image: get_env_var("CUSTOM_ENV_CI_JOB_IMAGE")?,
    })
}

fn config_step(context: &JobContext) -> anyhow::Result<()> {
    debug!(
        "Executing config step for job {} with runner {}",
        context.env.job_id, context.runner_name
    );
    let env = &context.env;
    // append job ID to builds_dir to make unique paths
    let config_obj = json!({
      "driver": {
        "name": match &context.config.description {
            None => format!("{} custom executor", env!("CARGO_PKG_NAME")),
            Some(description) => format!("{} custom executor ({})", env!("CARGO_PKG_NAME"), description)
        },
        "version": "v1.0.0"
      },
      "builds_dir_is_shared": false,
      "builds_dir": context.config.builds_dir.join(&env.job_id)
    });
    println!("{}", to_string_pretty(&config_obj).unwrap());
    debug!(
        "Reported configuration {}",
        to_string_pretty(&config_obj).unwrap()
    );
    Ok(())
}

// This is a reimplementation of apptainer's url.GetName function
fn build_image_filename(image_name: &str) -> PathBuf {
    let url_parts = image_name.split_once(":");
    let (protocol, path) = match url_parts {
        None => ("", image_name),
        Some(tuple) => tuple,
    };
    let base_name = path.split("/").last().unwrap();
    match protocol {
        "http" | "https" => return PathBuf::from(base_name),
        _ => (),
    };
    let mut parts = base_name.split(":");
    let name = parts.next().unwrap();
    let tag = parts
        .last()
        .map_or("latest", |s| s.split(",").next().unwrap());
    format!("{}_{}.sif", name, tag).into()
}

// This is derived from apptainer's pull.getImageNameFromURI function,
// with docker being the default if the image name is not an URI
fn build_image_pull_url(image_name: &str) -> String {
    let parts = image_name.split_once(":");
    // if the image name contains a valid URL (based on its protocol name), we use it directly
    // we add local:// here to avoid the local sif to become non-existed docker hub image.
    match parts {
        Some((protocol, _)) => match protocol {
            "library" | "shub" | "docker" | "docker-archive" | "docker-daemon" | "oci"
            | "oci-archive" | "http" | "https" | "oras" | "local" => return image_name.to_owned(),
            _ => (),
        },
        _ => (),
    }
    // otherwise we assume it is a docker image
    format!("docker://{}", image_name)
}

// This uses docker hub rest api to grab the time from last push.
fn get_docker_tag_timestamp(pull_url: &str) -> anyhow::Result<DateTime<Utc>> {
    if !pull_url.contains("docker://") {
        return Err(anyhow!("only accept docker but it is {}", pull_url));
    }
    // change the pull_url to the rest API url
    let resturl = pull_url
        .replace("docker://", "<tmp>")
        .replace(":", "/tags/")
        .replace("<tmp>", "https://hub.docker.com/v2/repositories/");

    debug!("Asking {} as rest api from {}", resturl, pull_url);

    // Use RestAPI
    let response = blocking::get(&resturl)?;
    let body: Value = response.json()?;

    // Extract and parse the "tag_last_pushed" field
    if let Some(value) = body["tag_last_pushed"].as_str() {
        let dt_docker: DateTime<Utc> = value.parse()?;
        debug!("Parsed docker push datetime {} from {}", dt_docker, value);
        return Ok(dt_docker);
    }

    return Err(anyhow!("json[\"tag_last_pushed\"] is not available"));
}

// This uses inspect to grab the time from build.
fn get_sif_build_timestamp(command: &PathBuf, sifpath: &PathBuf) -> anyhow::Result<DateTime<Utc>> {
    let output = Command::new(command)
        .arg("inspect")
        .arg("-j")
        .arg(sifpath)
        .output()
        .expect("Failed to execute command");

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: Value = serde_json::from_str(&stdout)?;
        if let Some(value) = json["data"]["attributes"]["labels"]["org.label-schema.build-date"].as_str() {
            // because CEST is not in chrono or chrono_tz, so use date to change it
            let utc = Command::new("date")
                .arg("-u")
                .arg("-d")
                .arg(value.replace("_", " "))
                .arg("+%Y-%m-%dT%H:%M:%SZ")
                .output()
                .expect("Failed to execute command");
            if utc.status.success() {
                let stdout = String::from_utf8_lossy(&utc.stdout).to_string();
                let dt_sif: DateTime<Utc> = stdout.parse()?;
                debug!("Parsed SIF build datetime {} from {}", dt_sif, stdout);
                return Ok(dt_sif);
            } else {
                let stderr = String::from_utf8_lossy(&utc.stderr).to_string();
                return Err(anyhow!(stderr));
            }
        }
        return Err(anyhow!("json[\"data\"][\"attributes\"][\"labels\"][\"org.label-schema.build-date\"] is not available"));
    }

    return Err(anyhow!("{} inspect failed", command.display()));
}

async fn prepare_step(context: &JobContext) -> anyhow::Result<()> {
    debug!(
        "Executing prepare step for job {} with runner {}",
        context.env.job_id, context.runner_name
    );
    let env = &context.env;
    let config = &context.config;
    let image = &env.image;
    let pull_url = build_image_pull_url(image);
    let filename = build_image_filename(image);
    let filepath = config.image_dir.join(&filename);

    // create directories if missing
    debug!(
        "Creating image directory if necessary {:?}",
        config.image_dir
    );
    std::fs::create_dir_all(&config.image_dir).context("Failed creating image_dir")?;
    debug!("Creating builds_dir {:?}", env.builds_dir);
    std::fs::create_dir_all(&env.builds_dir).context("Failed creating builds_dir")?;
    debug!("Creating cache_dir if necessary {:?}", config.cache_dir);
    std::fs::create_dir_all(&config.cache_dir).context("Failed creating cache_dir")?;
    if let Some(path) = &config.image_cache_dir {
        debug!("Creating image cache directory if necessary {:?}", path);
        std::fs::create_dir_all(&path).context(format!(
            "Creating image_cache_dir {:?}",
            config.image_cache_dir
        ))?;
    }
    if let Some(path) = &config.image_tmp_dir {
        debug!("Creating image tmp directory if necessary {:?}", path);
        std::fs::create_dir_all(&path).context(format!(
            "Failed creating image_tmp_dir {:?}",
            config.image_tmp_dir
        ))?;
    }

    let image_exists =
        std::fs::exists(&filepath).context("Failed checking for existence of image file")?;
    let mut dt_docker: Option<DateTime<Utc>> = None;
    if pull_url.contains("docker://") {
        dt_docker = Some(get_docker_tag_timestamp(&pull_url)?);
    }
    let mut dt_sif: Option<DateTime<Utc>> = None;
    if image_exists {
        dt_sif = Some(get_sif_build_timestamp(&config.apptainer_executable, &filepath)?);
    }
    let mut remote_is_newer = false;
    if let (Some(dt_docker_val), Some(dt_sif_val)) = (dt_docker, dt_sif) {
        remote_is_newer = dt_docker_val > dt_sif_val;
        info!("docker push time: {} and SIF build time: {}, so do we pull? {}", dt_docker_val, dt_sif_val, remote_is_newer);
    }

    let pull_needed = match config.pull_policy {
        GitLabExecutorPullPolicy::Always => true,
        GitLabExecutorPullPolicy::Never => {
            if !image_exists {
                Err(anyhow!(
                    "Pull policy is 'never', but image file doesn't exist!"
                ))?;
            };
            false
        }
        GitLabExecutorPullPolicy::IfNotPresent => !image_exists,
        GitLabExecutorPullPolicy::TryUpdate => !image_exists || remote_is_newer, 
    };
    info!("Using image {}", image);
    if !pull_needed {
        info!("No pull necessary");
        return Ok(());
    }

    // Pull if necessary
    // the temporary file is meant to prevent race conditions in image replacement
    let mut tmp_filename = filename.clone();
    tmp_filename.set_extension(format!("{}.tmp", env.job_id));
    let tmp_filepath = config.image_dir.join(&tmp_filename);
    debug!("Preparing image pull for {} to {:?}", pull_url, filename);
    // execute the pull process as a child with the same environment and output pipes
    let is_apptainer = config.apptainer_executable.ends_with("apptainer");
    let mut pull_command = async_process::Command::new(&config.apptainer_executable);
    pull_command
        .current_dir(config.image_dir.clone())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::null())
        .arg("pull")
        .arg(tmp_filename)
        .arg(pull_url.as_str());
    // set cache and image dir environment variables
    config.image_cache_dir.as_ref().map(|dir| {
        if is_apptainer {
            pull_command.env_remove("SINGULARITY_CACHEDIR");
            pull_command.env("APPTAINER_CACHEDIR", dir);
        } else {
            pull_command.env("SINGULARITY_CACHEDIR", dir);
        }
    });
    config.image_tmp_dir.as_ref().map(|dir| {
        if is_apptainer {
            pull_command.env_remove("SINGULARITY_TMPDIR");
            pull_command.env("APPTAINER_TMPDIR", dir);
        } else {
            pull_command.env("SINGULARITY_TMPDIR", dir);
        }
    });
    debug!("Pulling image with command {:?}", pull_command);
    // execute pull
    let mut pull_process = pull_command
        .spawn()
        .context("Failed creating pull process")?;
    let status = pull_process
        .status()
        .await
        .context("Failed awaiting pull process finish")?;
    if status.success() {
        debug!("Renaming {:?} to {:?}", tmp_filepath, filepath);
        // finally move temporary image to final position
        fs::rename(&tmp_filepath, &filepath)
            .context(format!("Renaming {:?} to {:?}", tmp_filepath, filepath))?;
        Ok(())
    } else {
        Err(anyhow!("Subprocess failed: {:?}", status))
    }
}

async fn run_step(
    context: &JobContext,
    script_path: &PathBuf,
    step_name: &str,
) -> anyhow::Result<()> {
    debug!(
        "Executing run step {} for job {} with runner {}",
        step_name, context.env.job_id, context.runner_name
    );
    let env = &context.env;
    let config = &context.config;
    let image = &env.image;
    let image_path = config.image_dir.join(build_image_filename(image));
    // mount script, builds and cache dir
    let binds: Vec<_> = [script_path, &env.builds_dir, &config.cache_dir]
        .iter()
        .map(|v| v.as_os_str().to_owned())
        .chain(config.mount.iter().map(|v| v.clone().into()))
        .collect();
    let bind_flags = binds
        .iter()
        .map(|mount| [OsStr::new("--bind"), &mount])
        .flatten();
    let mut run_command = async_process::Command::new(&config.apptainer_executable);
    run_command
        .current_dir(&env.builds_dir)
        .arg("exec")
        .arg("--no-home")
        .arg("--writable-tmpfs")
        .arg("--cleanenv")
        .args(bind_flags)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::null());
    // handle additional flags
    if config.gpu_amd {
        run_command.arg("--rocm");
    }
    if config.gpu_nvidia {
        run_command.arg("--nv");
    }
    // add positional arguments
    run_command
        .arg(image_path)
        .arg("bash")
        .arg("-l")
        .arg(script_path)
        .arg(step_name);
    debug!("Executing step with command {:?}", run_command);
    // execute process
    let mut run_process = run_command.spawn()?;
    let status = run_process.status().await?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("Subprocess failed: {:?}", status))
    }
}

fn cleanup_step(context: &JobContext) -> anyhow::Result<()> {
    debug!(
        "Executing cleanup step for job {} with runner {}",
        context.env.job_id, context.runner_name
    );
    debug!("Deleting builds_dir {:?}", context.env.builds_dir);
    std::fs::remove_dir_all(&context.env.builds_dir)?;
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 1)]
pub async fn exec(paths: &cli::Paths, options: &cli::ExecutorOptions) -> anyhow::Result<()> {
    debug!(
        "Starting executor with paths {:?} and options {:?}",
        paths, options
    );
    let full_config = read_config(&paths.config_file).context(format!(
        "Failed reading config file {:?}",
        paths.config_file
    ))?;
    debug!("Loaded config {:?}", full_config);
    let env = get_env().context("Failed parsing environment variables")?;
    debug!("Parsed environment {:?}", env);
    let runner_name = options.runner_name.clone();
    let instance = full_config
        .runners
        .get(&runner_name)
        .ok_or(anyhow!("Unknown runner instance {}", runner_name))?;
    debug!("Runner instance {:?}", instance);
    let config = expand_executor_config_template(&full_config, &runner_name, &instance)
        .context("Failed expanding executor config template")?;
    debug!("Instance config {:?}", config);
    let context = JobContext {
        runner_name,
        env,
        config,
    };
    match &options.command {
        cli::ExecutorCommand::Config => config_step(&context),
        cli::ExecutorCommand::Prepare => prepare_step(&context).await,
        cli::ExecutorCommand::Run {
            script_name,
            step_name,
        } => run_step(&context, script_name, step_name).await,
        cli::ExecutorCommand::Cleanup => cleanup_step(&context),
    }
}
