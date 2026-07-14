#![deny(warnings)]
// #![allow(unused)]
use anyhow::Result;
use chrono::Local;
use minijinja::context;
use reqwest;
use rhai::serde::{from_dynamic, to_dynamic};
use rhai::{Array, Dynamic, Engine, ImmutableString, Scope, AST};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::time::Duration;

use crate::notifier::{get_tag, redact_secrets, send_with_retry, Event, HostStat, NOTIFIER_HANDLE};

const KIND: &str = "webhook";

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct Receiver {
    pub enabled: bool,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub timeout: u32,
    pub script: String,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct Config {
    pub enabled: bool,
    pub receiver: Vec<Receiver>,
}

pub struct Webhook {
    config: &'static Config,
    http_client: reqwest::Client,
    engine: Engine,
    ast_list: Vec<Option<AST>>,
}

#[allow(clippy::needless_pass_by_value)]
fn join(arr: Array, sep: ImmutableString) -> ImmutableString {
    arr.iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(sep.as_str())
        .into()
}

fn now_str() -> ImmutableString {
    Local::now().format("%Y-%m-%d %H:%M:%S %Z").to_string().into()
}

#[allow(clippy::needless_pass_by_value)]
fn to_json(o: Dynamic) -> ImmutableString {
    serde_json::to_string(&o)
        .map(std::convert::Into::into)
        .unwrap_or_default()
}

impl Webhook {
    pub fn new(cfg: &'static Config) -> Self {
        let mut o = Self {
            config: cfg,
            http_client: reqwest::Client::new(),
            engine: Engine::new(),
            ast_list: Vec::new(),
        };

        o.engine.register_fn("to_json", to_json);
        o.engine.register_fn("join", join);
        o.engine.register_fn("now_str", now_str);

        for receiver in &o.config.receiver {
            let ast = if receiver.enabled {
                o.engine.compile(&receiver.script).ok()
            } else {
                None
            };
            o.ast_list.push(ast);
        }

        o
    }
    fn call_webhook(&self, receiver: &Receiver, content: String) -> Result<()> {
        if content.is_empty() {
            return Ok(());
        }

        let http_client = self.http_client.clone();
        let handle = NOTIFIER_HANDLE
            .lock()
            .map_err(|_| anyhow::anyhow!("notification runtime lock is unavailable"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("notification runtime is unavailable"))?;
        let receiver = receiver.clone();
        let secrets = legacy_receiver_secrets(&receiver);
        handle.spawn(async move {
            match deliver_legacy_receiver(&http_client, &receiver, content).await {
                Ok(()) => info!("legacy webhook notification sent"),
                Err(err) => error!(
                    "legacy webhook notification failed: {}",
                    sanitize_webhook_error(&err.to_string(), &secrets)
                ),
            }
        });
        Ok(())
    }

    fn call_structured_webhook(
        &self,
        receiver: &crate::admin::StructuredWebhookReceiver,
        content: String,
    ) -> Result<()> {
        if content.is_empty() {
            return Ok(());
        }

        let handle = NOTIFIER_HANDLE
            .lock()
            .map_err(|_| anyhow::anyhow!("notification runtime lock is unavailable"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("notification runtime is unavailable"))?;
        let http_client = self.http_client.clone();
        let receiver = receiver.clone();
        let secrets = structured_receiver_secrets(&receiver);
        handle.spawn(async move {
            match deliver_structured_receiver(&http_client, &receiver, content).await {
                Ok(()) => info!("structured webhook notification sent"),
                Err(err) => error!(
                    "structured webhook notification failed: {}",
                    sanitize_webhook_error(&err.to_string(), &secrets)
                ),
            }
        });
        Ok(())
    }

    fn execute_receivers<F>(&self, event: &Event, stat: &HostStat, mut deliver: F) -> Result<()>
    where
        F: FnMut(&Receiver, String) -> Result<()>,
    {
        let mut attempted = 0_usize;
        let mut succeeded = 0_usize;

        for (idx, receiver) in self.config.receiver.iter().enumerate() {
            if !receiver.enabled {
                continue;
            }
            let Some(ast) = self.ast_list.get(idx).and_then(Option::as_ref) else {
                continue;
            };
            attempted += 1;

            let result = (|| -> Result<()> {
                let mut scope = Scope::new();
                scope.push("event", get_tag(event));
                scope.push("host", to_dynamic(stat)?);
                scope.push("config", to_dynamic(receiver)?);
                scope.push("ip_info", to_dynamic(stat.ip_info.as_ref())?);
                scope.push("sys_info", to_dynamic(stat.sys_info.as_ref())?);

                let value: Dynamic = self.engine.eval_ast_with_scope(&mut scope, ast)?;
                if let Ok(parts) = from_dynamic::<Array>(&value) {
                    if parts.len() >= 2 && from_dynamic::<bool>(&parts[0]).unwrap_or_default() {
                        let content = serde_json::to_string(&parts[1])
                            .map_err(|_| anyhow::anyhow!("invalid legacy webhook payload"))?;
                        deliver(receiver, content)?;
                    }
                }
                Ok(())
            })();

            match result {
                Ok(()) => succeeded += 1,
                Err(error) => error!("{}", legacy_receiver_failure_context(idx, &error)),
            }
        }

        if attempted > 0 && succeeded == 0 {
            Err(anyhow::anyhow!("legacy webhook notification failed"))
        } else {
            Ok(())
        }
    }
}

fn legacy_receiver_failure_context(index: usize, _error: &anyhow::Error) -> String {
    format!("legacy webhook receiver failed: index={index}, error=notification failed")
}

impl crate::notifier::Notifier for Webhook {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn handles_readiness(&self) -> bool {
        true
    }

    fn send_notify(&self, _content: String) -> Result<()> {
        Ok(())
    }

    fn notify_test(&self) -> Result<()> {
        match crate::admin::effective_webhook_override(self.config) {
            crate::admin::EffectiveWebhookConfig::Structured(config) => {
                return execute_structured_receivers(
                    &config,
                    &Event::Custom,
                    &HostStat::default(),
                    |receiver, _content| self.call_structured_webhook(receiver, "❗ServerStatus test msg".to_string()),
                );
            }
            crate::admin::EffectiveWebhookConfig::Legacy(config) if !config.is_ready() => {
                return Ok(());
            }
            crate::admin::EffectiveWebhookConfig::Legacy(_) => {}
        }

        let mut attempted = 0_usize;
        let mut succeeded = 0_usize;
        for (idx, receiver) in self.config.receiver.iter().enumerate() {
            if !receiver.enabled || self.ast_list.get(idx).and_then(Option::as_ref).is_none() {
                continue;
            }
            attempted += 1;
            match self.call_webhook(receiver, "❗ServerStatus test msg".into()) {
                Ok(()) => succeeded += 1,
                Err(error) => error!("{}", legacy_receiver_failure_context(idx, &error)),
            }
        }
        if attempted > 0 && succeeded == 0 {
            return Err(anyhow::anyhow!("legacy webhook notification failed"));
        }
        Ok(())
    }

    fn notify(&self, e: &Event, stat: &HostStat) -> Result<()> {
        match crate::admin::effective_webhook_override(self.config) {
            crate::admin::EffectiveWebhookConfig::Legacy(config) => {
                if !config.is_ready() {
                    return Ok(());
                }
                self.execute_receivers(e, stat, |receiver, content| self.call_webhook(receiver, content))
            }
            crate::admin::EffectiveWebhookConfig::Structured(config) => {
                execute_structured_receivers(&config, e, stat, |receiver, content| {
                    self.call_structured_webhook(receiver, content)
                })
            }
        }
    }
}

async fn deliver_legacy_receiver(http_client: &reqwest::Client, receiver: &Receiver, content: String) -> Result<()> {
    send_with_retry(
        || {
            let mut request = http_client
                .post(&receiver.url)
                .timeout(Duration::from_secs(receiver.timeout.into()))
                .body(content.clone());
            for (name, value) in &receiver.headers {
                request = request.header(name, value);
            }
            if let (Some(user), Some(password)) = (receiver.username.as_ref(), receiver.password.as_ref()) {
                if !user.is_empty() && !password.is_empty() {
                    request = request.basic_auth(user, Some(password));
                }
            }
            request.send()
        },
        |_| async { Ok(()) },
    )
    .await?;
    Ok(())
}

async fn deliver_structured_receiver(
    http_client: &reqwest::Client,
    receiver: &crate::admin::StructuredWebhookReceiver,
    content: String,
) -> Result<()> {
    send_with_retry(
        || {
            let mut request = http_client
                .post(&receiver.url)
                .timeout(Duration::from_secs(receiver.timeout.into()))
                .body(content.clone());
            for header in &receiver.headers {
                request = request.header(&header.name, &header.value);
            }
            if !receiver.username.is_empty() && !receiver.password.is_empty() {
                request = request.basic_auth(&receiver.username, Some(&receiver.password));
            }
            request.send()
        },
        |_| async { Ok(()) },
    )
    .await?;
    Ok(())
}

fn execute_structured_receivers<F>(
    config: &crate::admin::StructuredWebhookOverride,
    event: &Event,
    stat: &HostStat,
    mut deliver: F,
) -> Result<()>
where
    F: FnMut(&crate::admin::StructuredWebhookReceiver, String) -> Result<()>,
{
    if !config.is_ready() {
        return Ok(());
    }

    let mut attempted = 0_usize;
    let mut succeeded = 0_usize;
    for (index, receiver) in config.receivers.iter().enumerate() {
        if !receiver.enabled {
            continue;
        }
        attempted += 1;
        let result = render_structured_body(receiver, event, stat).and_then(|content| deliver(receiver, content));
        match result {
            Ok(()) => succeeded += 1,
            Err(error) => error!("{}", structured_receiver_failure_context(index, &error)),
        }
    }

    if attempted > 0 && succeeded == 0 {
        Err(anyhow::anyhow!("structured webhook notification failed"))
    } else {
        Ok(())
    }
}

fn render_structured_body(
    receiver: &crate::admin::StructuredWebhookReceiver,
    event: &Event,
    stat: &HostStat,
) -> Result<String> {
    let headers = receiver
        .headers
        .iter()
        .map(|header| {
            serde_json::json!({
                "name": header.name,
                "value": if header.value.is_empty() { "" } else { "[redacted]" },
            })
        })
        .collect::<Vec<_>>();
    let safe_config = serde_json::json!({
        "id": receiver.id,
        "name": receiver.name,
        "enabled": receiver.enabled,
        "url": if receiver.url.is_empty() { "" } else { "[redacted]" },
        "username": receiver.username,
        "password": if receiver.password.is_empty() { "" } else { "[redacted]" },
        "timeout": receiver.timeout,
        "headers": headers,
    });
    let mut environment = minijinja::Environment::new();
    environment
        .add_template("structured-webhook", &receiver.body_tpl)
        .map_err(|_| anyhow::anyhow!("invalid structured webhook template"))?;
    environment
        .get_template("structured-webhook")
        .map_err(|_| anyhow::anyhow!("invalid structured webhook template"))?
        .render(context!(
            event => get_tag(event),
            host => stat,
            config => safe_config,
            ip_info => stat.ip_info,
            sys_info => stat.sys_info,
        ))
        .map_err(|_| anyhow::anyhow!("failed to render structured webhook template"))
}

fn sanitize_webhook_error<S>(message: &str, secrets: &[S]) -> String
where
    S: AsRef<str>,
{
    redact_secrets(message, secrets)
}

fn legacy_receiver_secrets(receiver: &Receiver) -> Vec<String> {
    let mut secrets = vec![receiver.url.clone()];
    if let Some(username) = &receiver.username {
        secrets.push(username.clone());
    }
    if let Some(password) = &receiver.password {
        secrets.push(password.clone());
    }
    for (name, value) in &receiver.headers {
        secrets.push(name.clone());
        secrets.push(value.clone());
    }
    secrets
}

fn structured_receiver_secrets(receiver: &crate::admin::StructuredWebhookReceiver) -> Vec<String> {
    let mut secrets = vec![
        receiver.url.clone(),
        receiver.username.clone(),
        receiver.password.clone(),
    ];
    for header in &receiver.headers {
        secrets.push(header.name.clone());
        secrets.push(header.value.clone());
    }
    secrets
}

fn structured_receiver_failure_context(index: usize, _error: &anyhow::Error) -> String {
    format!("structured webhook receiver failed: index={index}, error=notification failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Bytes,
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::post,
        Router,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    #[derive(Default)]
    struct CapturedRequest {
        authorization: String,
        custom_header: String,
        body: String,
    }

    async fn capture_request(
        State(captured): State<Arc<Mutex<CapturedRequest>>>,
        headers: HeaderMap,
        body: Bytes,
    ) -> StatusCode {
        let mut captured = captured.lock().unwrap();
        captured.authorization = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        captured.custom_header = headers
            .get("x-webhook-key")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        captured.body = String::from_utf8_lossy(&body).into_owned();
        StatusCode::NO_CONTENT
    }

    async fn capture_endpoint(captured: Arc<Mutex<CapturedRequest>>) -> String {
        let app = Router::new().route("/hook", post(capture_request)).with_state(captured);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}/hook")
    }

    async fn delayed_response(State(attempts): State<Arc<AtomicUsize>>) -> StatusCode {
        attempts.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(2)).await;
        StatusCode::NO_CONTENT
    }

    async fn delayed_endpoint(attempts: Arc<AtomicUsize>) -> String {
        let app = Router::new()
            .route("/sentinel-timeout-secret", post(delayed_response))
            .with_state(attempts);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}/sentinel-timeout-secret")
    }

    #[test]
    fn disabled_invalid_legacy_receiver_does_not_panic_during_construction() {
        let config = Box::leak(Box::new(Config {
            enabled: false,
            receiver: vec![Receiver {
                enabled: true,
                script: "let = invalid".into(),
                ..Default::default()
            }],
        }));

        let notifier = Webhook::new(config);

        assert_eq!(crate::notifier::Notifier::kind(&notifier), "webhook");
    }

    #[test]
    fn invalid_legacy_receiver_does_not_disable_valid_sibling() {
        let config = Box::leak(Box::new(Config {
            enabled: true,
            receiver: vec![
                Receiver {
                    enabled: true,
                    url: "https://invalid.example/hook".into(),
                    timeout: 5,
                    script: "let = sentinel-compile-secret".into(),
                    ..Default::default()
                },
                Receiver {
                    enabled: true,
                    url: "https://valid.example/hook".into(),
                    timeout: 5,
                    script: "[false, \"safe\"]".into(),
                    ..Default::default()
                },
            ],
        }));

        let notifier = Webhook::new(config);

        assert!(notifier.ast_list[0].is_none());
        assert!(notifier.ast_list[1].is_some());
        assert!(crate::notifier::Notifier::notify(&notifier, &Event::NodeDown, &HostStat::default(),).is_ok());
    }

    #[test]
    fn runtime_error_in_one_receiver_does_not_stop_valid_sibling() {
        let config = Box::leak(Box::new(Config {
            enabled: true,
            receiver: vec![
                Receiver {
                    enabled: true,
                    username: Some("user".into()),
                    password: Some("sentinel-webhook-secret".into()),
                    script: "throw \"sentinel-script-secret\";".into(),
                    ..Default::default()
                },
                Receiver {
                    enabled: true,
                    script: "[true, \"recorded\"]".into(),
                    ..Default::default()
                },
            ],
        }));
        let notifier = Webhook::new(config);
        let mut deliveries = Vec::new();

        let result = notifier.execute_receivers(&Event::NodeDown, &HostStat::default(), |_receiver, content| {
            deliveries.push(content);
            Ok(())
        });

        assert!(result.is_ok());
        assert_eq!(deliveries.len(), 1);
        assert!(deliveries[0].contains("recorded"));
        let context = legacy_receiver_failure_context(0, &anyhow::anyhow!("sentinel-script-secret"));
        assert_eq!(
            context,
            "legacy webhook receiver failed: index=0, error=notification failed"
        );
        assert!(!context.contains("sentinel-script-secret"));
        assert!(!context.contains("sentinel-webhook-secret"));
    }

    #[test]
    fn webhook_error_text_never_contains_credentials() {
        let detail = sanitize_webhook_error(
            "https://hooks.example/secret Authorization: Bearer abc",
            &["https://hooks.example/secret", "Bearer abc"],
        );

        assert_eq!(detail, "[redacted] Authorization: [redacted]");
    }

    #[test]
    fn structured_receivers_use_minijinja_context_and_isolate_failures() {
        let config = crate::admin::StructuredWebhookOverride {
            enabled: true,
            receivers: vec![
                crate::admin::StructuredWebhookReceiver {
                    id: "invalid".into(),
                    enabled: true,
                    url: "https://invalid.example/secret".into(),
                    timeout: 5,
                    body_tpl: "{% if %} sentinel-rhai-secret".into(),
                    ..Default::default()
                },
                crate::admin::StructuredWebhookReceiver {
                    id: "valid".into(),
                    name: "Operations".into(),
                    enabled: true,
                    url: "https://valid.example/sentinel-url-secret".into(),
                    password: "sentinel-password-secret".into(),
                    timeout: 5,
                    headers: vec![crate::admin::WebhookHeaderOverride {
                        name: "X-Webhook-Key".into(),
                        value: "sentinel-header-secret".into(),
                        ..Default::default()
                    }],
                    body_tpl: concat!(
                        "{{ event }}|{{ host.name }}|{{ config.name }}|",
                        "{{ config.url }}|{{ config.password }}|{{ config.headers }}|",
                        "{{ ip_info }}|{{ sys_info }}"
                    )
                    .into(),
                    ..Default::default()
                },
            ],
        };
        let stat = HostStat {
            name: "alpha".into(),
            ..Default::default()
        };
        let mut deliveries = Vec::new();

        let result = execute_structured_receivers(&config, &Event::NodeDown, &stat, |_receiver, content| {
            deliveries.push(content);
            Ok(())
        });

        assert!(result.is_ok());
        assert_eq!(deliveries.len(), 1);
        assert!(deliveries[0].starts_with("NodeDown|alpha|Operations|"));
        for secret in [
            "sentinel-rhai-secret",
            "sentinel-url-secret",
            "sentinel-password-secret",
            "sentinel-header-secret",
        ] {
            assert!(!deliveries[0].contains(secret));
        }
    }

    #[tokio::test]
    async fn structured_webhook_sends_headers_basic_auth_and_rendered_body() {
        let captured = Arc::new(Mutex::new(CapturedRequest::default()));
        let receiver = crate::admin::StructuredWebhookReceiver {
            enabled: true,
            url: capture_endpoint(Arc::clone(&captured)).await,
            username: "operator".into(),
            password: "sentinel-basic-secret".into(),
            timeout: 5,
            headers: vec![crate::admin::WebhookHeaderOverride {
                name: "X-Webhook-Key".into(),
                value: "sentinel-header-secret".into(),
                ..Default::default()
            }],
            body_tpl: "unused".into(),
            ..Default::default()
        };

        deliver_structured_receiver(&reqwest::Client::new(), &receiver, "rendered-body".into())
            .await
            .unwrap();

        let captured = captured.lock().unwrap();
        assert!(captured.authorization.starts_with("Basic "));
        assert_eq!(captured.custom_header, "sentinel-header-secret");
        assert_eq!(captured.body, "rendered-body");
    }

    #[tokio::test]
    async fn structured_webhook_timeout_is_generic_and_retried_three_times() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let receiver = crate::admin::StructuredWebhookReceiver {
            enabled: true,
            url: delayed_endpoint(Arc::clone(&attempts)).await,
            timeout: 1,
            body_tpl: "unused".into(),
            ..Default::default()
        };

        let error = deliver_structured_receiver(&reqwest::Client::new(), &receiver, "body".into())
            .await
            .unwrap_err()
            .to_string();
        let sanitized = sanitize_webhook_error(&error, &structured_receiver_secrets(&receiver));

        assert_eq!(sanitized, "notification transport failed");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert!(!sanitized.contains("sentinel-timeout-secret"));
    }
}
