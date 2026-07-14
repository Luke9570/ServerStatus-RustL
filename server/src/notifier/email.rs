#![deny(warnings)]
use anyhow::{anyhow, Result};
use lettre::{
    message::{header, Mailbox, Mailboxes, MultiPart, SinglePart},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use log::{error, info};
use minijinja::context;
use serde::{Deserialize, Serialize};

use crate::notifier::{Event, HostStat, NOTIFIER_HANDLE};

const KIND: &str = "email";

fn default_expire_tpl() -> String {
    "{{config.title}}<pre>{{host.location}} {{host.name}} {{host.expire.label}}</pre><pre>Expire: {{host.expire.date}}</pre>".to_string()
}

fn default_health_tpl() -> String {
    "{{config.title}}<pre>{{host.custom}}</pre>".to_string()
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    pub enabled: bool,
    pub server: String,
    pub username: String,
    pub password: String,
    pub to: String,
    pub subject: String,
    pub title: String,
    pub online_tpl: String,
    pub offline_tpl: String,
    pub custom_tpl: String,
    #[serde(default = "default_expire_tpl")]
    pub expire_tpl: String,
    #[serde(default = "default_health_tpl")]
    pub health_tpl: String,
}

pub struct Email {
    config: &'static Config,
}

impl Email {
    pub fn new(cfg: &'static Config) -> Self {
        Self { config: cfg }
    }

    fn send_with_config(config: &Config, html_content: String) -> Result<()> {
        if !config.enabled {
            return Ok(());
        }
        if !config.is_ready() {
            return Err(anyhow!("email notifier is not ready"));
        }

        let email = build_message(config, &html_content)?;
        let creds = Credentials::new(config.username.clone(), config.password.clone());
        let mailer: AsyncSmtpTransport<Tokio1Executor> =
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.server)
                .map_err(|_| anyhow!("invalid SMTP relay"))?
                .credentials(creds)
                .build();
        let handle = NOTIFIER_HANDLE
            .lock()
            .map_err(|_| anyhow!("notification runtime lock is unavailable"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("notification runtime is unavailable"))?;

        handle.spawn(async move {
            match mailer.send(email).await {
                Ok(_) => info!("email sent successfully"),
                Err(_) => error!("email delivery failed"),
            }
        });

        Ok(())
    }
}

impl crate::notifier::Notifier for Email {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn send_notify(&self, html_content: String) -> Result<()> {
        let config = crate::admin::effective_email_config(self.config);
        Self::send_with_config(&config, html_content)
    }

    fn notify(&self, e: &Event, stat: &HostStat) -> Result<()> {
        let config = crate::admin::effective_email_config(self.config);
        if !config.enabled {
            return Ok(());
        }
        if !config.is_ready() {
            return Err(anyhow!("email notifier is not ready"));
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
        Self::send_with_config(&config, content)
    }

    fn notify_test(&self) -> Result<()> {
        let config = crate::admin::effective_email_config(self.config);
        Self::send_with_config(&config, "❗ServerStatus test msg".to_string())
    }
}

fn build_message(config: &Config, html_content: &str) -> Result<Message> {
    let sender = format!("ServerStatus <{}>", config.username)
        .parse::<Mailbox>()
        .map_err(|_| anyhow!("invalid sender address"))?;
    let recipients = config
        .to
        .parse::<Mailboxes>()
        .map_err(|_| anyhow!("invalid recipient addresses"))?;
    let mut builder = Message::builder().subject(config.subject.clone()).from(sender);
    for mailbox in recipients.iter() {
        builder = builder.to(mailbox.clone());
    }

    builder
        .multipart(
            MultiPart::alternative().singlepart(
                SinglePart::builder()
                    .header(header::ContentType::TEXT_HTML)
                    .body(html_content.to_string()),
            ),
        )
        .map_err(|_| anyhow!("invalid email message"))
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
        .add_template("email", source)
        .map_err(|_| anyhow!("invalid email template"))?;
    let rendered = environment
        .get_template("email")
        .map_err(|_| anyhow!("invalid email template"))?
        .render(context!(host => stat, config => config, ip_info => stat.ip_info, sys_info => stat.sys_info))
        .map_err(|_| anyhow!("failed to render email template"))?;
    Ok(trim_rendered(&rendered))
}

fn trim_rendered(content: &str) -> String {
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_email_addresses_return_error_without_panic() {
        let config = Config {
            enabled: true,
            username: "not-an-email".into(),
            to: "bad".into(),
            ..Default::default()
        };

        assert!(build_message(&config, "test").is_err());
    }

    #[test]
    fn invalid_email_template_returns_error_without_panic() {
        let config = Config {
            enabled: true,
            online_tpl: "{{ invalid".into(),
            ..Default::default()
        };

        assert!(render_content(&config, &Event::NodeUp, &HostStat::default()).is_err());
    }
}
