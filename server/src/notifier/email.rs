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

use crate::notifier::{Event, HostStat, NotificationTestError, NotificationTestResult, NOTIFIER_HANDLE};

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

#[derive(Serialize)]
struct TemplateConfig<'a> {
    enabled: bool,
    server: &'a str,
    username: &'a str,
    password: &'static str,
    to: &'a str,
    subject: &'a str,
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
            server: &config.server,
            username: &config.username,
            password: redacted_template_secret(&config.password),
            to: &config.to,
            subject: &config.subject,
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
        let (mailer, email) = prepare_delivery(config, &html_content)?;
        let handle = NOTIFIER_HANDLE
            .lock()
            .map_err(|_| anyhow!("notification runtime lock is unavailable"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("notification runtime is unavailable"))?;

        handle.spawn(async move {
            match deliver_email(&mailer, email).await {
                Ok(()) => info!("email sent successfully"),
                Err(()) => error!("email delivery failed"),
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

pub(crate) async fn test(config: &Config) -> NotificationTestResult {
    validate_test_config(config).map_err(|_| NotificationTestError::InvalidConfiguration)?;
    let (mailer, email) =
        prepare_delivery(config, "❗ServerStatus test msg").map_err(|_| NotificationTestError::InvalidConfiguration)?;
    deliver_email(&mailer, email)
        .await
        .map_err(|()| NotificationTestError::DeliveryFailed)
}

fn prepare_delivery(config: &Config, html_content: &str) -> Result<(AsyncSmtpTransport<Tokio1Executor>, Message)> {
    if !config.is_ready() {
        return Err(anyhow!("email notifier is not ready"));
    }
    let email = build_message(config, html_content)?;
    let creds = Credentials::new(config.username.clone(), config.password.clone());
    let mailer = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.server)
        .map_err(|_| anyhow!("invalid SMTP relay"))?
        .credentials(creds)
        .build();
    Ok((mailer, email))
}

async fn deliver_email(mailer: &AsyncSmtpTransport<Tokio1Executor>, email: Message) -> std::result::Result<(), ()> {
    mailer.send(email).await.map(|_| ()).map_err(|_| ())
}

fn validate_test_config(config: &Config) -> Result<()> {
    if !config.is_ready() {
        return Err(anyhow!("email notifier is not ready"));
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

fn build_message(config: &Config, html_content: &str) -> Result<Message> {
    let sender = format!("ServerStatus <{}>", config.username)
        .parse::<Mailbox>()
        .map_err(|_| anyhow!("invalid sender address"))?;
    let normalized_recipients = config
        .to
        .split([';', ','])
        .map(str::trim)
        .filter(|recipient| !recipient.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let recipients = normalized_recipients
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
    let safe_config = TemplateConfig::from(config);
    let mut environment = minijinja::Environment::new();
    environment
        .add_template("email", source)
        .map_err(|_| anyhow!("invalid email template"))?;
    let rendered = environment
        .get_template("email")
        .map_err(|_| anyhow!("invalid email template"))?
        .render(context!(host => stat, config => safe_config, ip_info => stat.ip_info, sys_info => stat.sys_info))
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

    #[test]
    fn semicolon_separated_recipients_build_a_deliverable_message() {
        let config = Config {
            username: "sender@example.com".into(),
            to: "first@example.com; second@example.com".into(),
            subject: "Test".into(),
            ..Default::default()
        };

        let message = build_message(&config, "test").unwrap();
        let formatted = String::from_utf8(message.formatted()).unwrap();

        assert!(formatted.contains("To: first@example.com, second@example.com"));
    }

    #[test]
    fn email_template_config_redacts_smtp_password() {
        let config = Config {
            server: "smtp.visible.example".into(),
            username: "sender@example.com".into(),
            password: "sentinel-smtp-password".into(),
            online_tpl: "{{ config.server }}|{{ config.username }}|{{ config.password }}".into(),
            ..Default::default()
        };

        let rendered = render_content(&config, &Event::NodeUp, &HostStat::default()).unwrap();

        assert_eq!(rendered, "smtp.visible.example|sender@example.com|[redacted]");
        assert!(!rendered.contains("sentinel-smtp-password"));
    }
}
