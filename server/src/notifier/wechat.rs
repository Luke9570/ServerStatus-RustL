#![deny(warnings)]
use anyhow::{anyhow, Result};
use log::{error, info};
use minijinja::context;
use reqwest;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;
use tokio::time::Duration;

use crate::notifier::{Event, HostStat, NOTIFIER_HANDLE};

// https://qydev.weixin.qq.com/wiki/index.php?title=%E4%B8%BB%E5%8A%A8%E8%B0%83%E7%94%A8
// https://qydev.weixin.qq.com/wiki/index.php?title=%E5%8F%91%E9%80%81%E6%8E%A5%E5%8F%A3%E8%AF%B4%E6%98%8E
static TOKEN_URL: &str = "https://qyapi.weixin.qq.com/cgi-bin/gettoken";
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
        if !config.is_ready() {
            return Err(anyhow!("WeChat notifier is not ready"));
        }

        let mut data = HashMap::new();
        data.insert("corpid", config.corp_id.clone());
        data.insert("corpsecret", config.corp_secret.clone());

        let http_client = self.http_client.clone();
        let handle = NOTIFIER_HANDLE
            .lock()
            .map_err(|_| anyhow!("notification runtime lock is unavailable"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("notification runtime is unavailable"))?;
        let agent_id = config.agent_id.clone();
        handle.spawn(async move {
            match http_client
                .post(TOKEN_URL)
                .timeout(Duration::from_secs(5))
                .json(&data)
                .send()
                .await
            {
                Ok(resp) => {
                    let json_res = resp.json::<HashMap<String, serde_json::Value>>().await;
                    if let Ok(json_data) = json_res {
                        if let Some(token) = json_data
                            .get("access_token")
                            .and_then(serde_json::Value::as_str)
                        {
                            let req_url =
                                format!("https://qyapi.weixin.qq.com/cgi-bin/message/send?access_token={token}");
                            let req_data = serde_json::json!({
                                "touser": "@all",
                                "agentid": agent_id,
                                "msgtype": "text",
                                "text": {
                                    "content": text_content,
                                },
                                "safe": 0
                            });

                            match http_client
                                .post(&req_url)
                                .timeout(Duration::from_secs(5))
                                .json(&req_data)
                                .send()
                                .await
                            {
                                Ok(resp) => {
                                    info!("wechat send message status => {}", resp.status());
                                }
                                Err(_) => error!("wechat send message failed"),
                            }
                        }
                    }
                }
                Err(_) => error!("wechat access token request failed"),
            }
        });

        Ok(())
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
    let mut environment = minijinja::Environment::new();
    environment
        .add_template("wechat", source)
        .map_err(|_| anyhow!("invalid WeChat template"))?;
    let rendered = environment
        .get_template("wechat")
        .map_err(|_| anyhow!("invalid WeChat template"))?
        .render(context!(host => stat, config => config, ip_info => stat.ip_info, sys_info => stat.sys_info))
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
}
