#![deny(warnings)]
use anyhow::{anyhow, Result};
use log::{error, info};
use minijinja::context;
use reqwest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::time::Duration;

use crate::notifier::{
    redact_secrets, send_with_retry, Event, HostStat, NotificationTestError, NotificationTestResult, NOTIFIER_HANDLE,
};

const KIND: &str = "tgbot";

fn default_expire_tpl() -> String {
    "{{config.title}}\n<pre>{{host.location}} {{host.name}} {{host.expire.label}}</pre>\n<pre>Expire: {{host.expire.date}}</pre>".to_string()
}

fn default_health_tpl() -> String {
    "{{config.title}}\n<pre>{{host.custom}}</pre>".to_string()
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct Config {
    pub enabled: bool,
    pub bot_token: String,
    pub chat_id: String,
    pub title: String,
    pub online_tpl: String,
    pub offline_tpl: String,
    pub custom_tpl: String,
    #[serde(default = "default_expire_tpl")]
    pub expire_tpl: String,
    #[serde(default = "default_health_tpl")]
    pub health_tpl: String,
}

pub struct TGBot {
    config: &'static Config,
    http_client: reqwest::Client,
}

impl TGBot {
    pub fn new(cfg: &'static Config) -> Self {
        Self {
            config: cfg,
            http_client: reqwest::Client::new(),
        }
    }

    fn send_with_config(&self, config: &Config, html_content: String) -> Result<()> {
        if !config.enabled {
            return Ok(());
        }
        let (tg_url, data) = prepare_delivery(config, html_content)?;
        let handle = NOTIFIER_HANDLE
            .lock()
            .map_err(|_| anyhow!("notification runtime lock is unavailable"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("notification runtime is unavailable"))?;
        let http_client = self.http_client.clone();
        let secrets = [tg_url.clone(), config.bot_token.clone(), config.chat_id.clone()];
        handle.spawn(async move {
            match deliver_telegram(&http_client, &tg_url, &data).await {
                Ok(()) => info!("telegram notification sent"),
                Err(err) => {
                    error!(
                        "telegram notification failed: {}",
                        redact_secrets(&err.to_string(), &secrets)
                    );
                }
            }
        });

        Ok(())
    }
}

pub(crate) async fn test(config: &Config) -> NotificationTestResult {
    let endpoint = format!("https://api.telegram.org/bot{}/sendMessage", config.bot_token);
    test_with_endpoint(config, &endpoint).await
}

async fn test_with_endpoint(config: &Config, endpoint: &str) -> NotificationTestResult {
    validate_test_config(config).map_err(|_| NotificationTestError::InvalidConfiguration)?;
    let (_, data) = prepare_delivery(config, "❗ServerStatus test msg".to_string())
        .map_err(|_| NotificationTestError::InvalidConfiguration)?;
    deliver_telegram(&reqwest::Client::new(), endpoint, &data)
        .await
        .map_err(|_| NotificationTestError::DeliveryFailed)
}

fn prepare_delivery(config: &Config, html_content: String) -> Result<(String, HashMap<&'static str, String>)> {
    if !config.is_ready() {
        return Err(anyhow!("Telegram notifier is not ready"));
    }
    let mut data = HashMap::new();
    data.insert("chat_id", config.chat_id.clone());
    data.insert("parse_mode", "HTML".to_string());
    data.insert("text", html_content);
    let endpoint = format!("https://api.telegram.org/bot{}/sendMessage", config.bot_token);
    Ok((endpoint, data))
}

fn validate_test_config(config: &Config) -> Result<()> {
    if !config.is_ready() {
        return Err(anyhow!("Telegram notifier is not ready"));
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
struct TelegramResponse {
    ok: bool,
}

async fn deliver_telegram(http_client: &reqwest::Client, endpoint: &str, data: &HashMap<&str, String>) -> Result<()> {
    send_with_retry(
        || {
            http_client
                .post(endpoint)
                .timeout(Duration::from_secs(5))
                .json(data)
                .send()
        },
        |response| async move {
            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(anyhow::Error::new)
                .map_err(|error| error.context("invalid Telegram response"))?;
            validate_telegram_response(status, &body)
        },
    )
    .await
}

fn validate_telegram_response(status: reqwest::StatusCode, body: &str) -> Result<()> {
    if !status.is_success() {
        return Err(anyhow!("Telegram request failed with HTTP status {}", status.as_u16()));
    }
    let response: TelegramResponse = serde_json::from_str(body).map_err(|_| anyhow!("invalid Telegram response"))?;
    if response.ok {
        Ok(())
    } else {
        Err(anyhow!("Telegram provider rejected notification"))
    }
}

impl crate::notifier::Notifier for TGBot {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn send_notify(&self, html_content: String) -> Result<()> {
        let config = crate::admin::effective_tgbot_config(self.config);
        self.send_with_config(&config, html_content)
    }

    fn notify(&self, e: &Event, stat: &HostStat) -> Result<()> {
        let config = crate::admin::effective_tgbot_config(self.config);
        if !config.enabled {
            return Ok(());
        }
        if !config.is_ready() {
            return Err(anyhow!("Telegram notifier is not ready"));
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
        let config = crate::admin::effective_tgbot_config(self.config);
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
    let mut environment = minijinja::Environment::new();
    environment
        .add_template("telegram", source)
        .map_err(|_| anyhow!("invalid Telegram template"))?;
    let rendered = environment
        .get_template("telegram")
        .map_err(|_| anyhow!("invalid Telegram template"))?
        .render(context!(host => stat, config => config, ip_info => stat.ip_info, sys_info => stat.sys_info))
        .map_err(|_| anyhow!("failed to render Telegram template"))?;
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

    #[test]
    fn disabled_malformed_telegram_constructor_does_not_panic() {
        let config = Box::leak(Box::new(Config {
            enabled: false,
            online_tpl: "{{ sentinel-template-secret".into(),
            ..Default::default()
        }));

        let notifier = TGBot::new(config);

        assert_eq!(crate::notifier::Notifier::kind(&notifier), "tgbot");
    }

    #[test]
    fn enabled_malformed_telegram_template_returns_redacted_error() {
        let config = Box::leak(Box::new(Config {
            enabled: true,
            bot_token: "sentinel-token-secret".into(),
            chat_id: "chat-id".into(),
            online_tpl: "{{ sentinel-template-secret".into(),
            ..Default::default()
        }));
        let notifier = TGBot::new(config);

        let error = crate::notifier::Notifier::notify(&notifier, &Event::NodeUp, &HostStat::default())
            .unwrap_err()
            .to_string();

        assert_eq!(error, "invalid Telegram template");
        assert!(!error.contains("sentinel-template-secret"));
        assert!(!error.contains("sentinel-token-secret"));
    }

    #[test]
    fn telegram_rendering_selects_each_current_event_template() {
        let config = Config {
            title: "current".into(),
            online_tpl: "online {{ config.title }}".into(),
            offline_tpl: "offline {{ config.title }}".into(),
            custom_tpl: "custom {{ config.title }}".into(),
            expire_tpl: "expire {{ config.title }}".into(),
            health_tpl: "health {{ config.title }}".into(),
            ..Default::default()
        };
        let stat = HostStat::default();

        for (event, expected) in [
            (Event::NodeUp, "online current"),
            (Event::NodeDown, "offline current"),
            (Event::Custom, "custom current"),
            (Event::Expire, "expire current"),
            (Event::Health, "health current"),
        ] {
            assert_eq!(render_content(&config, &event, &stat).unwrap(), expected);
        }
    }

    #[test]
    fn telegram_response_requires_http_success_and_ok_true() {
        assert!(validate_telegram_response(reqwest::StatusCode::OK, r#"{"ok":true}"#).is_ok());
        assert!(validate_telegram_response(reqwest::StatusCode::OK, r#"{"ok":false}"#).is_err());
        assert!(validate_telegram_response(reqwest::StatusCode::BAD_GATEWAY, r#"{"ok":true}"#,).is_err());
    }
}
