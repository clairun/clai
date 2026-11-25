//! Retrying a provider request that failed with a [`FailureKind::Transient`]
//! status: 1 attempt plus up to 2 retries, after 2 s and then 8 s, or after
//! the provider's `Retry-After` when that is longer.

use std::future::Future;
use std::time::Duration;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use super::failure::FailureKind;
use super::types::{ProviderError, ProviderStream};
use crate::assistant::types::ProviderEvent;

const BACKOFF: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(8)];
/// The initial attempt plus one retry per backoff step.
pub const MAX_ATTEMPTS: u32 = BACKOFF.len() as u32 + 1;
/// A longer `Retry-After` is a limit, not a blip: the request fails at once.
const MAX_DELAY: Duration = Duration::from_secs(60);

/// A retry about to happen, reported before its delay starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryNotice {
    /// The attempt that runs after the delay: 2 or 3.
    pub attempt: u32,
    pub delay: Duration,
}

/// How a retried request is stopped and reported.
pub struct RetryHooks<'a> {
    pub cancel: &'a CancellationToken,
    pub on_retry: Box<dyn Fn(RetryNotice) + Send + Sync + 'a>,
}

impl<'a> RetryHooks<'a> {
    /// Hooks that report nothing, for requests no run can show.
    pub fn silent(cancel: &'a CancellationToken) -> Self {
        Self {
            cancel,
            on_retry: Box::new(|_| {}),
        }
    }
}

#[derive(Debug)]
pub enum RetryStop {
    Failed(ProviderError),
    /// The cancel token fired before or during a request, or during a delay.
    Cancelled,
}

/// Runs `request` until it succeeds, fails for good, or is cancelled.
pub async fn retry_transient<T, F, Fut>(
    hooks: &RetryHooks<'_>,
    mut request: F,
) -> Result<T, RetryStop>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>>,
{
    for (attempt, backoff) in (2..).zip(BACKOFF) {
        let error = match unless_cancelled(hooks.cancel, request()).await? {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let delay = retry_delay(backoff, error).map_err(RetryStop::Failed)?;
        (hooks.on_retry)(RetryNotice { attempt, delay });
        unless_cancelled(hooks.cancel, tokio::time::sleep(delay)).await?;
    }
    unless_cancelled(hooks.cancel, request())
        .await?
        .map_err(RetryStop::Failed)
}

/// Awaits `work` unless `cancel` fires first; a fired token wins without
/// polling `work` at all.
async fn unless_cancelled<T>(
    cancel: &CancellationToken,
    work: impl Future<Output = T>,
) -> Result<T, RetryStop> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(RetryStop::Cancelled),
        output = work => Ok(output),
    }
}

/// The wait before retrying `error`, or the error to fail with.
fn retry_delay(backoff: Duration, error: ProviderError) -> Result<Duration, ProviderError> {
    let ProviderError::Failed(mut failure) = error else {
        return Err(error);
    };
    if failure.kind != FailureKind::Transient {
        return Err(ProviderError::Failed(failure));
    }
    match failure.retry_after {
        Some(wait) if wait > MAX_DELAY => {
            let note = format!("The provider asked to wait {}.", human_duration(wait));
            failure.message = match failure.message.trim_end() {
                "" => note,
                text if text.ends_with(['.', '!', '?']) => format!("{text} {note}"),
                text => format!("{text}. {note}"),
            };
            Err(ProviderError::Failed(failure))
        }
        wait => Ok(backoff.max(wait.unwrap_or_default())),
    }
}

/// Reads an opened `stream` up to its first output, and returns the stream
/// with the events read put back in front. An error the provider reports
/// before that output fails the request as an HTTP error would, so
/// [`retry_transient`] and the context-limit recovery act on its kind;
/// nothing was shown yet, so a retry repeats nothing.
pub async fn until_first_output(stream: ProviderStream) -> Result<ProviderStream, ProviderError> {
    let mut stream = stream.fuse();
    let mut read = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(ProviderEvent::ProviderError { failure }) => {
                return Err(ProviderError::Failed(failure));
            }
            Ok(event) if !is_output(&event) => read.push(Ok(event)),
            item => {
                read.push(item);
                break;
            }
        }
    }
    Ok(Box::pin(futures::stream::iter(read).chain(stream)))
}

/// Events the turn shows or keeps: text, thinking and tool calls.
fn is_output(event: &ProviderEvent) -> bool {
    matches!(
        event,
        ProviderEvent::TextDelta { .. }
            | ProviderEvent::ThinkingDelta { .. }
            | ProviderEvent::ToolCallReady { .. }
    )
}

/// `5400 s` as "1 h 30 min".
fn human_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    [
        (secs / 3600, "h"),
        (secs % 3600 / 60, "min"),
        (secs % 60, "s"),
    ]
    .iter()
    .filter(|(value, _)| *value > 0)
    .map(|(value, unit)| format!("{value} {unit}"))
    .collect::<Vec<_>>()
    .join(" ")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::*;
    use crate::assistant::providers::failure::ProviderFailure;

    fn failure(kind: FailureKind, retry_after: Option<u64>) -> ProviderError {
        ProviderError::Failed(ProviderFailure {
            kind,
            status: Some(503),
            provider_code: None,
            message: "overloaded".into(),
            retry_after: retry_after.map(Duration::from_secs),
        })
    }

    fn transient() -> ProviderError {
        failure(FailureKind::Transient, None)
    }

    /// Runs `retry_transient` against a request that takes `latency`, fails
    /// with `errors` in order, then succeeds. Returns the outcome, the number
    /// of requests started, the notices and the virtual time spent.
    async fn run(
        errors: Vec<ProviderError>,
        latency: Duration,
        cancel: &CancellationToken,
    ) -> (
        Result<&'static str, RetryStop>,
        usize,
        Vec<RetryNotice>,
        Duration,
    ) {
        let notices = Mutex::new(Vec::new());
        let hooks = RetryHooks {
            cancel,
            on_retry: Box::new(|notice| notices.lock().unwrap().push(notice)),
        };
        let mut errors = errors.into_iter();
        let requests = AtomicUsize::new(0);
        let started = tokio::time::Instant::now();
        let outcome = retry_transient(&hooks, || {
            let next = errors.next();
            let requests = &requests;
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(latency).await;
                next.map_or(Ok("ok"), Err)
            }
        })
        .await;
        let elapsed = started.elapsed();
        drop(hooks);
        (
            outcome,
            requests.into_inner(),
            notices.into_inner().unwrap(),
            elapsed,
        )
    }

    fn cancel_after(secs: u64) -> CancellationToken {
        let cancel = CancellationToken::new();
        let stopper = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(secs)).await;
            stopper.cancel();
        });
        cancel
    }

    fn notice(attempt: u32, secs: u64) -> RetryNotice {
        RetryNotice {
            attempt,
            delay: Duration::from_secs(secs),
        }
    }

    fn failed_message(outcome: Result<&str, RetryStop>) -> String {
        match outcome {
            Err(RetryStop::Failed(ProviderError::Failed(failure))) => failure.message,
            other => panic!("expected a provider failure, got {other:?}"),
        }
    }

    /// Text, thinking and a ready tool call each end the read: an error
    /// after them reaches the caller as an event. Anything else is read past.
    #[tokio::test]
    async fn until_first_output_stops_at_text_thinking_or_a_tool_call() {
        use crate::assistant::types::ToolInvocationDraft;
        let error = || ProviderEvent::ProviderError {
            failure: ProviderFailure::in_stream(FailureKind::Transient, "overloaded".into()),
        };
        let outputs = [
            ProviderEvent::TextDelta { text: "a".into() },
            ProviderEvent::ThinkingDelta { text: "b".into() },
            ProviderEvent::ToolCallReady {
                tool_call: ToolInvocationDraft {
                    tool_call_id: "call-1".into(),
                    tool_name: "t".into(),
                    params: serde_json::json!({}),
                    thought_signature: None,
                },
            },
        ];
        for output in outputs {
            let label = format!("{output:?}");
            let stream: ProviderStream = Box::pin(futures::stream::iter(
                [ProviderEvent::MessageStart, output, error()].map(Ok),
            ));
            let replayed: Vec<_> = until_first_output(stream)
                .await
                .unwrap_or_else(|error| panic!("{label}: {error}"))
                .collect()
                .await;
            assert_eq!(replayed.len(), 3, "{label}");
        }

        let silent: ProviderStream = Box::pin(futures::stream::iter(
            [
                ProviderEvent::MessageStart,
                ProviderEvent::ToolCallDelta {
                    tool_call_id: "call-1".into(),
                    partial: serde_json::json!({}),
                },
                error(),
            ]
            .map(Ok),
        ));
        let failed = until_first_output(silent).await.err();
        assert_eq!(
            failed.and_then(|error| error.kind()),
            Some(FailureKind::Transient)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn retry_transient_then_success() {
        let (outcome, requests, notices, elapsed) =
            run(vec![transient()], Duration::ZERO, &CancellationToken::new()).await;
        assert_eq!(outcome.unwrap(), "ok");
        assert_eq!(requests, 2);
        assert_eq!(notices, [notice(2, 2)]);
        assert_eq!(elapsed, Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_three_transient_failures_end_in_the_last_error() {
        let mut last = failure(FailureKind::Transient, None);
        if let ProviderError::Failed(failure) = &mut last {
            failure.message = "third".into();
        }
        let (outcome, requests, notices, elapsed) = run(
            vec![transient(), transient(), last, transient()],
            Duration::ZERO,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(failed_message(outcome), "third");
        assert_eq!(requests, MAX_ATTEMPTS as usize);
        assert_eq!(notices, [notice(2, 2), notice(3, 8)]);
        assert_eq!(elapsed, Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_skips_every_kind_but_transient() {
        for kind in FailureKind::ALL {
            if kind == FailureKind::Transient {
                continue;
            }
            let (outcome, requests, notices, _) = run(
                vec![failure(kind, None)],
                Duration::ZERO,
                &CancellationToken::new(),
            )
            .await;
            assert!(
                matches!(outcome, Err(RetryStop::Failed(ref e)) if e.kind() == Some(kind)),
                "{kind:?}"
            );
            assert_eq!((requests, notices.len()), (1, 0), "{kind:?}");
        }
        let unclassified = ProviderError::RequestFailed("connection reset".into());
        let (outcome, requests, _, _) = run(
            vec![unclassified],
            Duration::ZERO,
            &CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            outcome,
            Err(RetryStop::Failed(ProviderError::RequestFailed(_)))
        ));
        assert_eq!(requests, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_over_a_minute_fails_at_once_and_names_the_wait() {
        let (outcome, requests, notices, elapsed) = run(
            vec![failure(FailureKind::Transient, Some(86_400))],
            Duration::ZERO,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(
            failed_message(outcome),
            "overloaded. The provider asked to wait 24 h."
        );
        assert_eq!((requests, notices.len()), (1, 0));
        assert_eq!(elapsed, Duration::ZERO);
    }

    #[test]
    fn a_long_wait_is_appended_as_its_own_sentence() {
        let message = |text: &str, secs: u64| {
            let error = ProviderError::Failed(ProviderFailure {
                kind: FailureKind::Transient,
                status: Some(429),
                provider_code: None,
                message: text.into(),
                retry_after: Some(Duration::from_secs(secs)),
            });
            match retry_delay(BACKOFF[0], error) {
                Err(ProviderError::Failed(failure)) => failure.message,
                other => panic!("expected a failure, got {other:?}"),
            }
        };
        assert_eq!(
            message("Rate limit exceeded", 86_400),
            "Rate limit exceeded. The provider asked to wait 24 h."
        );
        assert_eq!(
            message("Slow down. ", 5_400),
            "Slow down. The provider asked to wait 1 h 30 min."
        );
        assert_eq!(message("", 61), "The provider asked to wait 1 min 1 s.");
    }

    #[test]
    fn retry_delay_is_the_longer_of_backoff_and_retry_after() {
        let delay = |backoff: u64, retry_after: Option<u64>| {
            retry_delay(
                Duration::from_secs(backoff),
                failure(FailureKind::Transient, retry_after),
            )
            .map(|delay| delay.as_secs())
            .map_err(|e| e.to_string())
        };
        assert_eq!(delay(2, None), Ok(2));
        assert_eq!(delay(8, Some(3)), Ok(8));
        assert_eq!(delay(2, Some(5)), Ok(5));
        assert_eq!(delay(8, Some(60)), Ok(60));
        assert!(delay(2, Some(61)).is_err());
        assert!(delay(2, Some(3600)).unwrap_err().contains("wait 1 h."));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_cancelled_during_backoff_stops_without_another_request() {
        let (outcome, requests, notices, elapsed) = run(
            vec![failure(FailureKind::Transient, Some(30))],
            Duration::ZERO,
            &cancel_after(1),
        )
        .await;
        assert!(matches!(outcome, Err(RetryStop::Cancelled)));
        assert_eq!(requests, 1);
        assert_eq!(notices, [notice(2, 30)]);
        assert_eq!(elapsed, Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_with_a_cancelled_token_sends_no_request() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (outcome, requests, notices, _) = run(vec![], Duration::ZERO, &cancel).await;
        assert!(matches!(outcome, Err(RetryStop::Cancelled)));
        assert_eq!((requests, notices.len()), (0, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_cancelled_during_a_request_reports_no_retry() {
        let (outcome, requests, notices, elapsed) =
            run(vec![transient()], Duration::from_secs(5), &cancel_after(1)).await;
        assert!(matches!(outcome, Err(RetryStop::Cancelled)));
        assert_eq!((requests, notices.len()), (1, 0));
        assert_eq!(elapsed, Duration::from_secs(1));
    }
}
