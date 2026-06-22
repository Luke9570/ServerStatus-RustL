use anyhow::Result;
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use crate::config::Host;
use crate::expiry::{BillingConfig, ExpireNotifyConfig};
use crate::notifier;

const SETTINGS_PATH: &str = "admin-overrides.json";

static ADMIN_STATE: OnceCell<AdminState> = OnceCell::new();

struct AdminState {
    path: String,
    data: Mutex<AdminData>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BillingOverride {
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub end_date: Option<String>,
    #[serde(default)]
    pub auto_renewal: Option<String>,
    #[serde(default)]
    pub cycle: Option<String>,
    #[serde(default)]
    pub amount: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeOverride {
    #[serde(default)]
    pub expire: Option<String>,
    #[serde(default)]
    pub billing: BillingOverride,
    #[serde(default)]
    pub expire_notify: Option<bool>,
    #[serde(default)]
    pub weight: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TgbotOverride {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bot_token: String,
    #[serde(default)]
    pub chat_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub expire_tpl: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BarkOverride {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub device_key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub sound: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub expire_tpl: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdminData {
    #[serde(default)]
    pub hosts: HashMap<String, NodeOverride>,
    #[serde(default)]
    pub groups: HashMap<String, NodeOverride>,
    #[serde(default)]
    pub expire_notify: Option<ExpireNotifyConfig>,
    #[serde(default)]
    pub tgbot: Option<TgbotOverride>,
    #[serde(default)]
    pub bark: Option<BarkOverride>,
}

pub fn init() -> Result<()> {
    let data = fs::read_to_string(SETTINGS_PATH)
        .ok()
        .and_then(|contents| serde_json::from_str::<AdminData>(&contents).ok())
        .unwrap_or_default();
    let _ = ADMIN_STATE.set(AdminState {
        path: SETTINGS_PATH.to_string(),
        data: Mutex::new(data),
    });
    Ok(())
}

pub fn snapshot() -> AdminData {
    ADMIN_STATE
        .get()
        .and_then(|state| state.data.lock().ok().map(|data| data.clone()))
        .unwrap_or_default()
}

pub fn replace(data: AdminData) -> Result<AdminData> {
    let state = ADMIN_STATE.get().expect("admin state not initialized");
    if let Some(parent) = Path::new(&state.path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    fs::write(&state.path, serde_json::to_string_pretty(&data)?)?;
    *state.data.lock().unwrap() = data.clone();
    Ok(data)
}

pub fn apply_host_override(host: &mut Host) {
    let data = snapshot();
    if !host.gid.is_empty() {
        if let Some(override_data) = data.groups.get(&host.gid) {
            override_data.apply_to(host);
        }
    }
    if let Some(override_data) = data.hosts.get(&host.name) {
        override_data.apply_to(host);
    }
}

pub fn effective_expire_notify(base: &ExpireNotifyConfig) -> ExpireNotifyConfig {
    snapshot().expire_notify.unwrap_or_else(|| base.clone())
}

pub fn tgbot_enabled() -> bool {
    snapshot().tgbot.is_some_and(|cfg| cfg.enabled)
}

pub fn bark_enabled() -> bool {
    snapshot().bark.is_some_and(|cfg| cfg.enabled)
}

pub fn effective_tgbot_config(base: &notifier::tgbot::Config) -> notifier::tgbot::Config {
    let mut cfg = base.clone();
    if let Some(override_data) = snapshot().tgbot {
        cfg.enabled = override_data.enabled;
        override_string(&mut cfg.bot_token, override_data.bot_token);
        override_string(&mut cfg.chat_id, override_data.chat_id);
        override_string(&mut cfg.title, override_data.title);
        override_string(&mut cfg.expire_tpl, override_data.expire_tpl);
    }
    cfg
}

pub fn effective_bark_config(base: &notifier::bark::Config) -> notifier::bark::Config {
    let mut cfg = base.clone();
    if let Some(override_data) = snapshot().bark {
        cfg.enabled = override_data.enabled;
        override_string(&mut cfg.server, override_data.server);
        override_string(&mut cfg.device_key, override_data.device_key);
        override_string(&mut cfg.title, override_data.title);
        override_string(&mut cfg.group, override_data.group);
        override_string(&mut cfg.icon, override_data.icon);
        override_string(&mut cfg.sound, override_data.sound);
        override_string(&mut cfg.url, override_data.url);
        override_string(&mut cfg.expire_tpl, override_data.expire_tpl);
        if let Some(timeout) = override_data.timeout {
            cfg.timeout = timeout;
        }
    }
    cfg
}

impl NodeOverride {
    fn apply_to(&self, host: &mut Host) {
        if let Some(expire) = &self.expire {
            host.expire.clone_from(expire);
        }
        self.billing.apply_to(&mut host.billing);
        if let Some(expire_notify) = self.expire_notify {
            host.expire_notify = expire_notify;
        }
        if let Some(weight) = self.weight {
            host.weight = weight;
        }
    }
}

impl BillingOverride {
    fn apply_to(&self, billing: &mut BillingConfig) {
        override_option_string(&mut billing.start_date, &self.start_date);
        override_option_string(&mut billing.end_date, &self.end_date);
        override_option_string(&mut billing.auto_renewal, &self.auto_renewal);
        override_option_string(&mut billing.cycle, &self.cycle);
        override_option_string(&mut billing.amount, &self.amount);
    }
}

fn override_option_string(target: &mut String, value: &Option<String>) {
    if let Some(value) = value {
        target.clone_from(value);
    }
}

fn override_string(target: &mut String, value: String) {
    if !value.trim().is_empty() {
        *target = value;
    }
}
