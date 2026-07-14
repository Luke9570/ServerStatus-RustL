#![deny(warnings)]
use anyhow::{anyhow, Result};
use log::{error, info};
use minijinja::context;
use reqwest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::time::Duration;

use crate::notifier::{Event, HostStat, NOTIFIER_HANDLE};

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
        if !config.is_ready() {
            return Err(anyhow!("Telegram notifier is not ready"));
        }

        let mut data = HashMap::new();
        data.insert("chat_id", config.chat_id.clone());
        data.insert("parse_mode", "HTML".to_string());
        data.insert("text", html_content);

        let tg_url = format!("https://api.telegram.org/bot{}/sendMessage", config.bot_token);
        let handle = NOTIFIER_HANDLE
            .lock()
            .map_err(|_| anyhow!("notification runtime lock is unavailable"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("notification runtime is unavailable"))?;
        let http_client = self.http_client.clone();
        handle.spawn(async move {
            match http_client
                .post(&tg_url)
                .timeout(Duration::from_secs(5))
                .json(&data)
                .send()
                .await
            {
                Ok(resp) => info!("tg send msg status => {}", resp.status()),
                Err(err) => {
                    error!(
                        "tg send msg error => {}",
                        sanitize_tg_error(&err.to_string(), &tg_url)
                    );
                }
            }
        });

        Ok(())
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
        .render(
            context!(host => stat, config => config, ip_info => stat.ip_info, sys_info => stat.sys_info),
        )
        .map_err(|_| anyhow!("failed to render Telegram template"))?;
    Ok(rendered
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

fn sanitize_tg_error(message: &str, tg_url: &str) -> String {
    let mut sanitized = message.replace(tg_url, "https://api.telegram.org/bot[redacted]/sendMessage");
    if let Some((_, rest)) = tg_url.split_once("/bot") {
        if let Some((token, _)) = rest.split_once("/sendMessage") {
            sanitized = sanitized.replace(token, "[redacted]");
        }
    }
    sanitized
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

        let error = crate::notifier::Notifier::notify(
            &notifier,
            &Event::NodeUp,
            &HostStat::default(),
        )
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
}
