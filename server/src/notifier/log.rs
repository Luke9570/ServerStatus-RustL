#![deny(warnings)]
use anyhow::{anyhow, Result};
use chrono::Local;
use minijinja::context;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::notifier::{Event, HostStat, NotificationTestError, NotificationTestResult};

const KIND: &str = "log";

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    pub enabled: bool,
    pub log_dir: String,
    pub tpl: String,
}

pub struct Log {
    config: &'static Config,
}

impl Log {
    pub fn new(cfg: &'static Config) -> Self {
        Self { config: cfg }
    }

    fn send_with_config(config: &Config, content: &str) -> Result<()> {
        if !config.enabled || content.is_empty() {
            return Ok(());
        }
        if !config.is_ready() {
            return Err(anyhow!("log notifier is not ready"));
        }

        let date = Local::now().format("%Y-%m-%d").to_string();
        let log_file = effective_log_path(config, &date)?;
        let parent = log_file
            .parent()
            .ok_or_else(|| anyhow!("notification log path has no parent"))?;
        fs::create_dir_all(parent).map_err(|_| anyhow!("failed to create notification log directory"))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
            .map_err(|_| anyhow!("failed to open notification log"))?;
        file.write_all(content.as_bytes())
            .map_err(|_| anyhow!("failed to write notification log"))?;
        if !content.ends_with('\n') {
            file.write_all(b"\n")
                .map_err(|_| anyhow!("failed to write notification log"))?;
        }
        file.flush().map_err(|_| anyhow!("failed to flush notification log"))
    }
}

impl crate::notifier::Notifier for Log {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn send_notify(&self, content: String) -> Result<()> {
        let config = crate::admin::effective_log_config(self.config);
        Self::send_with_config(&config, &content)
    }

    fn notify(&self, e: &Event, stat: &HostStat) -> Result<()> {
        let config = crate::admin::effective_log_config(self.config);
        if !config.enabled {
            return Ok(());
        }
        if !config.is_ready() {
            return Err(anyhow!("log notifier is not ready"));
        }

        let content = render_content(&config, e, stat)?;
        Self::send_with_config(&config, &content)
    }
}

pub(crate) async fn test(config: &Config) -> NotificationTestResult {
    if !config.is_ready() || render_content(config, &Event::Custom, &HostStat::default()).is_err() {
        return Err(NotificationTestError::InvalidConfiguration);
    }
    Log::send_with_config(config, "❗ServerStatus test msg").map_err(|_| NotificationTestError::DeliveryFailed)
}

fn render_content(config: &Config, event: &Event, stat: &HostStat) -> Result<String> {
    let mut environment = minijinja::Environment::new();
    environment
        .add_template("log", &config.tpl)
        .map_err(|_| anyhow!("invalid log template"))?;
    let rendered = environment
        .get_template("log")
        .map_err(|_| anyhow!("invalid log template"))?
        .render(context!(event => event, host => stat, config => config, ip_info => stat.ip_info, sys_info => stat.sys_info))
        .map_err(|_| anyhow!("failed to render log template"))?;
    Ok(rendered
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

fn admin_log_path(working_dir: &Path, date: &str) -> PathBuf {
    working_dir
        .join(crate::admin::ADMIN_NOTIFICATION_LOG_DIR)
        .join(format!("ssr.log.{date}"))
}

fn effective_log_path(config: &Config, date: &str) -> Result<PathBuf> {
    if config.log_dir == crate::admin::ADMIN_NOTIFICATION_LOG_DIR {
        let working_dir =
            std::env::current_dir().map_err(|_| anyhow!("failed to resolve notification working directory"))?;
        Ok(admin_log_path(&working_dir, date))
    } else {
        Ok(Path::new(&config.log_dir).join(format!("ssr.log.{date}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_log_path_is_fixed_below_notifications_directory() {
        let path = admin_log_path(Path::new("/data"), "2026-07-14");

        assert_eq!(path, Path::new("/data/notifications/ssr.log.2026-07-14"));
    }

    #[test]
    fn invalid_log_template_returns_error_without_panic() {
        let config = Config {
            enabled: true,
            tpl: "{{ invalid".into(),
            ..Default::default()
        };

        assert!(render_content(&config, &Event::NodeUp, &HostStat::default()).is_err());
    }
}
