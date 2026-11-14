use std::{
    fmt::Display,
    time::{Duration, Instant},
};

use gitlab::{api::ApiError, GitlabError, RestError};
use http::StatusCode;

/// Exit code for failures that a restart can't fix (EX_CONFIG), so systemd can stop restarting the service
pub const EXIT_PERMANENT: i32 = 78;

/// How long GitLab may keep rejecting the management token before the meta-runner gives up
pub const ACCESS_FAILURE_GRACE: Duration = Duration::from_secs(10 * 60);

const FIRST_RETRY_DELAY: Duration = Duration::from_secs(10);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5 * 60);

/// Marks an error as permanent: restarting the meta-runner won't fix it
#[derive(Debug)]
pub struct PermanentError(pub String);

impl Display for PermanentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for PermanentError {}

/// Process exit code for an error: EXIT_PERMANENT if it was marked permanent anywhere in its chain, 1 otherwise
pub fn exit_code(err: &anyhow::Error) -> i32 {
    if err.downcast_ref::<PermanentError>().is_some() {
        EXIT_PERMANENT
    } else {
        1
    }
}

fn api_status(err: &ApiError<RestError>) -> Option<StatusCode> {
    match err {
        ApiError::GitlabWithStatus { status, .. }
        | ApiError::GitlabObjectWithStatus { status, .. }
        | ApiError::GitlabUnrecognizedWithStatus { status, .. }
        | ApiError::GitlabService { status, .. } => Some(*status),
        _ => None,
    }
}

/// The HTTP status of the first GitLab API error in the error chain, if any
pub fn http_status(err: &anyhow::Error) -> Option<StatusCode> {
    err.chain().find_map(|cause| {
        if let Some(api) = cause.downcast_ref::<ApiError<RestError>>() {
            return api_status(api);
        }
        match cause.downcast_ref::<GitlabError>() {
            Some(GitlabError::Http { status }) => Some(*status),
            _ => None,
        }
    })
}

/// Whether GitLab refused access: token invalid or revoked (401), missing permissions (403) or no access to the project (404)
pub fn is_access_failure(err: &anyhow::Error) -> bool {
    matches!(
        http_status(err),
        Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND)
    )
}

/// Delay before the given retry attempt (starting at 0): 10 s, doubling up to 5 minutes
pub fn retry_delay(attempt: u32) -> Duration {
    FIRST_RETRY_DELAY
        .checked_mul(2u32.saturating_pow(attempt))
        .map_or(MAX_RETRY_DELAY, |delay| delay.min(MAX_RETRY_DELAY))
}

/// Tracks how long GitLab has been refusing access without any successful request in between
pub struct AccessFailureTracker {
    grace: Duration,
    since: Option<Instant>,
}

impl AccessFailureTracker {
    pub fn new(grace: Duration) -> Self {
        AccessFailureTracker { grace, since: None }
    }

    /// Records an access failure at `now`, returns true once access has been failing for at least the grace period
    pub fn record_failure(&mut self, now: Instant) -> bool {
        let since = *self.since.get_or_insert(now);
        now.saturating_duration_since(since) >= self.grace
    }

    /// Records a successful request, which ends the current failure period
    pub fn record_success(&mut self) {
        self.since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use gitlab::{api::ApiError, RestError};
    use http::StatusCode;

    fn api_error(status: StatusCode) -> anyhow::Error {
        anyhow::Error::new(ApiError::<RestError>::GitlabWithStatus {
            status,
            msg: "invalid_token".into(),
        })
    }

    #[test]
    fn retry_delay_doubles_and_caps() {
        let delays: Vec<u64> = (0..7)
            .map(|attempt| retry_delay(attempt).as_secs())
            .collect();
        assert_eq!(delays, vec![10, 20, 40, 80, 160, 300, 300]);
        assert_eq!(retry_delay(100).as_secs(), 300);
    }

    #[test]
    fn tracker_trips_after_grace() {
        let mut tracker = AccessFailureTracker::new(Duration::from_secs(600));
        let t0 = Instant::now();
        assert!(!tracker.record_failure(t0));
        assert!(!tracker.record_failure(t0 + Duration::from_secs(9 * 60)));
        assert!(tracker.record_failure(t0 + Duration::from_secs(10 * 60)));
    }

    #[test]
    fn tracker_resets_on_success() {
        let mut tracker = AccessFailureTracker::new(Duration::from_secs(600));
        let t0 = Instant::now();
        assert!(!tracker.record_failure(t0));
        tracker.record_success();
        // the grace period starts again with the next failure
        assert!(!tracker.record_failure(t0 + Duration::from_secs(11 * 60)));
        assert!(tracker.record_failure(t0 + Duration::from_secs(21 * 60)));
    }

    #[test]
    fn access_failures_classified() {
        assert!(is_access_failure(&api_error(StatusCode::UNAUTHORIZED)));
        assert!(is_access_failure(&api_error(StatusCode::FORBIDDEN)));
        assert!(is_access_failure(&api_error(StatusCode::NOT_FOUND)));
        assert!(!is_access_failure(&api_error(
            StatusCode::INTERNAL_SERVER_ERROR
        )));
        assert!(!is_access_failure(&anyhow!(
            "dns error: failed to lookup address"
        )));
        let wrapped = api_error(StatusCode::UNAUTHORIZED)
            .context("Failed fetching pending jobs")
            .context("Failed poll");
        assert!(is_access_failure(&wrapped));
        assert_eq!(http_status(&wrapped), Some(StatusCode::UNAUTHORIZED));
    }

    #[test]
    fn startup_auth_failure_classified() {
        // the client checks the token on startup, a 401 there arrives wrapped in GitlabError::Api
        let err = anyhow::Error::new(GitlabError::Api {
            source: ApiError::<RestError>::GitlabWithStatus {
                status: StatusCode::UNAUTHORIZED,
                msg: "invalid_token".into(),
            },
        })
        .context("Failed configuring GitLab API client");
        assert!(is_access_failure(&err));
    }

    #[test]
    fn exit_code_permanent_through_context() {
        let permanent = api_error(StatusCode::UNAUTHORIZED)
            .context(PermanentError(
                "GitLab rejected the management token".into(),
            ))
            .context("Poll task failed");
        assert_eq!(exit_code(&permanent), EXIT_PERMANENT);
        assert_eq!(exit_code(&anyhow!("Poll task panicked")), 1);
    }
}
