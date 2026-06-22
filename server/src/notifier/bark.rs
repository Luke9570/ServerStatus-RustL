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
        }
    }
}

pub struct Bark {
    config: &'static Config,
    http_client: reqwest::Client,
}

impl Bark {
    pub fn new(cfg: &'static Config) -> Self {
        let config = crate::admin::effective_bark_config(cfg);

        let o = Self {
            config: cfg,
            http_client: reqwest::Client::new(),
        };

        add_template(KIND, get_tag(&Event::NodeUp), config.online_tpl);
        add_template(KIND, get_tag(&Event::NodeDown), config.offline_tpl);
        add_template(KIND, get_tag(&Event::Custom), config.custom_tpl);
        add_template(KIND, get_tag(&Event::Expire), config.expire_tpl);

        o
    }

    fn payload(&self, body: String) -> HashMap<String, String> {
        let config = crate::admin::effective_bark_config(self.config);
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
}

impl crate::notifier::Notifier for Bark {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn send_notify(&self, body: String) -> Result<()> {
        let config = crate::admin::effective_bark_config(self.config);
        if config.device_key.trim().is_empty() {
            error!("bark device_key is empty");
            return Ok(());
        }

        let server = config.server.trim_end_matches('/');
        let push_url = if server.ends_with("/push") {
            server.to_string()
        } else {
            format!("{server}/push")
        };
        let timeout = config.timeout.max(1);
        let payload = self.payload(body);
        let handle = NOTIFIER_HANDLE.lock().unwrap().as_ref().unwrap().clone();
        let http_client = self.http_client.clone();

        handle.spawn(async move {
            match http_client
                .post(&push_url)
                .timeout(Duration::from_secs(timeout))
                .json(&payload)
                .send()
                .await
            {
                Ok(resp) => {
                    info!("bark send msg resp => {resp:?}");
                }
                Err(err) => {
                    error!("bark send msg error => {err:?}");
                }
            }
        });

        Ok(())
    }

    fn notify(&self, e: &Event, stat: &HostStat) -> Result<()> {
        let config = crate::admin::effective_bark_config(self.config);
        render_template(
            self.kind(),
            get_tag(e),
            context!(host => stat, config => &config, ip_info => stat.ip_info, sys_info => stat.sys_info),
            true,
        )
        .map(|content| {
            if !content.is_empty() {
                self.send_notify(content).unwrap_or_else(|err| {
                    error!("bark send msg err => {err:?}");
                });
            }
        })
    }
}
