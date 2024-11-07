This project is meant to bridge between the requirements of GitLab CI pipelines and the restrictions of an HPC cluster.

It consists of the following components:

- **Template instantiation:** The config file contains a list of named runner instances, and configuration section templates for `gitlab-runner`, a custom executor and the actual meta-runner functionality, which will be instantiated for each runner instance.
  Each runner instance can define a set of variables, which will be used (together with predefined variables and environment variables) to expand all variables `$VARIABLE` in these configuration values.
- **Runner management:** Use an API token to register, update and delete runners with the GitLab API. The obtained runner IDs and access tokens will be stored locally.
- **Runner configuration:** Create a `gitlab-runner.toml` configuration file containing the instantiated configuration for all gitlab-runner instances.
- **Custom executor:** Provide a custom executor for the `apptainer`/`singularity` HPC container runtime, which mirrors the behavior of the `docker` executor, pulling an image from a container registry and executing `gitlab-runner` job steps inside the container.
- **Runner launch:** Regularly poll the list of pending jobs for a GitLab project. For each job, attempt to match its tags against the tags of your runner instances. If you have a matching instance, run a custom command (e.g. `sbatch`) to dispatch a `gitlab-runner run-single` command to your HPC system's batch queue.

## Compiling the project

With the latest stable Rust version, run `cargo build` inside the cloned repository.

## Example configuration

Running `gitlab-meta-runner show-example-config` produces the following documented configuration:

```toml
# Unique name for the meta-runner
name = "meta-runner"
# GitLab Project name for the meta-runner
project = "gitlab-org/gitlab"
# GitLab hostname for the meta-runner
hostname = "gitlab.com"
# GitLab project token with read_api, create_runner, manage_runner permissions
management_token = "enter-your-token-here"

[runners.test-runner]
# Tags whose associated jobs will be run by this runner
tags = [
    "tag-1",
    "tag-2",
]
# Priority in which the instances' launch processes should be executed, higher priority means earlier launch.
# All jobs without a priority will be launched last.
launch_priority = 10

# Variables to be expanded in the template instantiation.
# Each value needs to be a string!
[runners.test-runner.config_variables]
VARIABLE = "value"

# Configuration for polling for new jobs
[poll]
# Interval (in seconds) for polling for new jobs
interval = 30

# Configuration for launching ephemeral runners
# Some of the configuration variables allow variable expansion from the runner instance variables
# Available variables are (in order of precedence)
# - $NAME for the runner instance name, to be passed to `gitlab-runner run-single --runner-name $NAME``
# - $THIS for the path to this executable
# - $CONFIG for the path to the generated gitlab-runner config file, to be passed to `gitlab-runner --config $CONFIG`
# - $NUM_JOBS for the number of jobs that were grouped together for this launch, to be passed to `gitlab-runner run-single --max-builds 1`
# - Any variables defined in runners.<runner_name>.config_variables
# - Any environment variables provided by gitlab-runner to this custom executor
[launch]
# Executable name or path, will be variable-expanded
executable = "sbatch"
# Arguments to pass to the executable, they will be variable-expanded
args = []
# Working directory for the executable, this will be variable-expanded
workdir = "$HOME/launch"
# The input to pass to the executable via stdin, this will be variable-expanded
stdin = """
#!/bin/bash
gitlab-runner run-single --config $CONFIG --runner $NAME --max-builds $NUM_JOBS --wait-timeout 1
"""
# The time to wait (in seconds) for each launch command to finish, will NOT be variable-expanded
timeout = 300
# The number of jobs to launch in a single launch command, will NOT be variable-expanded
group_size = 1

# Configuration for the custom executor
# Some of the configuration variables allow variable expansion from the runner instance variables
# Available variables are (in order of precedence)
# - $NAME for the runner instance name
# - $THIS for the path to this executable
# - Any variables defined in runners.<runner_name>.config_variables
# - Any environment variables provided by gitlab-runner to this custom executor
[executor]
# Override builds_dir provided by gitlab-runner config, will be variable-expanded
builds_dir = "$HOME/builds"
# Path to store the image files in, will be variable-expanded
image_dir = "$HOME/images"
# Path to use for caching image layers, will be variable-expanded
image_cache_dir = "$HOME/image_cache"
# Path to use for temporary files during pull, will be variable-expanded
image_tmp_dir = "$HOME/image_tmp"
# Pull policy to use for images, will NOT be variable-expanded
pull_policy = "if-not-present"
# Path to the apptainer executable (may be relative to workdir or $PATH), will be variable-expanded
apptainer_executable = "apptainer"
# Mount AMD GPU devices, will be variable-expanded
gpu_amd = false
# Mount NVIDIA GPU devices, will be variable-expanded
gpu_nvidia = false
# Additional bind mounts to use in the container, every individual entry will be variable-expanded
mount = []

# Configuration template for gitlab-runner config file
# It will be instantiated for every runner in the runners array,
# expanding occurrences of the runner instance variables into their values
# Available variables are (in order of precedence)
# - $NAME for the runner instance name
# - $THIS for the path to this executable
# - Any variables defined in runners.<runner_name>.config_variables
# - Any environment variables available when calling `gitlab-meta-runner (configure|show-config)`
[runner]
# Directory to use for builds, will be variable-expanded
builds_dir = "$HOME/builds/$NAME/"
# Directory to use for build caches, will be variable-expanded
cache_dir = "$HOME/cache/"
# The executor to use for this runner
executor = "custom"
# Additional environment variables, will be variable-expanded
environment = ["ENV_VARIABLE=value"]

[runner.custom]
# The executable to configure a job, will be template-expanded
config_exec = "$THIS"
# The arguments to pass to config_exec, will be template-expanded
config_args = [
    "executor",
    "$NAME",
    "config",
]
# The executable to prepare a job, will be template-expanded
prepare_exec = "$THIS"
# The arguments to pass to prepare_exec, will be template-expanded
prepare_args = [
    "executor",
    "$NAME",
    "prepare",
]
# The executable to run a job, will be template-expanded
run_exec = "$THIS"
# The arguments to pass to run_exec, will be template-expanded
run_args = [
    "executor",
    "$NAME",
    "run",
]
# The executable to execute to clean up after a job, will be template-expanded
cleanup_exec = "$THIS"
# The arguments to pass to cleanup_exec, will be template-expanded
cleanup_args = [
    "executor",
    "$NAME",
    "cleanup",
]
```
