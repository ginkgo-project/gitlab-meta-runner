use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap},
};

use crate::{config::GitLabRunnerInstance, gitlab_wrap::Job};

/// Pending jobs split by the runner instance that should launch them
pub struct Assignment<'a> {
    /// Jobs per runner instance, sorted by runner name
    pub assigned: Vec<(&'a String, &'a GitLabRunnerInstance, Vec<Job>)>,
    /// Jobs with matching runners that are all at their launch limit, to be retried next poll
    pub deferred: Vec<Job>,
    /// Jobs without any matching runner
    pub unmatched: Vec<Job>,
}

/// All runner instances whose tags include all tags of the job, in the order in which they should be tried:
/// higher match_priority first (no priority last), then fewer tags, then runner name.
pub fn candidates<'a>(
    runners: &'a HashMap<String, GitLabRunnerInstance>,
    job: &Job,
) -> Vec<(&'a String, &'a GitLabRunnerInstance)> {
    let mut result: Vec<_> = runners
        .iter()
        .filter(|(_, instance)| job.tags.iter().all(|tag| instance.tags.contains(tag)))
        .collect();
    result.sort_by(|(name_a, a), (name_b, b)| {
        (Reverse(a.match_priority), a.tags.len(), name_a).cmp(&(
            Reverse(b.match_priority),
            b.tags.len(),
            name_b,
        ))
    });
    result
}

/// Number of jobs a runner instance can still take, given its launch limit and currently active launches
pub fn free_slots(max_launches: usize, active: usize, group_size: usize) -> usize {
    max_launches.saturating_sub(active) * group_size
}

/// Assigns each job, oldest first, to the first candidate runner instance with a free slot.
/// Runner instances missing from `free` have no limit.
pub fn assign_jobs<'a>(
    runners: &'a HashMap<String, GitLabRunnerInstance>,
    mut jobs: Vec<Job>,
    free: &HashMap<String, usize>,
) -> Assignment<'a> {
    jobs.sort_by_key(|job| job.id);
    let mut remaining = free.clone();
    let mut assigned: BTreeMap<&'a String, (&'a GitLabRunnerInstance, Vec<Job>)> = BTreeMap::new();
    let mut deferred = Vec::new();
    let mut unmatched = Vec::new();
    for job in jobs {
        let job_candidates = candidates(runners, &job);
        if job_candidates.is_empty() {
            unmatched.push(job);
            continue;
        }
        let chosen = job_candidates.into_iter().find(|(name, _)| {
            remaining
                .get(name.as_str())
                .map_or(true, |slots| *slots > 0)
        });
        match chosen {
            Some((name, instance)) => {
                if let Some(slots) = remaining.get_mut(name.as_str()) {
                    *slots -= 1;
                }
                assigned
                    .entry(name)
                    .or_insert_with(|| (instance, Vec::new()))
                    .1
                    .push(job);
            }
            None => deferred.push(job),
        }
    }
    Assignment {
        assigned: assigned
            .into_iter()
            .map(|(name, (instance, jobs))| (name, instance, jobs))
            .collect(),
        deferred,
        unmatched,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn runner(tags: &[&str], match_priority: Option<u32>) -> GitLabRunnerInstance {
        GitLabRunnerInstance {
            tags: tags.iter().map(|t| t.to_string()).collect(),
            launch_priority: None,
            match_priority,
            max_launches: None,
            config_variables: HashMap::new(),
        }
    }

    fn runners(list: Vec<(&str, GitLabRunnerInstance)>) -> HashMap<String, GitLabRunnerInstance> {
        list.into_iter()
            .map(|(name, instance)| (name.to_owned(), instance))
            .collect()
    }

    fn job(id: u64, tags: &[&str]) -> Job {
        Job {
            id,
            name: format!("job-{}", id),
            tags: tags.iter().map(|t| t.to_string()).collect(),
        }
    }

    fn names(candidates: Vec<(&String, &GitLabRunnerInstance)>) -> Vec<String> {
        candidates
            .into_iter()
            .map(|(name, _)| name.clone())
            .collect()
    }

    fn free(list: &[(&str, usize)]) -> HashMap<String, usize> {
        list.iter()
            .map(|(name, n)| (name.to_string(), *n))
            .collect()
    }

    fn ids(jobs: &[Job]) -> Vec<u64> {
        jobs.iter().map(|job| job.id).collect()
    }

    fn assigned_ids(assignment: &Assignment, runner_name: &str) -> Vec<u64> {
        assignment
            .assigned
            .iter()
            .find(|(name, _, _)| name.as_str() == runner_name)
            .map_or(Vec::new(), |(_, _, jobs)| ids(jobs))
    }

    fn cuda_runners() -> HashMap<String, GitLabRunnerInstance> {
        runners(vec![
            (
                "nvidia_l40s",
                runner(&["nvidia-gpus", "nvidia-gpus-l40s", "tum"], Some(10)),
            ),
            (
                "nvidia_rocinante",
                runner(&["nvidia-gpus", "nvidia-gpus-rocinante", "tum"], None),
            ),
        ])
    }

    #[test]
    fn fewest_tags_wins_without_priority() {
        let r = runners(vec![
            (
                "specific",
                runner(&["nvidia-gpus", "nvidia-gpus-l40s", "tum"], None),
            ),
            ("generic", runner(&["nvidia-gpus", "tum"], None)),
            ("other", runner(&["cpu", "tum"], None)),
        ]);
        let result = candidates(&r, &job(1, &["nvidia-gpus", "tum"]));
        assert_eq!(names(result), vec!["generic", "specific"]);
    }

    #[test]
    fn ties_broken_by_name() {
        // HashMap iteration order is randomized per map, so build a fresh map every time
        for _ in 0..20 {
            let r = runners(vec![
                (
                    "amd_mi50",
                    runner(&["amd-gpus", "amd-gpus-mi50", "tum"], None),
                ),
                (
                    "amd_mi210",
                    runner(&["amd-gpus", "amd-gpus-mi210", "tum"], None),
                ),
            ]);
            let result = candidates(&r, &job(1, &["amd-gpus", "tum"]));
            assert_eq!(names(result), vec!["amd_mi210", "amd_mi50"]);
        }
    }

    #[test]
    fn match_priority_beats_tag_count() {
        let r = runners(vec![
            ("rocinante", runner(&["nvidia-gpus", "tum"], None)),
            (
                "l40s",
                runner(&["nvidia-gpus", "nvidia-gpus-l40s", "tum"], Some(10)),
            ),
            ("low", runner(&["nvidia-gpus", "x", "y", "tum"], Some(1))),
        ]);
        let result = candidates(&r, &job(1, &["nvidia-gpus", "tum"]));
        assert_eq!(names(result), vec!["l40s", "low", "rocinante"]);
    }

    #[test]
    fn overflow_to_next_candidate() {
        let r = cuda_runners();
        // pending jobs arrive newest first, the oldest ones should get the preferred runner
        let jobs = (1..=8)
            .rev()
            .map(|id| job(id, &["nvidia-gpus", "tum"]))
            .collect();
        let assignment = assign_jobs(
            &r,
            jobs,
            &free(&[("nvidia_l40s", 4), ("nvidia_rocinante", 2)]),
        );
        assert_eq!(assigned_ids(&assignment, "nvidia_l40s"), vec![1, 2, 3, 4]);
        assert_eq!(assigned_ids(&assignment, "nvidia_rocinante"), vec![5, 6]);
        assert_eq!(ids(&assignment.deferred), vec![7, 8]);
        assert!(assignment.unmatched.is_empty());
    }

    #[test]
    fn full_runner_skipped() {
        let r = cuda_runners();
        let jobs = vec![
            job(1, &["nvidia-gpus", "tum"]),
            job(2, &["nvidia-gpus", "tum"]),
        ];
        let assignment = assign_jobs(
            &r,
            jobs,
            &free(&[("nvidia_l40s", 0), ("nvidia_rocinante", 2)]),
        );
        assert_eq!(assigned_ids(&assignment, "nvidia_l40s"), Vec::<u64>::new());
        assert_eq!(assigned_ids(&assignment, "nvidia_rocinante"), vec![1, 2]);
        assert!(assignment.deferred.is_empty());
    }

    #[test]
    fn unlimited_runner_takes_all() {
        let r = cuda_runners();
        let jobs = (1..=10)
            .map(|id| job(id, &["nvidia-gpus", "tum"]))
            .collect();
        let assignment = assign_jobs(&r, jobs, &HashMap::new());
        assert_eq!(
            assigned_ids(&assignment, "nvidia_l40s"),
            (1..=10).collect::<Vec<_>>()
        );
        assert!(assignment.deferred.is_empty());
    }

    #[test]
    fn unmatched_jobs_reported() {
        let r = cuda_runners();
        let jobs = vec![job(1, &["status-jobs"]), job(2, &["nvidia-gpus", "tum"])];
        let assignment = assign_jobs(
            &r,
            jobs,
            &free(&[("nvidia_l40s", 0), ("nvidia_rocinante", 0)]),
        );
        assert_eq!(ids(&assignment.unmatched), vec![1]);
        assert_eq!(ids(&assignment.deferred), vec![2]);
    }

    #[test]
    fn free_slots_saturates() {
        assert_eq!(free_slots(2, 5, 2), 0);
        assert_eq!(free_slots(2, 1, 2), 2);
        assert_eq!(free_slots(0, 0, 2), 0);
        assert_eq!(free_slots(3, 0, 1), 3);
    }
}
