#![deny(warnings)]
use anyhow::{anyhow, Result};
use log::{error, info};
use minijinja::context;
use reqwest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::time::Duration;

use crate::notifier::{redact_secrets, send_with_retry, Event, HostStat, NOTIFIER_HANDLE};

const KIND: &str = "bark";

fn default_server() -> String {
    "https://api.day.app".to_string()
}

fn default_title() -> String {
    "ServerStatus".to_string()
}

fn default_timeout() -> u64 {
    5
}

fn default_online_tpl() -> String {
    "{{host.location}} {{host.alias}} is online".to_string()
}

fn default_offline_tpl() -> String {
    "{{host.location}} {{host.alias}} is offline".to_string()
}

fn default_expire_tpl() -> String {
    "{{host.location}} {{host.alias}} {{host.expire.label}}\nExpire: {{host.expire.date}}".to_string()
}

fn default_health_tpl() -> String {
    "{{host.custom}}".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_server")]
    pub server: String,
    #[serde(default)]
    pub device_key: String,
    #[serde(default = "default_title")]
    pub title: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub sound: String,
    #[serde(default)]
    pub url: String,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default = "default_online_tpl")]
    pub online_tpl: String,
    #[serde(default = "default_offline_tpl")]
    pub offline_tpl: String,
    #[serde(default)]
    pub custom_tpl: String,
    #[serde(default = "default_expire_tpl")]
    pub expire_tpl: String,
    #[serde(default = "default_health_tpl")]
    pub health_tpl: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            server: default_server(),
            device_key: String::new(),
            title: default_title(),
            group: String::new(),
            icon: String::new(),
            sound: String::new(),
            url: String::new(),
            timeout: default_timeout(),
            online_tpl: default_online_tpl(),
            offline_tpl: default_offline_tpl(),
            custom_tpl: String::new(),
            expire_tpl: default_expire_tpl(),
            health_tpl: default_health_tpl(),
        }
    }
}

pub struct Bark {
    config: &'static Config,
    http_client: reqwest::Client,
}

impl Bark {
    pub fn new(cfg: &'static Config) -> Self {
        Self {
            config: cfg,
            http_client: reqwest::Client::new(),
        }
    }

    fn payload(config: &Config, body: String) -> HashMap<String, String> {
        let mut data = HashMap::new();
        data.insert("device_key".to_string(), config.device_key.clone());
        data.insert("title".to_string(), config.title.clone());
        data.insert("body".to_string(), body);

        for (key, value) in [
            ("group", &config.group),
            ("icon", &config.icon),
            ("sound", &config.sound),
            ("url", &config.url),
        ] {
            if !value.trim().is_empty() {
                data.insert(key.to_string(), value.to_string());
            }
        }

        data
    }

    fn send_with_config(&self, config: &Config, body: String) -> Result<()> {
        if !config.enabled {
            return Ok(());
        }
        let (push_url, payload, timeout) = prepare_delivery(config, body)?;
        let handle = NOTIFIER_HANDLE
            .lock()
            .map_err(|_| anyhow!("notification runtime lock is unavailable"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("notification runtime is unavailable"))?;
        let http_client = self.http_client.clone();
        let secrets = [
            push_url.clone(),
            config.server.clone(),
            config.device_key.clone(),
            config.url.clone(),
        ];

        handle.spawn(async move {
            match deliver_bark(&http_client, &push_url, timeout, &payload).await {
                Ok(()) => info!("bark notification sent"),
                Err(err) => error!(
                    "bark notification failed: {}",
                    redact_secrets(&err.to_string(), &secrets)
                ),
            }
        });

        Ok(())
    }
}

impl crate::notifier::Notifier for Bark {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn send_notify(&self, body: String) -> Result<()> {
        let config = crate::admin::effective_bark_config(self.config);
        self.send_with_config(&config, body)
    }

    fn notify(&self, e: &Event, stat: &HostStat) -> Result<()> {
        let config = crate::admin::effective_bark_config(self.config);
        if !config.enabled {
            return Ok(());
        }
        if !config.is_ready() {
            return Err(anyhow!("Bark notifier is not ready"));
        }

        let content = render_content(&config, e, stat)?;
        if content.is_empty() {
            return Ok(());
        }
        self.send_with_config(&config, content)
    }

    fn notify_test(&self) -> Result<()> {
        let config = crate::admin::effective_bark_config(self.config);
        self.send_with_config(&config, "❗ServerStatus test msg".to_string())
    }
}

async fn deliver_bark(
    http_client: &reqwest::Client,
    endpoint: &str,
    timeout: u64,
    payload: &HashMap<String, String>,
) -> Result<()> {
    let response = send_with_retry(|| {
        http_client
            .post(endpoint)
            .timeout(Duration::from_secs(timeout))
            .json(payload)
            .send()
    })
    .await?;
    let status = response.status();
    let body = response.text().await.map_err(|_| anyhow!("invalid Bark response"))?;
    validate_bark_response(status, &body)
}

fn validate_bark_response(status: reqwest::StatusCode, body: &str) -> Result<()> {
    if !status.is_success() {
        return Err(anyhow!("Bark request failed with HTTP status {}", status.as_u16()));
    }
    let response: serde_json::Value = serde_json::from_str(body).map_err(|_| anyhow!("invalid Bark response"))?;
    let code = response.get("code").ok_or_else(|| anyhow!("invalid Bark response"))?;
    let accepted = code.as_i64().is_some_and(|value| value == 0 || value == 200)
        || code.as_str().is_some_and(|value| value == "0" || value == "200");
    if accepted {
        Ok(())
    } else {
        Err(anyhow!("Bark provider rejected notification"))
    }
}

fn prepare_delivery(config: &Config, body: String) -> Result<(String, HashMap<String, String>, u64)> {
    let mut normalized = config.clone();
    if let Some((server, device_key)) = split_server_and_device_key(&normalized.server) {
        normalized.server = server;
        if normalized.device_key.trim().is_empty() {
            normalized.device_key = device_key;
        }
    }
    if !normalized.is_ready() || normalized.device_key.trim().is_empty() {
        return Err(anyhow!("Bark notifier is not ready"));
    }

    let server = normalized.server.trim_end_matches('/');
    let endpoint = if server.ends_with("/push") {
        server.to_string()
    } else {
        format!("{server}/push")
    };
    let payload = Bark::payload(&normalized, body);
    Ok((endpoint, payload, normalized.timeout.max(1)))
}

fn split_server_and_device_key(input: &str) -> Option<(String, String)> {
    let value = input.trim().trim_end_matches('/');
    let (scheme, rest) = value
        .strip_prefix("https://")
        .map(|rest| ("https", rest))
        .or_else(|| value.strip_prefix("http://").map(|rest| ("http", rest)))?;
    let (authority, path) = rest.split_once('/')?;
    let device_key = path.split('/').find(|part| !part.trim().is_empty())?.trim();
    if device_key.eq_ignore_ascii_case("push") {
        return None;
    }
    if !authority.eq_ignore_ascii_case("api.day.app") && device_key.chars().count() < 12 {
        return None;
    }
    Some((format!("{scheme}://{authority}"), device_key.to_string()))
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
        .add_template("bark", source)
        .map_err(|_| anyhow!("invalid Bark template"))?;
    let rendered = environment
        .get_template("bark")
        .map_err(|_| anyhow!("invalid Bark template"))?
        .render(context!(host => stat, config => config, ip_info => stat.ip_info, sys_info => stat.sys_info))
        .map_err(|_| anyhow!("failed to render Bark template"))?;
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
    fn disabled_malformed_bark_constructor_does_not_panic() {
        let config = Box::leak(Box::new(Config {
            enabled: false,
            online_tpl: "{{ sentinel-template-secret".into(),
            ..Default::default()
        }));

        let notifier = Bark::new(config);

        assert_eq!(crate::notifier::Notifier::kind(&notifier), "bark");
    }

    #[test]
    fn enabled_malformed_bark_template_returns_redacted_error() {
        let config = Box::leak(Box::new(Config {
            enabled: true,
            server: "https://api.day.app".into(),
            device_key: "sentinel-device-secret".into(),
            online_tpl: "{{ sentinel-template-secret".into(),
            ..Default::default()
        }));
        let notifier = Bark::new(config);

        let error = crate::notifier::Notifier::notify(&notifier, &Event::NodeUp, &HostStat::default())
            .unwrap_err()
            .to_string();

        assert_eq!(error, "invalid Bark template");
        assert!(!error.contains("sentinel-template-secret"));
        assert!(!error.contains("sentinel-device-secret"));
    }

    #[test]
    fn bark_rendering_selects_each_current_event_template() {
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
    fn full_bark_api_url_is_normalized_for_delivery() {
        let config = Config {
            enabled: true,
            server: "https://api.day.app/abcdefghijklmnopqrstuv".into(),
            device_key: String::new(),
            ..Default::default()
        };

        let (endpoint, payload, _) = prepare_delivery(&config, "body".into()).unwrap();

        assert_eq!(endpoint, "https://api.day.app/push");
        assert_eq!(
            payload.get("device_key").map(String::as_str),
            Some("abcdefghijklmnopqrstuv")
        );
    }

    #[test]
    fn separate_bark_server_and_device_key_are_preserved_for_delivery() {
        let config = Config {
            enabled: true,
            server: "https://bark.example".into(),
            device_key: "separate-device-key".into(),
            ..Default::default()
        };

        let (endpoint, payload, _) = prepare_delivery(&config, "body".into()).unwrap();

        assert_eq!(endpoint, "https://bark.example/push");
        assert_eq!(
            payload.get("device_key").map(String::as_str),
            Some("separate-device-key")
        );
    }

    #[test]
    fn bark_response_requires_http_success_and_provider_success_code() {
        for body in [r#"{"code":0}"#, r#"{"code":200}"#, r#"{"code":"200"}"#] {
            assert!(validate_bark_response(reqwest::StatusCode::OK, body).is_ok());
        }
        assert!(validate_bark_response(reqwest::StatusCode::OK, r#"{"code":400}"#).is_err());
        assert!(validate_bark_response(reqwest::StatusCode::SERVICE_UNAVAILABLE, r#"{"code":200}"#,).is_err());
    }
}
