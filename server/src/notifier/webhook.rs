#![deny(warnings)]
// #![allow(unused)]
use anyhow::Result;
use chrono::Local;
use reqwest;
use rhai::serde::{from_dynamic, to_dynamic};
use rhai::{Array, Dynamic, Engine, ImmutableString, Scope, AST};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::time::Duration;

use crate::notifier::{get_tag, Event, HostStat, NOTIFIER_HANDLE};

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
        let mut request = http_client
            .post(&receiver.url)
            .timeout(Duration::from_secs(receiver.timeout.into()))
            .body(reqwest::Body::from(content.into_bytes()));

        for (name, value) in &receiver.headers {
            request = request.header(name, value);
        }

        if let (Some(user), Some(password)) = (receiver.username.as_ref(), receiver.password.as_ref()) {
            if !user.is_empty() && !password.is_empty() {
                request = request.basic_auth(user, Some(password));
            }
        }
        let request = request
            .build()
            .map_err(|_| anyhow::anyhow!("invalid legacy webhook request"))?;
        let handle = NOTIFIER_HANDLE
            .lock()
            .map_err(|_| anyhow::anyhow!("notification runtime lock is unavailable"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("notification runtime is unavailable"))?;
        handle.spawn(async move {
            match http_client.execute(request).await {
                Ok(resp) => info!("webhook send message status => {}", resp.status()),
                Err(_) => error!("webhook send message failed"),
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

    fn send_notify(&self, _content: String) -> Result<()> {
        Ok(())
    }

    fn notify_test(&self) -> Result<()> {
        let mut attempted = 0_usize;
        let mut succeeded = 0_usize;
        for (idx, receiver) in self.config.receiver.iter().enumerate() {
            if !receiver.enabled
                || self
                    .ast_list
                    .get(idx)
                    .and_then(Option::as_ref)
                    .is_none()
            {
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
        self.execute_receivers(e, stat, |receiver, content| {
            self.call_webhook(receiver, content)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(crate::notifier::Notifier::notify(
            &notifier,
            &Event::NodeDown,
            &HostStat::default(),
        )
        .is_ok());
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

        let result = notifier.execute_receivers(
            &Event::NodeDown,
            &HostStat::default(),
            |_receiver, content| {
                deliveries.push(content);
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert_eq!(deliveries.len(), 1);
        assert!(deliveries[0].contains("recorded"));
        let context = legacy_receiver_failure_context(
            0,
            &anyhow::anyhow!("sentinel-script-secret"),
        );
        assert_eq!(
            context,
            "legacy webhook receiver failed: index=0, error=notification failed"
        );
        assert!(!context.contains("sentinel-script-secret"));
        assert!(!context.contains("sentinel-webhook-secret"));
    }
}
