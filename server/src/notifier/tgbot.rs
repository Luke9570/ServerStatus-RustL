#![deny(warnings)]
use anyhow::Result;
use log::{error, info};
use minijinja::context;
use reqwest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::time::Duration;

use crate::jinja::{add_template, render_template};
use crate::notifier::{get_tag, Event, HostStat, NOTIFIER_HANDLE};

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
        let config = crate::admin::effective_tgbot_config(cfg);
        let o = Self {
            config: cfg,
            http_client: reqwest::Client::new(),
        };

        add_template(KIND, get_tag(&Event::NodeUp), config.online_tpl);
        add_template(KIND, get_tag(&Event::NodeDown), config.offline_tpl);
        add_template(KIND, get_tag(&Event::Custom), config.custom_tpl);
        add_template(KIND, get_tag(&Event::Expire), config.expire_tpl);
        add_template(KIND, get_tag(&Event::Health), config.health_tpl);

        o
    }
}

impl crate::notifier::Notifier for TGBot {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn send_notify(&self, html_content: String) -> Result<()> {
        let config = crate::admin::effective_tgbot_config(self.config);
        let mut data = HashMap::new();
        data.insert("chat_id", config.chat_id.clone());
        data.insert("parse_mode", "HTML".to_string());
        data.insert("text", html_content);

        let tg_url = format!("https://api.telegram.org/bot{}/sendMessage", config.bot_token);
        let handle = NOTIFIER_HANDLE.lock().unwrap().as_ref().unwrap().clone();
        let http_client = self.http_client.clone();
        handle.spawn(async move {
            match http_client
                .post(&tg_url)
                .timeout(Duration::from_secs(5))
                .json(&data)
                .send()
                .await
            {
                Ok(resp) => {
                    info!("tg send msg status => {}", resp.status());
                }
                Err(err) => {
                    error!("tg send msg error => {}", sanitize_tg_error(&err.to_string(), &tg_url));
                }
            }
        });

        Ok(())
    }

    fn notify(&self, e: &Event, stat: &HostStat) -> Result<()> {
        let config = crate::admin::effective_tgbot_config(self.config);
        render_template(
            self.kind(),
            get_tag(e),
            context!(host => stat, config => &config, ip_info => stat.ip_info, sys_info => stat.sys_info),
            true,
        )
        .map(|content| match *e {
            Event::NodeUp | Event::NodeDown | Event::Expire | Event::Health => {
                if !content.is_empty() {
                    self.send_notify(content).unwrap();
                }
            }
            Event::Custom => {
                info!("render.custom.tpl => {content}");
                if !content.is_empty() {
                    self.send_notify(format!("{}\n{}", config.title, content))
                        .unwrap_or_else(|err| {
                            error!("send_msg err => {err:?}");
                        });
                }
            }
        })
    }
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
