#![deny(warnings)]
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use uuid::Uuid;

use crate::expiry::{BillingConfig, ExpireNotifyConfig};
use crate::notifier;

fn default_as_true() -> bool {
    true
}
fn default_grpc_addr() -> String {
    "0.0.0.0:9394".to_string()
}
fn default_http_addr() -> String {
    "0.0.0.0:8080".to_string()
}
fn default_workspace() -> String {
    "/opt/ServerStatus".to_string()
}
fn default_tls_dir() -> String {
    "tls".to_string()
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Host {
    pub name: String,
    pub password: String,
    #[serde(default = "Default::default")]
    pub alias: String,
    #[serde(default = "Default::default")]
    pub location: String,
    #[serde(default = "Default::default")]
    pub r#type: String,
    #[serde(default = "u32::default")]
    pub monthstart: u32,
    #[serde(default = "default_as_true")]
    pub notify: bool,
    #[serde(default = "bool::default")]
    pub disabled: bool,
    #[serde(default = "Default::default")]
    pub labels: String,
    #[serde(default = "Default::default")]
    pub expire: String,
    #[serde(default = "Default::default")]
    pub billing: BillingConfig,
    #[serde(default = "default_as_true")]
    pub expire_notify: bool,

    #[serde(skip_deserializing)]
    pub last_network_in: u64,
    #[serde(skip_deserializing)]
    pub last_network_out: u64,

    // user data
    #[serde(skip_serializing, skip_deserializing)]
    pub pos: usize,
    #[serde(default = "Default::default", skip_serializing)]
    pub weight: u64,
    #[serde(default = "Default::default")]
    pub gid: String,
    #[serde(default = "Default::default")]
    pub latest_ts: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HostGroup {
    pub gid: String,
    pub password: String,
    #[serde(default = "Default::default")]
    pub location: String,
    #[serde(default = "Default::default")]
    pub r#type: String,
    #[serde(default = "default_as_true")]
    pub notify: bool,
    // user data
    #[serde(skip_serializing, skip_deserializing)]
    pub pos: usize,
    #[serde(default = "Default::default", skip_serializing)]
    pub weight: u64,
    #[serde(default = "Default::default")]
    pub labels: String,
    #[serde(default = "Default::default")]
    pub expire: String,
    #[serde(default = "Default::default")]
    pub billing: BillingConfig,
    #[serde(default = "default_as_true")]
    pub expire_notify: bool,
}

impl HostGroup {
    pub fn inst_host(&self, name: &str) -> Host {
        Host {
            name: name.to_owned(),
            gid: self.gid.clone(),
            password: self.password.clone(),
            location: self.location.clone(),
            r#type: self.r#type.clone(),
            monthstart: 1,
            notify: self.notify,
            pos: self.pos,
            weight: self.weight,
            labels: self.labels.clone(),
            expire: self.expire.clone(),
            billing: self.billing.clone(),
            expire_notify: self.expire_notify,
            ..Default::default()
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    #[serde(default = "default_http_addr")]
    pub http_addr: String,
    #[serde(default = "default_grpc_addr")]
    pub grpc_addr: String,
    #[serde(default = "Default::default")]
    pub notify_interval: u64,
    #[serde(default = "Default::default")]
    pub offline_threshold: u64,
    #[serde(default = "Default::default")]
    pub grpc_tls: u32,
    #[serde(default = "default_tls_dir")]
    pub tls_dir: String,
    // admin user & pass
    pub admin_user: Option<String>,
    pub admin_pass: Option<String>,
    pub jwt_secret: Option<String>,

    #[serde(default = "Default::default")]
    pub tgbot: notifier::tgbot::Config,
    #[serde(default = "Default::default")]
    pub wechat: notifier::wechat::Config,
    #[serde(default = "Default::default")]
    pub email: notifier::email::Config,
    #[serde(default = "Default::default")]
    pub bark: notifier::bark::Config,
    #[serde(default = "Default::default")]
    pub log: notifier::log::Config,
    #[serde(default = "Default::default")]
    pub webhook: notifier::webhook::Config,
    #[serde(default = "Default::default")]
    pub expire_notify: ExpireNotifyConfig,

    #[serde(default = "Default::default")]
    pub hosts: Vec<Host>,
    #[serde(default = "Default::default")]
    pub hosts_group: Vec<HostGroup>,
    #[serde(default = "Default::default")]
    pub group_gc: u64,

    // deploy
    #[serde(default = "Default::default")]
    pub server_url: String,
    #[serde(default = "default_workspace")]
    pub workspace: String,

    #[serde(skip_deserializing)]
    pub hosts_map: HashMap<String, Host>,

    #[serde(skip_deserializing)]
    pub hosts_group_map: HashMap<String, HostGroup>,
}

impl Config {
    pub fn auth(&self, user: &str, pass: &str) -> bool {
        if let Some(o) = self.hosts_map.get(user) {
            return pass.eq(o.password.as_str());
        }
        false
    }
    pub fn group_auth(&self, gid: &str, pass: &str) -> bool {
        if let Some(o) = crate::admin::effective_group(&self.hosts_group_map, gid) {
            return pass.eq(o.password.as_str());
        }
        false
    }
    pub fn admin_auth(&self, user: &str, pass: &str) -> bool {
        if let Some(u) = crate::admin::effective_admin_user(self.admin_user.as_deref()) {
            return user.eq(u.as_str()) && crate::admin::admin_password_matches(self.admin_pass.as_deref(), pass);
        }
        false
    }

    pub fn to_admin_json_value(&self) -> Value {
        let admin_data = crate::admin::snapshot();
        let deleted_hosts: std::collections::HashSet<String> = admin_data.deleted_hosts.iter().cloned().collect();
        let hosts: Vec<Value> = self
            .hosts
            .iter()
            .filter(|host| !deleted_hosts.contains(&host.name))
            .map(|host| {
                json!({
                    "name": host.name,
                    "alias": host.alias,
                    "location": host.location,
                    "type": host.r#type,
                    "monthstart": host.monthstart,
                    "notify": host.notify,
                    "disabled": host.disabled,
                    "labels": host.labels,
                    "expire": host.expire,
                    "billing": host.billing,
                    "expire_notify": host.expire_notify,
                    "weight": host.weight,
                    "gid": host.gid,
                    "latest_ts": host.latest_ts,
                })
            })
            .collect();
        let deleted_access_keys: std::collections::HashSet<String> =
            admin_data.deleted_access_keys.iter().cloned().collect();
        let mut seen_groups = std::collections::HashSet::new();
        let mut hosts_group: Vec<Value> = Vec::new();
        for group in &self.hosts_group {
            if deleted_access_keys.contains(&group.gid) {
                continue;
            }
            if let Some(group) = crate::admin::effective_group(&self.hosts_group_map, &group.gid) {
                seen_groups.insert(group.gid.clone());
                hosts_group.push(group_to_admin_json(&group));
            }
        }
        for gid in admin_data.access_keys.keys() {
            if seen_groups.contains(gid) || deleted_access_keys.contains(gid) {
                continue;
            }
            if let Some(group) = crate::admin::effective_group(&self.hosts_group_map, gid) {
                seen_groups.insert(group.gid.clone());
                hosts_group.push(group_to_admin_json(&group));
            }
        }

        json!({
            "notify_interval": self.notify_interval,
            "offline_threshold": self.offline_threshold,
            "admin": {
                "username": crate::admin::effective_admin_user(self.admin_user.as_deref()).unwrap_or_default(),
            },
            "expire_notify": self.expire_notify,
            "hosts": hosts,
            "hosts_group": hosts_group,
            "tgbot": {
                "enabled": self.tgbot.enabled,
                "bot_token": "",
                "bot_token_configured": is_configured_secret(&self.tgbot.bot_token),
                "chat_id": "",
                "chat_id_configured": is_configured_secret(&self.tgbot.chat_id),
                "title": self.tgbot.title,
                "expire_tpl": self.tgbot.expire_tpl,
                "health_tpl": self.tgbot.health_tpl,
            },
            "bark": {
                "enabled": self.bark.enabled,
                "server": public_bark_server(&self.bark.server),
                "device_key": "",
                "device_key_configured": is_configured_secret(&self.bark.device_key)
                    || bark_server_contains_key(&self.bark.server),
                "title": self.bark.title,
                "group": self.bark.group,
                "icon": self.bark.icon,
                "sound": self.bark.sound,
                "url": self.bark.url,
                "timeout": self.bark.timeout,
                "expire_tpl": self.bark.expire_tpl,
                "health_tpl": self.bark.health_tpl,
            },
        })
    }

    // pub fn to_string(&self) -> Result<String> {
    //     serde_json::to_string(&self).map_err(anyhow::Error::new)
    // }
}

fn is_configured_secret(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && !value.starts_with('<') && !value.ends_with('>')
}

fn public_bark_server(server: &str) -> String {
    let server = server.trim().trim_end_matches('/');
    let Some((scheme, rest)) = server
        .strip_prefix("https://")
        .map(|rest| ("https", rest))
        .or_else(|| server.strip_prefix("http://").map(|rest| ("http", rest)))
    else {
        return server.to_string();
    };
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() {
        server.to_string()
    } else {
        format!("{scheme}://{authority}")
    }
}

fn bark_server_contains_key(server: &str) -> bool {
    let server = server.trim().trim_end_matches('/');
    let Some(rest) = server
        .strip_prefix("https://")
        .or_else(|| server.strip_prefix("http://"))
    else {
        return false;
    };
    let Some((_, path)) = rest.split_once('/') else {
        return false;
    };
    path.split('/')
        .find(|part| !part.trim().is_empty())
        .is_some_and(|part| !part.eq_ignore_ascii_case("push"))
}

fn group_to_admin_json(group: &HostGroup) -> Value {
    json!({
        "gid": group.gid,
        "location": group.location,
        "type": group.r#type,
        "notify": group.notify,
        "labels": group.labels,
        "expire": group.expire,
        "billing": group.billing,
        "expire_notify": group.expire_notify,
        "weight": group.weight,
    })
}

pub fn from_str(content: &str) -> Option<Config> {
    let mut o = toml::from_str::<Config>(content).unwrap();
    o.hosts_map = HashMap::new();

    for (idx, host) in o.hosts.iter_mut().enumerate() {
        host.pos = idx;
        if host.alias.is_empty() {
            host.alias = host.name.clone();
        }
        if host.monthstart < 1 || host.monthstart > 31 {
            host.monthstart = 1;
        }
        if host.weight == 0 {
            host.weight = 10000_u64 - idx as u64;
        }
        o.hosts_map.insert(host.name.clone(), host.clone());
    }

    for (idx, group) in o.hosts_group.iter_mut().enumerate() {
        group.pos = idx;
        if group.weight == 0 {
            group.weight = (10000 - (1 + idx) * 100) as u64;
        }
        o.hosts_group_map.insert(group.gid.clone(), group.clone());
    }

    if o.offline_threshold < 30 {
        o.offline_threshold = 30;
    }
    if o.notify_interval < 30 {
        o.notify_interval = 30;
    }
    if o.expire_notify.interval < 60 {
        o.expire_notify.interval = 60;
    }
    o.expire_notify.days.retain(|day| *day >= 0);
    o.expire_notify.days.sort_unstable();
    o.expire_notify.days.dedup();
    if o.group_gc < 30 {
        o.group_gc = 30;
    }

    if let Some(user) = o.admin_user.as_deref().map(str::trim).filter(|user| !user.is_empty()) {
        o.admin_user = Some(user.to_string());
    } else {
        o.admin_user = Some("admin".to_string());
    }
    let generated_admin_pass = o
        .admin_pass
        .as_deref()
        .map(str::trim)
        .filter(|pass| !pass.is_empty())
        .is_none();
    if generated_admin_pass {
        o.admin_pass = Some(Uuid::new_v4().to_string());
    } else if let Some(pass) = o.admin_pass.as_deref().map(str::trim) {
        o.admin_pass = Some(pass.to_string());
    }
    let generated_jwt_secret = o
        .jwt_secret
        .as_deref()
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
        .is_none();
    if generated_jwt_secret {
        o.jwt_secret = Some(Uuid::new_v4().to_string());
    } else if let Some(secret) = o.jwt_secret.as_deref().map(str::trim) {
        o.jwt_secret = Some(secret.to_string());
    }

    eprintln!("✨ admin_user: {}", o.admin_user.as_ref()?);
    if should_print_generated_admin_pass(generated_admin_pass, crate::admin::admin_password_override_configured()) {
        eprintln!("✨ admin_pass: {}", o.admin_pass.as_ref()?);
    } else if generated_admin_pass {
        eprintln!("✨ admin_pass: configured in admin override (hidden)");
    } else {
        eprintln!("✨ admin_pass: configured (hidden)");
    }
    if generated_jwt_secret {
        eprintln!("✨ jwt_secret: generated for this startup");
    } else {
        eprintln!("✨ jwt_secret: configured (hidden)");
    }

    Some(o)
}

fn should_print_generated_admin_pass(generated_admin_pass: bool, admin_override_configured: bool) -> bool {
    generated_admin_pass && !admin_override_configured
}

#[cfg(test)]
mod tests {
    use super::should_print_generated_admin_pass;

    #[test]
    fn generated_admin_password_is_hidden_when_override_exists() {
        assert!(should_print_generated_admin_pass(true, false));
        assert!(!should_print_generated_admin_pass(true, true));
        assert!(!should_print_generated_admin_pass(false, false));
    }
}

pub fn from_file(cfg: &str) -> Option<Config> {
    fs::read_to_string(cfg)
        .map(|contents| from_str(contents.as_str()))
        .ok()?
}

pub fn test_from_file(cfg: &str) -> Result<Config> {
    fs::read_to_string(cfg)
        .map(|contents| toml::from_str::<Config>(&contents))
        .unwrap()
        .map_err(anyhow::Error::new)
}
