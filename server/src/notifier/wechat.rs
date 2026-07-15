#![deny(warnings)]
use anyhow::{anyhow, Result};
use log::{error, info};
use minijinja::context;
use reqwest;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;
use tokio::time::Duration;

use crate::notifier::{
    redact_secrets, send_with_retry, Event, HostStat, NotificationTestError, NotificationTestResult, NOTIFIER_HANDLE,
};

// https://qydev.weixin.qq.com/wiki/index.php?title=%E4%B8%BB%E5%8A%A8%E8%B0%83%E7%94%A8
// https://qydev.weixin.qq.com/wiki/index.php?title=%E5%8F%91%E9%80%81%E6%8E%A5%E5%8F%A3%E8%AF%B4%E6%98%8E
static TOKEN_URL: &str = "https://qyapi.weixin.qq.com/cgi-bin/gettoken";
static SEND_URL: &str = "https://qyapi.weixin.qq.com/cgi-bin/message/send";
const KIND: &str = "wechat";

fn default_expire_tpl() -> String {
    "{{config.title}}\n{{host.location}} {{host.name}} {{host.expire.label}}\nExpire: {{host.expire.date}}".to_string()
}

fn default_health_tpl() -> String {
    "{{config.title}}\n{{host.custom}}".to_string()
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    pub enabled: bool,
    pub corp_id: String,
    pub corp_secret: String,
    pub agent_id: String,
    pub title: String,
    pub online_tpl: String,
    pub offline_tpl: String,
    pub custom_tpl: String,
    #[serde(default = "default_expire_tpl")]
    pub expire_tpl: String,
    #[serde(default = "default_health_tpl")]
    pub health_tpl: String,
}

#[derive(Serialize)]
struct TemplateConfig<'a> {
    enabled: bool,
    corp_id: &'a str,
    corp_secret: &'static str,
    agent_id: &'a str,
    title: &'a str,
    online_tpl: &'a str,
    offline_tpl: &'a str,
    custom_tpl: &'a str,
    expire_tpl: &'a str,
    health_tpl: &'a str,
}

impl<'a> From<&'a Config> for TemplateConfig<'a> {
    fn from(config: &'a Config) -> Self {
        Self {
            enabled: config.enabled,
            corp_id: &config.corp_id,
            corp_secret: redacted_template_secret(&config.corp_secret),
            agent_id: &config.agent_id,
            title: &config.title,
            online_tpl: &config.online_tpl,
            offline_tpl: &config.offline_tpl,
            custom_tpl: &config.custom_tpl,
            expire_tpl: &config.expire_tpl,
            health_tpl: &config.health_tpl,
        }
    }
}

fn redacted_template_secret(value: &str) -> &'static str {
    if value.is_empty() {
        ""
    } else {
        "[redacted]"
    }
}

pub struct WeChat {
    config: &'static Config,
    http_client: reqwest::Client,
}

impl WeChat {
    pub fn new(cfg: &'static Config) -> Self {
        Self {
            config: cfg,
            http_client: reqwest::Client::new(),
        }
    }

    fn send_with_config(&self, config: &Config, text_content: String) -> Result<()> {
        if !config.enabled {
            return Ok(());
        }
        let (data, agent_id) = prepare_delivery(config)?;

        let http_client = self.http_client.clone();
        let handle = NOTIFIER_HANDLE
            .lock()
            .map_err(|_| anyhow!("notification runtime lock is unavailable"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("notification runtime is unavailable"))?;
        let secrets = [
            config.corp_id.clone(),
            config.corp_secret.clone(),
            config.agent_id.clone(),
        ];
        handle.spawn(async move {
            match deliver_wechat(&http_client, TOKEN_URL, SEND_URL, &data, &agent_id, &text_content).await {
                Ok(()) => info!("wechat notification sent"),
                Err(err) => error!(
                    "wechat notification failed: {}",
                    redact_secrets(&err.to_string(), &secrets)
                ),
            }
        });

        Ok(())
    }
}

pub(crate) async fn test(config: &Config) -> NotificationTestResult {
    test_with_endpoints(config, TOKEN_URL, SEND_URL).await
}

async fn test_with_endpoints(config: &Config, token_endpoint: &str, send_endpoint: &str) -> NotificationTestResult {
    validate_test_config(config).map_err(|_| NotificationTestError::InvalidConfiguration)?;
    let (data, agent_id) = prepare_delivery(config).map_err(|_| NotificationTestError::InvalidConfiguration)?;
    deliver_wechat(
        &reqwest::Client::new(),
        token_endpoint,
        send_endpoint,
        &data,
        &agent_id,
        "❗ServerStatus test msg",
    )
    .await
    .map_err(|_| NotificationTestError::DeliveryFailed)
}

fn prepare_delivery(config: &Config) -> Result<(HashMap<&'static str, String>, String)> {
    if !config.is_ready() {
        return Err(anyhow!("WeChat notifier is not ready"));
    }
    Ok((
        HashMap::from([
            ("corpid", config.corp_id.clone()),
            ("corpsecret", config.corp_secret.clone()),
        ]),
        config.agent_id.clone(),
    ))
}

fn validate_test_config(config: &Config) -> Result<()> {
    if !config.is_ready() {
        return Err(anyhow!("WeChat notifier is not ready"));
    }
    let stat = HostStat::default();
    for event in [
        Event::NodeUp,
        Event::NodeDown,
        Event::Custom,
        Event::Expire,
        Event::Health,
    ] {
        render_content(config, &event, &stat)?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct WeChatResponse {
    errcode: i64,
    #[serde(default)]
    access_token: Option<String>,
}

async fn deliver_wechat(
    http_client: &reqwest::Client,
    token_endpoint: &str,
    send_endpoint: &str,
    token_data: &HashMap<&str, String>,
    agent_id: &str,
    text_content: &str,
) -> Result<()> {
    let query_secrets = token_data.values().map(String::as_str).collect::<Vec<_>>();
    let mut token_url = reqwest::Url::parse(token_endpoint).map_err(|_| anyhow!("invalid WeChat token endpoint"))?;
    {
        let mut query = token_url.query_pairs_mut();
        for (name, value) in token_data {
            query.append_pair(name, value);
        }
    }
    let token = send_with_retry(
        || {
            http_client
                .get(token_url.clone())
                .timeout(Duration::from_secs(5))
                .send()
        },
        |response| async move {
            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(anyhow::Error::new)
                .map_err(|error| error.context("invalid WeChat token response"))?;
            parse_token_response(status, &body)
        },
    )
    .await
    .map_err(|error| anyhow!(redact_secrets(&error.to_string(), &query_secrets)))?;

    let send_url = format!("{send_endpoint}?access_token={token}");
    let send_data = serde_json::json!({
        "touser": "@all",
        "agentid": agent_id,
        "msgtype": "text",
        "text": {
            "content": text_content,
        },
        "safe": 0
    });
    send_with_retry(
        || {
            http_client
                .post(&send_url)
                .timeout(Duration::from_secs(5))
                .json(&send_data)
                .send()
        },
        |response| async move {
            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(anyhow::Error::new)
                .map_err(|error| error.context("invalid WeChat send response"))?;
            validate_send_response(status, &body)
        },
    )
    .await
}

fn parse_token_response(status: reqwest::StatusCode, body: &str) -> Result<String> {
    if !status.is_success() {
        return Err(anyhow!(
            "WeChat token request failed with HTTP status {}",
            status.as_u16()
        ));
    }
    let response: WeChatResponse = serde_json::from_str(body).map_err(|_| anyhow!("invalid WeChat token response"))?;
    if response.errcode != 0 {
        return Err(anyhow!("WeChat token request was rejected"));
    }
    response
        .access_token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| anyhow!("invalid WeChat token response"))
}

fn validate_send_response(status: reqwest::StatusCode, body: &str) -> Result<()> {
    if !status.is_success() {
        return Err(anyhow!(
            "WeChat send request failed with HTTP status {}",
            status.as_u16()
        ));
    }
    let response: WeChatResponse = serde_json::from_str(body).map_err(|_| anyhow!("invalid WeChat send response"))?;
    if response.errcode == 0 {
        Ok(())
    } else {
        Err(anyhow!("WeChat send request was rejected"))
    }
}

impl crate::notifier::Notifier for WeChat {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn send_notify(&self, text_content: String) -> Result<()> {
        let config = crate::admin::effective_wechat_config(self.config);
        self.send_with_config(&config, text_content)
    }

    fn notify(&self, e: &Event, stat: &HostStat) -> Result<()> {
        let config = crate::admin::effective_wechat_config(self.config);
        if !config.enabled {
            return Ok(());
        }
        if !config.is_ready() {
            return Err(anyhow!("WeChat notifier is not ready"));
        }

        let content = render_content(&config, e, stat)?;
        if content.is_empty() {
            return Ok(());
        }
        let content = if matches!(e, Event::Custom) {
            format!("{}\n{content}", config.title)
        } else {
            content
        };
        self.send_with_config(&config, content)
    }

    fn notify_test(&self) -> Result<()> {
        let config = crate::admin::effective_wechat_config(self.config);
        self.send_with_config(&config, "❗ServerStatus test msg".to_string())
    }
}

fn render_content(config: &Config, event: &Event, stat: &HostStat) -> Result<String> {
    let source = match event {
        Event::NodeUp => &config.online_tpl,
        Event::NodeDown => &config.offline_tpl,
        Event::Custom => &config.custom_tpl,
        Event::Expire => &config.expire_tpl,
        Event::Health => &config.health_tpl,
    };
    let safe_config = TemplateConfig::from(config);
    let mut environment = minijinja::Environment::new();
    environment
        .add_template("wechat", source)
        .map_err(|_| anyhow!("invalid WeChat template"))?;
    let rendered = environment
        .get_template("wechat")
        .map_err(|_| anyhow!("invalid WeChat template"))?
        .render(context!(host => stat, config => safe_config, ip_info => stat.ip_info, sys_info => stat.sys_info))
        .map_err(|_| anyhow!("failed to render WeChat template"))?;
    Ok(rendered
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Query, State},
        routing::{get, post},
        Json, Router,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    #[derive(Default)]
    struct CapturedWeChatRequests {
        token_calls: AtomicUsize,
        token_queries: Mutex<Vec<HashMap<String, String>>>,
        send_queries: Mutex<Vec<HashMap<String, String>>>,
    }

    async fn token_response(
        State(captured): State<Arc<CapturedWeChatRequests>>,
        Query(query): Query<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        let call = captured.token_calls.fetch_add(1, Ordering::SeqCst);
        captured.token_queries.lock().unwrap().push(query.clone());
        if call == 0 {
            Json(serde_json::json!({
                "errcode": 0,
                "access_token": "access-token",
            }))
        } else {
            Json(serde_json::json!({
                "errcode": 40013,
                "errmsg": format!(
                    "{} {}",
                    query.get("corpid").map(String::as_str).unwrap_or_default(),
                    query.get("corpsecret").map(String::as_str).unwrap_or_default(),
                ),
            }))
        }
    }

    async fn send_response(
        State(captured): State<Arc<CapturedWeChatRequests>>,
        Query(query): Query<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        captured.send_queries.lock().unwrap().push(query);
        Json(serde_json::json!({ "errcode": 0 }))
    }

    async fn wechat_endpoints(captured: Arc<CapturedWeChatRequests>) -> (String, String) {
        let app = Router::new()
            .route("/token", get(token_response))
            .route("/send", post(send_response))
            .with_state(captured);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}/token"), format!("http://{address}/send"))
    }

    #[test]
    fn disabled_invalid_template_does_not_panic_during_construction() {
        let config = Box::leak(Box::new(Config {
            enabled: false,
            online_tpl: "{{ invalid".into(),
            ..Default::default()
        }));

        let notifier = WeChat::new(config);

        assert_eq!(crate::notifier::Notifier::kind(&notifier), "wechat");
    }

    #[test]
    fn enabled_malformed_wechat_template_returns_redacted_error() {
        let config = Box::leak(Box::new(Config {
            enabled: true,
            corp_id: "sentinel-corp-id".into(),
            corp_secret: "sentinel-corp-secret".into(),
            agent_id: "sentinel-agent-id".into(),
            online_tpl: "{{ sentinel-template-secret".into(),
            ..Default::default()
        }));
        let notifier = WeChat::new(config);

        let error = crate::notifier::Notifier::notify(&notifier, &Event::NodeUp, &HostStat::default())
            .unwrap_err()
            .to_string();

        assert_eq!(error, "invalid WeChat template");
        for secret in [
            "sentinel-corp-id",
            "sentinel-corp-secret",
            "sentinel-agent-id",
            "sentinel-template-secret",
        ] {
            assert!(!error.contains(secret));
        }
    }

    #[test]
    fn wechat_template_config_redacts_corp_secret() {
        let config = Config {
            corp_id: "visible-corp-id".into(),
            corp_secret: "sentinel-corp-secret".into(),
            agent_id: "visible-agent-id".into(),
            online_tpl: "{{ config.corp_id }}|{{ config.agent_id }}|{{ config.corp_secret }}".into(),
            ..Default::default()
        };

        let rendered = render_content(&config, &Event::NodeUp, &HostStat::default()).unwrap();

        assert_eq!(rendered, "visible-corp-id|visible-agent-id|[redacted]");
        assert!(!rendered.contains("sentinel-corp-secret"));
    }

    #[test]
    fn wechat_token_and_send_responses_require_errcode_zero() {
        assert_eq!(
            parse_token_response(reqwest::StatusCode::OK, r#"{"errcode":0,"access_token":"token"}"#,).unwrap(),
            "token"
        );
        assert!(parse_token_response(
            reqwest::StatusCode::OK,
            r#"{"errcode":40013,"access_token":"sentinel-token"}"#,
        )
        .is_err());
        assert!(validate_send_response(reqwest::StatusCode::OK, r#"{"errcode":0}"#).is_ok());
        assert!(validate_send_response(
            reqwest::StatusCode::OK,
            r#"{"errcode":81013,"errmsg":"sentinel-provider-secret"}"#,
        )
        .is_err());
        assert!(validate_send_response(reqwest::StatusCode::BAD_GATEWAY, r#"{"errcode":0}"#,).is_err());
    }

    #[tokio::test]
    async fn wechat_token_uses_get_query_and_redacts_provider_failures() {
        let captured = Arc::new(CapturedWeChatRequests::default());
        let (token_endpoint, send_endpoint) = wechat_endpoints(Arc::clone(&captured)).await;
        let corp_id = "sentinel corp&id".to_string();
        let corp_secret = "sentinel secret&value".to_string();
        let token_data = HashMap::from([("corpid", corp_id.clone()), ("corpsecret", corp_secret.clone())]);
        let client = reqwest::Client::new();

        deliver_wechat(
            &client,
            &token_endpoint,
            &send_endpoint,
            &token_data,
            "agent-id",
            "message",
        )
        .await
        .unwrap();

        let error = deliver_wechat(
            &client,
            &token_endpoint,
            &send_endpoint,
            &token_data,
            "agent-id",
            "message",
        )
        .await
        .unwrap_err()
        .to_string();

        let token_queries = captured.token_queries.lock().unwrap();
        assert_eq!(token_queries.len(), 2);
        assert_eq!(token_queries[0].get("corpid"), Some(&corp_id));
        assert_eq!(token_queries[0].get("corpsecret"), Some(&corp_secret));
        let send_queries = captured.send_queries.lock().unwrap();
        assert_eq!(send_queries.len(), 1);
        assert_eq!(
            send_queries[0].get("access_token").map(String::as_str),
            Some("access-token")
        );
        assert_eq!(error, "WeChat token request was rejected");
        for secret in [
            corp_id.as_str(),
            corp_secret.as_str(),
            "sentinel+corp%26id",
            "sentinel+secret%26value",
        ] {
            assert!(!error.contains(secret));
        }
    }
}
