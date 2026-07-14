use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use reqwest::{Response, StatusCode};
use serde::Serialize;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::sync::Mutex;
use tokio::runtime::Handle;
use tokio::time::{sleep, Duration};

use crate::payload::HostStat;

pub mod bark;
pub mod email;
pub mod log;
pub mod tgbot;
pub mod webhook;
pub mod wechat;

pub static NOTIFIER_HANDLE: Lazy<Mutex<Option<Handle>>> = Lazy::new(Default::default);
pub(crate) const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotificationTestError {
    UnsupportedKind,
    InvalidConfiguration,
    DeliveryFailed,
}

impl Display for NotificationTestError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedKind => "notification type is unsupported",
            Self::InvalidConfiguration => "notification configuration is invalid",
            Self::DeliveryFailed => "notification delivery failed",
        })
    }
}

impl std::error::Error for NotificationTestError {}

pub(crate) type NotificationTestResult = std::result::Result<(), NotificationTestError>;

pub(crate) async fn test_effective_notification(kind: &str, config: &crate::config::Config) -> NotificationTestResult {
    match kind {
        "tgbot" | "telegram" | "tg" => tgbot::test(&crate::admin::effective_tgbot_config(&config.tgbot)).await,
        "bark" => bark::test(&crate::admin::effective_bark_config(&config.bark)).await,
        "wechat" => wechat::test(&crate::admin::effective_wechat_config(&config.wechat)).await,
        "email" => email::test(&crate::admin::effective_email_config(&config.email)).await,
        "webhook" => webhook::test(&crate::admin::effective_webhook_override(&config.webhook)).await,
        "log" => log::test(&crate::admin::effective_log_config(&config.log)).await,
        _ => Err(NotificationTestError::UnsupportedKind),
    }
}

pub(crate) fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT || status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn is_retryable_transport_error(error: &reqwest::Error) -> bool {
    error.is_request() || error.is_connect() || error.is_timeout() || error.is_body() || error.is_decode()
}

pub(crate) fn redact_secrets<S>(message: &str, secrets: &[S]) -> String
where
    S: AsRef<str>,
{
    secrets.iter().fold(message.to_string(), |sanitized, secret| {
        let secret = secret.as_ref();
        if secret.is_empty() {
            sanitized
        } else {
            sanitized.replace(secret, "[redacted]")
        }
    })
}

pub(crate) async fn send_with_retry<F, Fut, V, VFut, T>(request: F, validate: V) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = reqwest::Result<Response>>,
    V: FnMut(Response) -> VFut,
    VFut: Future<Output = Result<T>>,
{
    send_with_retry_delays(request, validate, RETRY_DELAYS).await
}

async fn send_with_retry_delays<F, Fut, V, VFut, T>(mut request: F, mut validate: V, delays: [Duration; 2]) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = reqwest::Result<Response>>,
    V: FnMut(Response) -> VFut,
    VFut: Future<Output = Result<T>>,
{
    for attempt in 0..=delays.len() {
        match request().await {
            Ok(response) if response.status().is_success() => match validate(response).await {
                Ok(value) => return Ok(value),
                Err(error) if is_retryable_validation_error(&error) && attempt < delays.len() => {
                    sleep(delays[attempt]).await;
                }
                Err(error) => return Err(error),
            },
            Ok(response) if is_retryable_status(response.status()) && attempt < delays.len() => {
                sleep(delays[attempt]).await;
            }
            Ok(response) => {
                return Err(anyhow!(
                    "notification request failed with HTTP status {}",
                    response.status().as_u16()
                ));
            }
            Err(error) if is_retryable_transport_error(&error) && attempt < delays.len() => {
                sleep(delays[attempt]).await;
            }
            Err(error) if is_retryable_transport_error(&error) => {
                return Err(anyhow!("notification transport failed"));
            }
            Err(_) => return Err(anyhow!("notification request failed")),
        }
    }

    Err(anyhow!("notification delivery failed"))
}

fn is_retryable_validation_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<reqwest::Error>()
        .is_some_and(is_retryable_transport_error)
}

#[derive(Debug, Serialize, Clone)]
pub enum Event {
    NodeUp,
    NodeDown,
    Custom,
    Expire,
    Health,
}

fn get_tag(e: &Event) -> &'static str {
    match *e {
        Event::NodeUp => "NodeUp",
        Event::NodeDown => "NodeDown",
        Event::Custom => "Custom",
        Event::Expire => "Expire",
        Event::Health => "Health",
    }
}

pub trait Notifier {
    fn kind(&self) -> &'static str;
    fn handles_readiness(&self) -> bool {
        false
    }
    fn notify(&self, e: &Event, stat: &HostStat) -> Result<()>;
    // send notify impl
    fn send_notify(&self, content: String) -> Result<()>;
    fn notify_test(&self) -> Result<()> {
        self.send_notify("❗ServerStatus test msg".to_string())
    }
}

pub struct ReadinessGate<N, F> {
    inner: N,
    is_ready: F,
}

impl<N, F> ReadinessGate<N, F> {
    pub fn new(inner: N, is_ready: F) -> Self {
        Self { inner, is_ready }
    }
}

impl<N, F> Notifier for ReadinessGate<N, F>
where
    N: Notifier,
    F: Fn() -> bool,
{
    fn kind(&self) -> &'static str {
        self.inner.kind()
    }

    fn notify(&self, event: &Event, stat: &HostStat) -> Result<()> {
        if !(self.is_ready)() && !self.inner.handles_readiness() {
            return Ok(());
        }
        self.inner.notify(event, stat)
    }

    fn send_notify(&self, content: String) -> Result<()> {
        if !(self.is_ready)() && !self.inner.handles_readiness() {
            return Ok(());
        }
        self.inner.send_notify(content)
    }

    fn notify_test(&self) -> Result<()> {
        if !(self.is_ready)() && !self.inner.handles_readiness() {
            return Ok(());
        }
        self.inner.notify_test()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, http::StatusCode, routing::post, Router};
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct CountingNotifier(Arc<AtomicUsize>);

    impl Notifier for CountingNotifier {
        fn kind(&self) -> &'static str {
            "counting"
        }

        fn notify(&self, _e: &Event, _stat: &HostStat) -> Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn send_notify(&self, _content: String) -> Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct SelfGatedNotifier(Arc<AtomicUsize>);

    impl Notifier for SelfGatedNotifier {
        fn kind(&self) -> &'static str {
            "self-gated"
        }

        fn handles_readiness(&self) -> bool {
            true
        }

        fn notify(&self, _e: &Event, _stat: &HostStat) -> Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn send_notify(&self, _content: String) -> Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn readiness_gate_rechecks_before_each_notification() {
        let ready = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = ReadinessGate::new(CountingNotifier(Arc::clone(&calls)), {
            let ready = Arc::clone(&ready);
            move || ready.load(Ordering::SeqCst)
        });

        gate.notify(&Event::NodeDown, &HostStat::default()).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        ready.store(true, Ordering::SeqCst);
        gate.notify(&Event::NodeDown, &HostStat::default()).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn self_gated_notifier_receives_mode_selection_calls() {
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = ReadinessGate::new(SelfGatedNotifier(Arc::clone(&calls)), || false);

        gate.notify(&Event::NodeDown, &HostStat::default()).unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn retry_policy_is_bounded_to_transient_statuses() {
        assert!(is_retryable_status(StatusCode::REQUEST_TIMEOUT));
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(StatusCode::BAD_GATEWAY));
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
        assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
        assert_eq!(RETRY_DELAYS, [Duration::from_secs(1), Duration::from_secs(3)]);
    }

    #[test]
    fn configured_secrets_are_redacted_from_errors() {
        let detail = redact_secrets(
            "https://hooks.example/secret Authorization: Bearer abc",
            &["https://hooks.example/secret", "Bearer abc"],
        );

        assert_eq!(detail, "[redacted] Authorization: [redacted]");
    }

    #[tokio::test]
    async fn effective_notification_tests_reject_incomplete_configs_for_every_supported_kind() {
        let config: crate::config::Config = toml::from_str("").unwrap();

        for kind in ["tgbot", "bark", "wechat", "email", "webhook", "log"] {
            assert_eq!(
                test_effective_notification(kind, &config).await,
                Err(NotificationTestError::InvalidConfiguration),
                "kind={kind}"
            );
        }
    }

    #[tokio::test]
    async fn effective_notification_tests_report_unsupported_kinds_without_config_details() {
        let config: crate::config::Config = toml::from_str("").unwrap();

        let error = test_effective_notification("unknown", &config).await.unwrap_err();

        assert_eq!(error, NotificationTestError::UnsupportedKind);
        assert_eq!(error.to_string(), "notification type is unsupported");
    }

    async fn transient_then_success(State(attempts): State<Arc<AtomicUsize>>) -> StatusCode {
        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
        if attempt < 2 {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::NO_CONTENT
        }
    }

    async fn permanent_failure(State(attempts): State<Arc<AtomicUsize>>) -> StatusCode {
        attempts.fetch_add(1, Ordering::SeqCst);
        StatusCode::BAD_REQUEST
    }

    async fn local_endpoint(
        attempts: Arc<AtomicUsize>,
        handler: fn(State<Arc<AtomicUsize>>) -> std::pin::Pin<Box<dyn std::future::Future<Output = StatusCode> + Send>>,
    ) -> String {
        let app = Router::new()
            .route("/", post(move |state| handler(state)))
            .with_state(attempts);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}/")
    }

    async fn truncated_response_endpoint(attempts: Arc<AtomicUsize>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut request = [0_u8; 1024];
                let _ = socket.read(&mut request).await;
                attempts.fetch_add(1, Ordering::SeqCst);
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 64\r\nConnection: close\r\n\r\n{\"ok\":",
                    )
                    .await;
                let _ = socket.shutdown().await;
            }
        });
        format!("http://{address}/")
    }

    #[tokio::test]
    async fn transient_http_failures_retry_at_most_three_attempts() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let url = local_endpoint(Arc::clone(&attempts), |state| Box::pin(transient_then_success(state))).await;
        let client = reqwest::Client::new();

        let response = send_with_retry_delays(
            || client.post(&url).send(),
            |response| async move { Ok(response) },
            [Duration::ZERO, Duration::ZERO],
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn permanent_http_failure_is_not_retried() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let url = local_endpoint(Arc::clone(&attempts), |state| Box::pin(permanent_failure(state))).await;
        let client = reqwest::Client::new();

        let error = send_with_retry_delays(
            || client.post(&url).send(),
            |response| async move { Ok(response) },
            [Duration::ZERO, Duration::ZERO],
        )
        .await
        .unwrap_err()
        .to_string();

        assert_eq!(error, "notification request failed with HTTP status 400");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(!error.contains(&url));
    }

    #[tokio::test]
    async fn transport_failure_retries_at_most_three_attempts() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let url = format!("http://{address}/");
        let attempts = AtomicUsize::new(0);
        let client = reqwest::Client::new();

        let error = send_with_retry_delays(
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                client.post(&url).send()
            },
            |response| async move { Ok(response) },
            [Duration::ZERO, Duration::ZERO],
        )
        .await
        .unwrap_err()
        .to_string();

        assert_eq!(error, "notification transport failed");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert!(!error.contains(&url));
    }

    #[tokio::test]
    async fn response_body_transport_failures_are_retried_by_shared_helper() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let url = truncated_response_endpoint(Arc::clone(&attempts)).await;
        let client = reqwest::Client::new();

        let error = send_with_retry_delays(
            || client.post(&url).send(),
            |response| async move {
                response
                    .text()
                    .await
                    .map_err(anyhow::Error::new)
                    .map_err(|error| error.context("invalid provider response"))
            },
            [Duration::ZERO, Duration::ZERO],
        )
        .await
        .unwrap_err()
        .to_string();

        assert_eq!(error, "invalid provider response");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert!(!error.contains(&url));
    }
}
