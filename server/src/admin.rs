use anyhow::Result;
use once_cell::sync::OnceCell;
use ring::rand::SecureRandom;
use ring::{digest, pbkdf2, rand};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Mutex;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use crate::config::{Host, HostGroup};
use crate::expiry::{BillingConfig, ExpireNotifyConfig};
use crate::notifier;

const SETTINGS_PATH: &str = "admin-overrides.json";
const DEFAULT_ACCESS_KEY_ID: &str = "default";
const ADMIN_PASSWORD_HASH_ALGO: &str = "pbkdf2-sha256";
const ADMIN_PASSWORD_HASH_ITERATIONS: u32 = 210_000;
const ADMIN_PASSWORD_SALT_BYTES: usize = 16;
const ADMIN_PASSWORD_HASH_BYTES: usize = 32;
const MIN_ADMIN_PASSWORD_LEN: usize = 12;
const MAX_ADMIN_PASSWORD_LEN: usize = 256;
const MAX_ADMIN_USERNAME_LEN: usize = 64;
pub const DEFAULT_ADMIN_PATH: &str = "/admin";
const MAX_ADMIN_PATH_LEN: usize = 64;
const INSTALL_TOKEN_TTL_SECONDS: u64 = 24 * 3600;

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
    pub alias: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub public_note: Option<String>,
    #[serde(default)]
    pub spec: Option<String>,
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
pub struct ServerGroupOverride {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub servers: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccessKeyOverride {
    #[serde(default)]
    pub source_gid: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub notify: Option<bool>,
    #[serde(default)]
    pub labels: String,
    #[serde(default)]
    pub expire: String,
    #[serde(default)]
    pub billing: BillingOverride,
    #[serde(default)]
    pub expire_notify: Option<bool>,
    #[serde(default)]
    pub weight: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotificationGroupOverride {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub notifications: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstallTokenOverride {
    #[serde(default)]
    pub gid: String,
    #[serde(default)]
    pub token_hash: String,
    #[serde(default)]
    pub expires_at: u64,
}

fn default_alert_repeat_interval() -> u64 {
    3600
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRuleOverride {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_as_true")]
    pub enabled: bool,
    #[serde(default)]
    pub metric: String,
    #[serde(default)]
    pub threshold: Option<f64>,
    #[serde(default)]
    pub duration: u64,
    #[serde(default = "default_alert_repeat_interval")]
    pub repeat_interval: u64,
    #[serde(default)]
    pub notification_group: String,
    #[serde(default)]
    pub notifications: Vec<String>,
    #[serde(default)]
    pub server_groups: Vec<String>,
    #[serde(default)]
    pub servers: Vec<String>,
}

impl Default for AlertRuleOverride {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            enabled: true,
            metric: String::new(),
            threshold: None,
            duration: 120,
            repeat_interval: default_alert_repeat_interval(),
            notification_group: String::new(),
            notifications: Vec::new(),
            server_groups: Vec::new(),
            servers: Vec::new(),
        }
    }
}

fn default_as_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TgbotOverride {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bot_token: String,
    #[serde(default, skip_serializing_if = "is_false_bool")]
    pub clear_bot_token: bool,
    #[serde(default, skip_deserializing, skip_serializing_if = "is_false_bool")]
    pub bot_token_configured: bool,
    #[serde(default)]
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "is_false_bool")]
    pub clear_chat_id: bool,
    #[serde(default, skip_deserializing, skip_serializing_if = "is_false_bool")]
    pub chat_id_configured: bool,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub expire_tpl: String,
    #[serde(default)]
    pub health_tpl: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BarkOverride {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub device_key: String,
    #[serde(default, skip_serializing_if = "is_false_bool")]
    pub clear_device_key: bool,
    #[serde(default, skip_deserializing, skip_serializing_if = "is_false_bool")]
    pub device_key_configured: bool,
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
    #[serde(default)]
    pub health_tpl: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdminData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_password_hash: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub admin_session_version: u64,
    #[serde(default)]
    pub admin_path: String,
    #[serde(default)]
    pub hosts: HashMap<String, NodeOverride>,
    #[serde(default)]
    pub groups: HashMap<String, NodeOverride>,
    #[serde(default)]
    pub deleted_hosts: Vec<String>,
    #[serde(default)]
    pub server_groups: Vec<ServerGroupOverride>,
    #[serde(default)]
    pub access_keys: HashMap<String, AccessKeyOverride>,
    #[serde(default)]
    pub deleted_access_keys: Vec<String>,
    #[serde(default)]
    pub notification_groups: Vec<NotificationGroupOverride>,
    #[serde(default)]
    pub alert_rules: Vec<AlertRuleOverride>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub install_tokens: HashMap<String, InstallTokenOverride>,
    #[serde(default)]
    pub access_base_url: String,
    #[serde(default)]
    pub agent_base_url: String,
    #[serde(default)]
    pub expire_notify: Option<ExpireNotifyConfig>,
    #[serde(default)]
    pub tgbot: Option<TgbotOverride>,
    #[serde(default)]
    pub bark: Option<BarkOverride>,
}

pub fn init() -> Result<()> {
    let mut data = fs::read_to_string(SETTINGS_PATH)
        .ok()
        .and_then(|contents| serde_json::from_str::<AdminData>(&contents).ok())
        .unwrap_or_default();
    normalize_admin_data(&mut data);
    if validate_admin_path(&data.admin_path).is_err() {
        data.admin_path = DEFAULT_ADMIN_PATH.to_string();
    }
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
    let current = state
        .data
        .lock()
        .ok()
        .map(|current| current.clone())
        .unwrap_or_default();
    let mut data = data;
    merge_sensitive_fields(&mut data, &current);
    normalize_admin_data(&mut data);
    validate_admin_path(&data.admin_path).map_err(|err| anyhow::anyhow!("{err}"))?;
    data.access_base_url = data.access_base_url.trim().trim_end_matches('/').to_string();
    data.agent_base_url = data.agent_base_url.trim().trim_end_matches('/').to_string();
    write_data(state, data)
}

fn write_data(state: &AdminState, data: AdminData) -> Result<AdminData> {
    if let Some(parent) = Path::new(&state.path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    write_settings_file(&state.path, &serde_json::to_string_pretty(&data)?)?;
    *state.data.lock().unwrap() = data.clone();
    Ok(data)
}

fn write_settings_file(path: &str, contents: &str) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

pub fn public_snapshot() -> AdminData {
    let mut data = snapshot();
    if validate_admin_path(&data.admin_path).is_err() {
        data.admin_path = DEFAULT_ADMIN_PATH.to_string();
    }
    data.admin_password_hash = None;
    data.admin_session_version = 0;
    for access_key in data.access_keys.values_mut() {
        access_key.password.clear();
    }
    if let Some(tgbot) = &mut data.tgbot {
        tgbot.bot_token_configured = !tgbot.clear_bot_token && is_configured_secret(&tgbot.bot_token);
        tgbot.chat_id_configured = !tgbot.clear_chat_id && is_configured_secret(&tgbot.chat_id);
        tgbot.bot_token.clear();
        tgbot.chat_id.clear();
    }
    if let Some(bark) = &mut data.bark {
        bark.device_key_configured = !bark.clear_device_key && is_configured_secret(&bark.device_key);
        bark.device_key.clear();
    }
    data.install_tokens.clear();
    data
}

pub fn deleted_hosts() -> HashSet<String> {
    snapshot().deleted_hosts.into_iter().collect()
}

pub fn purge_deleted_hosts(hosts: &[String]) -> Result<AdminData> {
    let state = ADMIN_STATE.get().expect("admin state not initialized");
    let purge_set: HashSet<String> = hosts
        .iter()
        .map(|host| host.trim().to_string())
        .filter(|host| !host.is_empty())
        .collect();
    if purge_set.is_empty() {
        return Ok(public_snapshot());
    }

    let current = state
        .data
        .lock()
        .ok()
        .map(|current| current.clone())
        .unwrap_or_default();
    let mut data = current;
    data.deleted_hosts.retain(|host| !purge_set.contains(host));
    data.hosts.retain(|host, _| !purge_set.contains(host));
    for group in &mut data.server_groups {
        group.servers.retain(|host| !purge_set.contains(host));
    }
    for rule in &mut data.alert_rules {
        rule.servers.retain(|host| !purge_set.contains(host));
    }
    normalize_admin_data(&mut data);
    write_data(state, data)?;
    Ok(public_snapshot())
}

pub fn ensure_default_access_key() -> Result<HostGroup> {
    let state = ADMIN_STATE.get().expect("admin state not initialized");
    let mut data = state.data.lock().unwrap().clone();
    normalize_admin_data(&mut data);
    if !data.deleted_access_keys.iter().any(|gid| gid == DEFAULT_ACCESS_KEY_ID) {
        data.access_keys
            .entry(DEFAULT_ACCESS_KEY_ID.to_string())
            .or_insert_with(|| AccessKeyOverride {
                source_gid: DEFAULT_ACCESS_KEY_ID.to_string(),
                password: uuid::Uuid::new_v4().to_string(),
                notify: Some(true),
                expire_notify: Some(true),
                ..Default::default()
            });
    } else {
        data.deleted_access_keys.retain(|gid| gid != DEFAULT_ACCESS_KEY_ID);
        data.access_keys.insert(
            DEFAULT_ACCESS_KEY_ID.to_string(),
            AccessKeyOverride {
                source_gid: DEFAULT_ACCESS_KEY_ID.to_string(),
                password: uuid::Uuid::new_v4().to_string(),
                notify: Some(true),
                expire_notify: Some(true),
                ..Default::default()
            },
        );
    }
    let data = write_data(state, data)?;
    effective_group_from_data(&data, &HashMap::new(), DEFAULT_ACCESS_KEY_ID)
        .ok_or_else(|| anyhow::anyhow!("failed to create default access key"))
}

pub fn create_install_token(gid: &str) -> Result<String> {
    let state = ADMIN_STATE.get().expect("admin state not initialized");
    let token = random_install_token();
    let now = unix_ts();
    let token_data = InstallTokenOverride {
        gid: gid.trim().to_string(),
        token_hash: install_token_hash(&token),
        expires_at: now.saturating_add(INSTALL_TOKEN_TTL_SECONDS),
    };

    let mut data = state.data.lock().unwrap().clone();
    data.install_tokens.retain(|_, item| install_token_valid_at(item, now));
    data.install_tokens.insert(token_data.token_hash.clone(), token_data);
    write_data(state, data)?;
    Ok(token)
}

pub fn resolve_install_token(base: &HashMap<String, HostGroup>, token: &str) -> Option<HostGroup> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let token_hash = install_token_hash(token);
    let now = unix_ts();
    let data = snapshot();
    let item = data
        .install_tokens
        .values()
        .find(|item| item.token_hash == token_hash && install_token_valid_at(item, now))?;
    effective_group_from_data(&data, base, &item.gid)
}

pub fn effective_admin_user(base: Option<&str>) -> Option<String> {
    let data = snapshot();
    data.admin_user
        .as_deref()
        .map(str::trim)
        .filter(|user| !user.is_empty())
        .map(str::to_string)
        .or_else(|| base.map(str::trim).filter(|user| !user.is_empty()).map(str::to_string))
}

pub fn admin_password_matches(base: Option<&str>, password: &str) -> bool {
    admin_password_matches_from_data(&snapshot(), base, password)
}

fn admin_password_matches_from_data(data: &AdminData, base: Option<&str>, password: &str) -> bool {
    if password.is_empty() {
        return false;
    }
    if let Some(hash) = data
        .admin_password_hash
        .as_deref()
        .filter(|hash| !hash.trim().is_empty())
    {
        return verify_admin_password_hash(hash, password);
    }
    base.is_some_and(|base| password.eq(base))
}

pub fn admin_session_version() -> u64 {
    snapshot().admin_session_version
}

pub fn effective_admin_path() -> String {
    let path = snapshot().admin_path;
    if validate_admin_path(&path).is_ok() {
        path
    } else {
        DEFAULT_ADMIN_PATH.to_string()
    }
}

pub fn request_matches_admin_path(path: &str) -> bool {
    let trimmed = path.trim_end_matches('/');
    let normalized = if trimmed.is_empty() { "/" } else { trimmed };
    normalized == effective_admin_path()
}

pub fn admin_password_override_configured() -> bool {
    snapshot()
        .admin_password_hash
        .as_deref()
        .is_some_and(|hash| !hash.trim().is_empty())
}

#[derive(Debug)]
pub enum PasswordUpdateError {
    InvalidUsername,
    InvalidAdminPath,
    WrongCurrentPassword,
    NewPasswordTooShort,
    NewPasswordTooLong,
    NewPasswordUnchanged,
    NothingChanged,
    HashFailed,
    SaveFailed,
}

pub fn update_admin_credentials(
    base_user: Option<&str>,
    base: Option<&str>,
    current_password: &str,
    new_username: Option<&str>,
    new_password: Option<&str>,
    new_admin_path: Option<&str>,
) -> std::result::Result<(), PasswordUpdateError> {
    let state = ADMIN_STATE.get().expect("admin state not initialized");
    let mut data = state.data.lock().unwrap().clone();
    let changed = apply_admin_credentials_update(
        &mut data,
        base_user,
        base,
        current_password,
        new_username,
        new_password,
        new_admin_path,
    )?;
    if !changed {
        return Err(PasswordUpdateError::NothingChanged);
    }
    normalize_admin_data(&mut data);
    data.admin_session_version = data.admin_session_version.saturating_add(1);
    write_data(state, data)
        .map(|_| ())
        .map_err(|_| PasswordUpdateError::SaveFailed)
}

fn apply_admin_credentials_update(
    data: &mut AdminData,
    base_user: Option<&str>,
    base: Option<&str>,
    current_password: &str,
    new_username: Option<&str>,
    new_password: Option<&str>,
    new_admin_path: Option<&str>,
) -> std::result::Result<bool, PasswordUpdateError> {
    if !admin_password_matches_from_data(&data, base, current_password) {
        return Err(PasswordUpdateError::WrongCurrentPassword);
    }

    let current_user = effective_admin_user_from_data(data, base_user).unwrap_or_else(|| "admin".to_string());
    let next_user = new_username
        .map(str::trim)
        .filter(|user| !user.is_empty())
        .unwrap_or(current_user.as_str());
    validate_admin_username(next_user)?;

    let current_admin_path = normalize_admin_path_value(&data.admin_path);
    let next_admin_path = new_admin_path
        .map(normalize_admin_path_value)
        .unwrap_or_else(|| current_admin_path.clone());
    validate_admin_path(&next_admin_path).map_err(|_| PasswordUpdateError::InvalidAdminPath)?;

    let next_password = new_password.map(str::trim).filter(|password| !password.is_empty());
    let user_changed = next_user != current_user;
    let password_changed = next_password.is_some();
    let admin_path_changed = next_admin_path != current_admin_path;
    if let Some(next_password) = next_password {
        validate_new_admin_password(current_password, next_password)?;
        let hash = hash_admin_password(next_password).map_err(|_| PasswordUpdateError::HashFailed)?;
        data.admin_password_hash = Some(hash);
    }
    if user_changed {
        data.admin_user = Some(next_user.to_string());
    }
    if admin_path_changed {
        data.admin_path = next_admin_path;
    }
    Ok(user_changed || password_changed || admin_path_changed)
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

pub fn effective_tgbot_config(base: &notifier::tgbot::Config) -> notifier::tgbot::Config {
    let mut cfg = base.clone();
    if let Some(override_data) = snapshot().tgbot {
        cfg.enabled = override_data.enabled;
        if override_data.clear_bot_token {
            cfg.bot_token.clear();
        } else {
            override_string(&mut cfg.bot_token, override_data.bot_token);
        }
        if override_data.clear_chat_id {
            cfg.chat_id.clear();
        } else {
            override_string(&mut cfg.chat_id, override_data.chat_id);
        }
        override_string(&mut cfg.title, override_data.title);
        override_string(&mut cfg.expire_tpl, override_data.expire_tpl);
        override_string(&mut cfg.health_tpl, override_data.health_tpl);
    }
    cfg
}

pub fn effective_bark_config(base: &notifier::bark::Config) -> notifier::bark::Config {
    let mut cfg = base.clone();
    if let Some(override_data) = snapshot().bark {
        cfg.enabled = override_data.enabled;
        override_string(&mut cfg.server, override_data.server);
        if override_data.clear_device_key {
            cfg.device_key.clear();
        } else {
            override_string(&mut cfg.device_key, override_data.device_key);
        }
        override_string(&mut cfg.title, override_data.title);
        override_string(&mut cfg.group, override_data.group);
        override_string(&mut cfg.icon, override_data.icon);
        override_string(&mut cfg.sound, override_data.sound);
        override_string(&mut cfg.url, override_data.url);
        override_string(&mut cfg.expire_tpl, override_data.expire_tpl);
        override_string(&mut cfg.health_tpl, override_data.health_tpl);
        if let Some(timeout) = override_data.timeout {
            cfg.timeout = timeout;
        }
    }
    cfg
}

pub fn normalize_bark_override(config: &mut BarkOverride) {
    config.server = config.server.trim().trim_end_matches('/').to_string();
    config.device_key = config.device_key.trim().to_string();
    if is_secret_mask(&config.device_key) {
        config.device_key.clear();
    }
    if config.clear_device_key {
        config.device_key.clear();
        if let Some((server, _)) = split_bark_server_and_key(&config.server) {
            config.server = server;
        }
        return;
    }
    if !config.device_key.is_empty() {
        config.clear_device_key = false;
    }
    if let Some((server, device_key)) = split_bark_server_and_key(&config.server) {
        config.server = server;
        if config.device_key.is_empty() {
            config.device_key = device_key;
            config.clear_device_key = false;
        }
    }
}

fn split_bark_server_and_key(input: &str) -> Option<(String, String)> {
    let value = input.trim().trim_end_matches('/');
    let (scheme, rest) = value
        .strip_prefix("https://")
        .map(|rest| ("https", rest))
        .or_else(|| value.strip_prefix("http://").map(|rest| ("http", rest)))?;
    let (authority, path) = rest.split_once('/')?;
    let device_key = path.split('/').find(|part| !part.trim().is_empty())?.trim();
    if device_key.eq_ignore_ascii_case("push") {
        return None;
    }
    if !authority.eq_ignore_ascii_case("api.day.app") && device_key.chars().count() < 12 {
        return None;
    }
    Some((format!("{scheme}://{authority}"), device_key.to_string()))
}

impl NodeOverride {
    fn normalize(&mut self) {
        normalize_optional_string(&mut self.alias);
        normalize_optional_string(&mut self.note);
        normalize_optional_string(&mut self.public_note);
        normalize_optional_string(&mut self.spec);
        normalize_optional_string(&mut self.expire);
        self.billing.normalize();
    }

    fn apply_to(&self, host: &mut Host) {
        if let Some(alias) = &self.alias {
            host.alias.clone_from(alias);
        }
        if let Some(note) = &self.note {
            host.labels = set_label_value(&host.labels, "note", note);
        }
        if let Some(public_note) = &self.public_note {
            host.labels = set_label_value(&host.labels, "public_note", public_note);
        }
        if let Some(spec) = &self.spec {
            host.labels = set_label_value(&host.labels, "spec", spec);
        }
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
    fn normalize(&mut self) {
        normalize_optional_string(&mut self.start_date);
        normalize_optional_string(&mut self.end_date);
        normalize_optional_string(&mut self.auto_renewal);
        normalize_optional_string(&mut self.cycle);
        normalize_optional_string(&mut self.amount);
    }

    fn apply_to(&self, billing: &mut BillingConfig) {
        override_option_string(&mut billing.start_date, &self.start_date);
        override_option_string(&mut billing.end_date, &self.end_date);
        override_option_string(&mut billing.auto_renewal, &self.auto_renewal);
        override_option_string(&mut billing.cycle, &self.cycle);
        override_option_string(&mut billing.amount, &self.amount);
    }
}

impl AccessKeyOverride {
    fn normalize(&mut self) {
        self.source_gid = self.source_gid.trim().to_string();
        self.password = self.password.trim().to_string();
        self.location = self.location.trim().to_string();
        self.r#type = self.r#type.trim().to_string();
        self.labels = self.labels.trim().to_string();
        self.expire = self.expire.trim().to_string();
        self.billing.normalize();
    }

    fn to_host_group(&self, gid: &str, base: &HashMap<String, HostGroup>) -> Option<HostGroup> {
        let source_gid = if self.source_gid.trim().is_empty() {
            gid
        } else {
            self.source_gid.trim()
        };
        let mut group = base.get(source_gid).cloned().unwrap_or_else(|| HostGroup {
            gid: gid.to_string(),
            password: String::new(),
            location: String::new(),
            r#type: String::new(),
            notify: true,
            pos: 0,
            weight: 0,
            labels: String::new(),
            expire: String::new(),
            billing: BillingConfig::default(),
            expire_notify: true,
        });
        group.gid = gid.to_string();
        override_string(&mut group.password, self.password.clone());
        override_string(&mut group.location, self.location.clone());
        override_string(&mut group.r#type, self.r#type.clone());
        override_string(&mut group.labels, self.labels.clone());
        override_string(&mut group.expire, self.expire.clone());
        self.billing.apply_to(&mut group.billing);
        if let Some(notify) = self.notify {
            group.notify = notify;
        }
        if let Some(expire_notify) = self.expire_notify {
            group.expire_notify = expire_notify;
        }
        if let Some(weight) = self.weight {
            group.weight = weight;
        }
        if group.password.is_empty() {
            return None;
        }
        Some(group)
    }
}

pub fn effective_group(base: &HashMap<String, HostGroup>, gid: &str) -> Option<HostGroup> {
    let data = snapshot();
    effective_group_from_data(&data, base, gid).or_else(|| {
        if data.deleted_access_keys.iter().any(|item| item == gid) {
            return None;
        }
        base.get(gid).cloned()
    })
}

fn effective_group_from_data(data: &AdminData, base: &HashMap<String, HostGroup>, gid: &str) -> Option<HostGroup> {
    if data.deleted_access_keys.iter().any(|item| item == gid) {
        return None;
    }
    if let Some(access_key) = data.access_keys.get(gid) {
        return access_key.to_host_group(gid, base);
    }
    None
}

pub fn access_base_url() -> Option<String> {
    let value = snapshot().access_base_url.trim().trim_end_matches('/').to_string();
    (!value.is_empty()).then_some(value)
}

pub fn agent_base_url() -> Option<String> {
    let value = snapshot().agent_base_url.trim().trim_end_matches('/').to_string();
    (!value.is_empty()).then_some(value)
}

pub fn effective_alert_rules() -> Vec<AlertRuleOverride> {
    snapshot()
        .alert_rules
        .into_iter()
        .filter(|rule| rule.enabled && !rule.metric.trim().is_empty())
        .collect()
}

pub fn notification_group_allows(group_id: &str, notifier_kind: &str) -> bool {
    let group_id = group_id.trim();
    if group_id.is_empty() {
        return true;
    }
    let method = match notifier_kind {
        "tgbot" => "tg",
        "bark" => "bark",
        other => other,
    };
    let data = snapshot();
    let Some(group) = data.notification_groups.iter().find(|group| group.id == group_id) else {
        return true;
    };
    group.notifications.is_empty() || group.notifications.iter().any(|item| item == method)
}

pub fn notification_methods_allow(methods: &[String], notifier_kind: &str) -> bool {
    if methods.is_empty() {
        return true;
    }
    let method = match notifier_kind {
        "tgbot" => "tg",
        "bark" => "bark",
        other => other,
    };
    methods.iter().any(|item| item == method)
}

fn merge_sensitive_fields(data: &mut AdminData, current: &AdminData) {
    data.admin_user.clone_from(&current.admin_user);
    data.admin_password_hash.clone_from(&current.admin_password_hash);
    data.admin_session_version = current.admin_session_version;
    data.install_tokens.clone_from(&current.install_tokens);
    if let (Some(next), Some(prev)) = (&mut data.tgbot, &current.tgbot) {
        if !next.clear_bot_token && (next.bot_token.trim().is_empty() || is_secret_mask(&next.bot_token)) {
            next.bot_token.clone_from(&prev.bot_token);
            next.clear_bot_token = prev.clear_bot_token;
        }
        if !next.clear_chat_id && (next.chat_id.trim().is_empty() || is_secret_mask(&next.chat_id)) {
            next.chat_id.clone_from(&prev.chat_id);
            next.clear_chat_id = prev.clear_chat_id;
        }
    }
    if let (Some(next), Some(prev)) = (&mut data.bark, &current.bark) {
        if !next.clear_device_key && (next.device_key.trim().is_empty() || is_secret_mask(&next.device_key)) {
            next.device_key.clone_from(&prev.device_key);
            next.clear_device_key = prev.clear_device_key;
        }
    }
    for (gid, access_key) in &mut data.access_keys {
        if access_key.password.trim().is_empty() {
            if let Some(prev) = current.access_keys.get(gid) {
                access_key.password.clone_from(&prev.password);
            } else if !access_key.source_gid.trim().is_empty() {
                if let Some(prev) = current.access_keys.get(access_key.source_gid.trim()) {
                    access_key.password.clone_from(&prev.password);
                }
            }
        }
    }
}

fn normalize_admin_data(data: &mut AdminData) {
    normalize_optional_string(&mut data.admin_user);
    data.admin_path = normalize_admin_path_value(&data.admin_path);
    if let Some(tgbot) = &mut data.tgbot {
        normalize_tgbot_override(tgbot);
    }
    if let Some(bark) = &mut data.bark {
        normalize_bark_override(bark);
    }
    for override_data in data.hosts.values_mut() {
        override_data.normalize();
    }
    for override_data in data.groups.values_mut() {
        override_data.normalize();
    }
    for access_key in data.access_keys.values_mut() {
        access_key.normalize();
    }
    data.server_groups.iter_mut().for_each(normalize_server_group);
    data.server_groups
        .retain(|group| !group.id.is_empty() && !group.name.is_empty());
    dedup_by_id(&mut data.server_groups, |group| &group.id);

    data.notification_groups
        .iter_mut()
        .for_each(normalize_notification_group);
    data.notification_groups
        .retain(|group| !group.id.is_empty() && !group.name.is_empty());
    dedup_by_id(&mut data.notification_groups, |group| &group.id);

    data.alert_rules.iter_mut().for_each(normalize_alert_rule);
    data.alert_rules
        .retain(|rule| !rule.id.is_empty() && !rule.name.is_empty() && !rule.metric.is_empty());
    dedup_by_id(&mut data.alert_rules, |rule| &rule.id);

    data.deleted_hosts = data
        .deleted_hosts
        .iter()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    data.deleted_hosts.sort();
    data.deleted_hosts.dedup();
    let deleted_hosts: HashSet<String> = data.deleted_hosts.iter().cloned().collect();
    data.hosts
        .retain(|name, _| !name.trim().is_empty() && !deleted_hosts.contains(name));
    for group in &mut data.server_groups {
        group.servers.retain(|name| !deleted_hosts.contains(name));
    }
    for rule in &mut data.alert_rules {
        rule.servers.retain(|name| !deleted_hosts.contains(name));
    }

    data.deleted_access_keys = data
        .deleted_access_keys
        .iter()
        .map(|gid| gid.trim().to_string())
        .filter(|gid| !gid.is_empty())
        .collect();
    data.deleted_access_keys.sort();
    data.deleted_access_keys.dedup();
    let deleted: HashSet<String> = data.deleted_access_keys.iter().cloned().collect();
    data.access_keys
        .retain(|gid, _| !gid.trim().is_empty() && !deleted.contains(gid));
    data.groups
        .retain(|gid, _| !gid.trim().is_empty() && !deleted.contains(gid));

    let now = unix_ts();
    data.install_tokens.retain(|token, item| {
        !token.trim().is_empty() && !item.gid.trim().is_empty() && install_token_valid_at(item, now)
    });
}

pub(crate) fn normalize_tgbot_override(config: &mut TgbotOverride) {
    config.bot_token = config.bot_token.trim().to_string();
    config.chat_id = config.chat_id.trim().to_string();
    if is_secret_mask(&config.bot_token) {
        config.bot_token.clear();
    }
    if is_secret_mask(&config.chat_id) {
        config.chat_id.clear();
    }
    if !config.bot_token.is_empty() {
        config.clear_bot_token = false;
    }
    if !config.chat_id.is_empty() {
        config.clear_chat_id = false;
    }
    if config.clear_bot_token {
        config.bot_token.clear();
    }
    if config.clear_chat_id {
        config.chat_id.clear();
    }
}

fn is_secret_mask(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value.chars().all(|ch| matches!(ch, '*' | '•' | '●' | '·'))
}

fn is_configured_secret(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && !value.starts_with('<') && !value.ends_with('>')
}

fn normalize_server_group(group: &mut ServerGroupOverride) {
    group.id = group.id.trim().to_string();
    group.name = group.name.trim().to_string();
    group.servers = normalized_string_vec(&group.servers);
}

fn normalize_notification_group(group: &mut NotificationGroupOverride) {
    group.id = group.id.trim().to_string();
    group.name = group.name.trim().to_string();
    group.notifications = normalized_string_vec(&group.notifications);
}

fn normalize_alert_rule(rule: &mut AlertRuleOverride) {
    rule.id = rule.id.trim().to_string();
    rule.name = rule.name.trim().to_string();
    rule.metric = rule.metric.trim().to_string();
    rule.notification_group = rule.notification_group.trim().to_string();
    rule.notifications = normalized_string_vec(&rule.notifications);
    rule.server_groups = normalized_string_vec(&rule.server_groups);
    rule.servers = normalized_string_vec(&rule.servers);
    rule.duration = rule.duration.max(30);
    rule.repeat_interval = rule.repeat_interval.max(60);
    if rule.metric == "offline" {
        rule.threshold = None;
    } else if let Some(threshold) = rule.threshold {
        let threshold = if threshold.is_finite() { threshold } else { 0.0 };
        rule.threshold = Some(if matches!(rule.metric.as_str(), "cpu" | "memory" | "disk") {
            threshold.clamp(0.0, 100.0)
        } else {
            threshold.max(0.0)
        });
    }
}

fn normalized_string_vec(values: &[String]) -> Vec<String> {
    let mut values = values
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

fn dedup_by_id<T, F>(values: &mut Vec<T>, id: F)
where
    F: Fn(&T) -> &str,
{
    let mut seen = HashSet::new();
    values.retain(|item| seen.insert(id(item).to_string()));
}

fn normalize_optional_string(value: &mut Option<String>) {
    if let Some(trimmed) = value.as_deref().map(str::trim).map(str::to_string) {
        if trimmed.is_empty() {
            *value = None;
        } else {
            *value = Some(trimmed);
        }
    }
}

fn normalize_admin_path_value(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches('/').trim();
    if trimmed.is_empty() {
        return DEFAULT_ADMIN_PATH.to_string();
    }
    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

pub fn validate_admin_path(path: &str) -> std::result::Result<(), &'static str> {
    let path = path.trim();
    if path.is_empty() || path == "/" {
        return Err("后台入口路径不能为空");
    }
    if path.len() > MAX_ADMIN_PATH_LEN {
        return Err("后台入口路径不能超过 64 个字符");
    }
    let Some(segment) = path.strip_prefix('/') else {
        return Err("后台入口路径必须以 / 开头");
    };
    if segment.is_empty() || segment.contains('/') {
        return Err("后台入口路径只能是一段路径");
    }
    if !segment
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("后台入口路径只能包含字母、数字、横线和下划线");
    }
    if matches!(
        segment,
        "api" | "static" | "report" | "json" | "detail" | "map" | "i" | "admin.html" | "index.html"
    ) {
        return Err("后台入口路径与系统路径冲突");
    }
    Ok(())
}

fn set_label_value(labels: &str, key: &str, value: &str) -> String {
    let mut parts: Vec<(String, String)> = labels
        .split(';')
        .filter_map(|part| {
            let (k, v) = part.split_once('=')?;
            let k = k.trim();
            if k.is_empty() {
                None
            } else {
                Some((k.to_string(), v.trim().to_string()))
            }
        })
        .collect();
    let mut found = false;
    for (k, v) in &mut parts {
        if k == key {
            *v = value.trim().to_string();
            found = true;
        }
    }
    if !found && !value.trim().is_empty() {
        parts.push((key.to_string(), value.trim().to_string()));
    }
    parts
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(";")
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

fn effective_admin_user_from_data(data: &AdminData, base: Option<&str>) -> Option<String> {
    data.admin_user
        .as_deref()
        .map(str::trim)
        .filter(|user| !user.is_empty())
        .map(str::to_string)
        .or_else(|| base.map(str::trim).filter(|user| !user.is_empty()).map(str::to_string))
}

fn validate_admin_username(username: &str) -> std::result::Result<(), PasswordUpdateError> {
    let username = username.trim();
    if username.is_empty() || username.len() > MAX_ADMIN_USERNAME_LEN {
        return Err(PasswordUpdateError::InvalidUsername);
    }
    if !username
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'@'))
    {
        return Err(PasswordUpdateError::InvalidUsername);
    }
    Ok(())
}

fn validate_new_admin_password(
    current_password: &str,
    new_password: &str,
) -> std::result::Result<(), PasswordUpdateError> {
    if new_password.chars().count() < MIN_ADMIN_PASSWORD_LEN {
        return Err(PasswordUpdateError::NewPasswordTooShort);
    }
    if new_password.len() > MAX_ADMIN_PASSWORD_LEN {
        return Err(PasswordUpdateError::NewPasswordTooLong);
    }
    if new_password == current_password {
        return Err(PasswordUpdateError::NewPasswordUnchanged);
    }
    Ok(())
}

fn hash_admin_password(password: &str) -> Result<String> {
    let rng = rand::SystemRandom::new();
    let mut salt = [0_u8; ADMIN_PASSWORD_SALT_BYTES];
    rng.fill(&mut salt)
        .map_err(|_| anyhow::anyhow!("failed to generate password salt"))?;
    let mut hash = [0_u8; ADMIN_PASSWORD_HASH_BYTES];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(ADMIN_PASSWORD_HASH_ITERATIONS).unwrap(),
        &salt,
        password.as_bytes(),
        &mut hash,
    );
    Ok(format!(
        "{ADMIN_PASSWORD_HASH_ALGO}${ADMIN_PASSWORD_HASH_ITERATIONS}${}${}",
        hex_encode(&salt),
        hex_encode(&hash)
    ))
}

fn verify_admin_password_hash(encoded: &str, password: &str) -> bool {
    let parts = encoded.split('$').collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != ADMIN_PASSWORD_HASH_ALGO {
        return false;
    }
    let Ok(iterations) = parts[1].parse::<u32>() else {
        return false;
    };
    let Some(iterations) = NonZeroU32::new(iterations) else {
        return false;
    };
    let Some(salt) = hex_decode(parts[2]) else {
        return false;
    };
    let Some(hash) = hex_decode(parts[3]) else {
        return false;
    };
    if hash.len() != ADMIN_PASSWORD_HASH_BYTES {
        return false;
    }
    pbkdf2::verify(
        pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        &salt,
        password.as_bytes(),
        &hash,
    )
    .is_ok()
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn hex_decode(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        decoded.push((high << 4) | low);
    }
    Some(decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn random_install_token() -> String {
    format!("it_{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

fn install_token_hash(token: &str) -> String {
    hex_encode(digest::digest(&digest::SHA256, token.trim().as_bytes()).as_ref())
}

fn install_token_valid_at(token: &InstallTokenOverride, now: u64) -> bool {
    !token.token_hash.trim().is_empty() && token.expires_at >= now
}

fn unix_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

fn is_false_bool(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_password_hash_round_trips() {
        let hash = hash_admin_password("new-secure-password").unwrap();
        assert!(verify_admin_password_hash(&hash, "new-secure-password"));
        assert!(!verify_admin_password_hash(&hash, "wrong-password"));
    }

    #[test]
    fn validates_admin_username() {
        assert!(validate_admin_username("admin_01@example").is_ok());
        assert!(validate_admin_username("").is_err());
        assert!(validate_admin_username("bad:name").is_err());
        assert!(validate_admin_username("bad name").is_err());
        assert!(validate_admin_username("a".repeat(MAX_ADMIN_USERNAME_LEN + 1).as_str()).is_err());
    }

    #[test]
    fn normalizes_admin_path_to_default_or_single_segment() {
        let mut default_data = AdminData::default();
        normalize_admin_data(&mut default_data);
        assert_eq!(default_data.admin_path, "/admin");

        let mut custom_data = AdminData {
            admin_path: " panel_2026 ".to_string(),
            ..Default::default()
        };
        normalize_admin_data(&mut custom_data);
        assert_eq!(custom_data.admin_path, "/panel_2026");
    }

    #[test]
    fn validates_admin_path_reserved_and_unsafe_values() {
        assert!(validate_admin_path("/panel_2026").is_ok());
        assert!(validate_admin_path("/admin-88").is_ok());
        assert!(validate_admin_path("").is_err());
        assert!(validate_admin_path("/api").is_err());
        assert!(validate_admin_path("/static").is_err());
        assert!(validate_admin_path("/report").is_err());
        assert!(validate_admin_path("/nested/path").is_err());
        assert!(validate_admin_path("../admin").is_err());
        assert!(validate_admin_path("/bad path").is_err());
    }

    #[test]
    fn account_update_can_change_admin_path_without_password_change() {
        let mut data = AdminData {
            admin_path: DEFAULT_ADMIN_PATH.to_string(),
            ..Default::default()
        };

        let changed = apply_admin_credentials_update(
            &mut data,
            Some("admin"),
            Some("current-password"),
            "current-password",
            Some("admin"),
            None,
            Some(" panel_preview "),
        )
        .unwrap();

        assert!(changed);
        assert_eq!(data.admin_path, "/panel_preview");
        assert!(data.admin_user.is_none());
        assert!(data.admin_password_hash.is_none());
    }

    #[test]
    fn account_update_rejects_invalid_admin_path() {
        let mut data = AdminData::default();

        let err = apply_admin_credentials_update(
            &mut data,
            Some("admin"),
            Some("current-password"),
            "current-password",
            Some("admin"),
            None,
            Some("/api"),
        )
        .unwrap_err();

        assert!(matches!(err, PasswordUpdateError::InvalidAdminPath));
    }

    #[test]
    fn deleted_hosts_are_removed_from_overrides_and_references() {
        let mut data = AdminData {
            hosts: HashMap::from([
                ("gone".to_string(), NodeOverride::default()),
                ("kept".to_string(), NodeOverride::default()),
            ]),
            deleted_hosts: vec!["gone".to_string(), "deleted".to_string()],
            server_groups: vec![ServerGroupOverride {
                id: "grp".to_string(),
                name: "Group".to_string(),
                servers: vec!["gone".to_string(), "kept".to_string()],
            }],
            alert_rules: vec![AlertRuleOverride {
                id: "rule".to_string(),
                name: "Rule".to_string(),
                metric: "offline".to_string(),
                servers: vec!["gone".to_string(), "kept".to_string()],
                ..Default::default()
            }],
            ..Default::default()
        };

        normalize_admin_data(&mut data);

        assert_eq!(data.deleted_hosts, vec!["deleted", "gone"]);
        assert!(!data.hosts.contains_key("gone"));
        assert!(data.hosts.contains_key("kept"));
        assert_eq!(data.server_groups[0].servers, vec!["kept"]);
        assert_eq!(data.alert_rules[0].servers, vec!["kept"]);
    }

    #[test]
    fn masked_notification_secrets_keep_existing_values() {
        let current = AdminData {
            tgbot: Some(TgbotOverride {
                bot_token: "old-token".to_string(),
                chat_id: "old-chat".to_string(),
                ..Default::default()
            }),
            bark: Some(BarkOverride {
                device_key: "old-device-key".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut next = AdminData {
            tgbot: Some(TgbotOverride {
                bot_token: "••••••••••••".to_string(),
                chat_id: "************".to_string(),
                ..Default::default()
            }),
            bark: Some(BarkOverride {
                device_key: "••••••••••••".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };

        merge_sensitive_fields(&mut next, &current);
        normalize_admin_data(&mut next);

        let tgbot = next.tgbot.unwrap();
        let bark = next.bark.unwrap();
        assert_eq!(tgbot.bot_token, "old-token");
        assert_eq!(tgbot.chat_id, "old-chat");
        assert_eq!(bark.device_key, "old-device-key");
    }

    #[test]
    fn notification_secrets_can_be_explicitly_cleared() {
        let current = AdminData {
            tgbot: Some(TgbotOverride {
                bot_token: "old-token".to_string(),
                chat_id: "old-chat".to_string(),
                ..Default::default()
            }),
            bark: Some(BarkOverride {
                device_key: "old-device-key".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut next = AdminData {
            tgbot: Some(TgbotOverride {
                clear_bot_token: true,
                clear_chat_id: true,
                ..Default::default()
            }),
            bark: Some(BarkOverride {
                clear_device_key: true,
                ..Default::default()
            }),
            ..Default::default()
        };

        merge_sensitive_fields(&mut next, &current);
        normalize_admin_data(&mut next);

        let tgbot = next.tgbot.unwrap();
        let bark = next.bark.unwrap();
        assert!(tgbot.bot_token.is_empty());
        assert!(tgbot.chat_id.is_empty());
        assert!(bark.device_key.is_empty());
        assert!(tgbot.clear_bot_token);
        assert!(tgbot.clear_chat_id);
        assert!(bark.clear_device_key);
    }

    #[test]
    fn bark_full_api_url_is_split_into_server_and_device_key() {
        let mut config = BarkOverride {
            server: "https://api.day.app/example-device-key".to_string(),
            ..Default::default()
        };

        normalize_bark_override(&mut config);

        assert_eq!(config.server, "https://api.day.app");
        assert_eq!(config.device_key, "example-device-key");
    }

    #[test]
    fn bark_push_endpoint_is_kept_as_server_url() {
        let mut config = BarkOverride {
            server: "https://api.day.app/push".to_string(),
            ..Default::default()
        };

        normalize_bark_override(&mut config);

        assert_eq!(config.server, "https://api.day.app/push");
        assert!(config.device_key.is_empty());
    }

    #[test]
    fn bark_clear_device_key_does_not_restore_key_from_server_url() {
        let mut config = BarkOverride {
            server: "https://api.day.app/example-device-key".to_string(),
            clear_device_key: true,
            ..Default::default()
        };

        normalize_bark_override(&mut config);

        assert_eq!(config.server, "https://api.day.app");
        assert!(config.device_key.is_empty());
        assert!(config.clear_device_key);
    }

    #[test]
    fn install_token_hash_does_not_store_raw_token() {
        let token = "it_example-token";
        let hash = install_token_hash(token);

        assert_ne!(hash, token);
        assert_eq!(hash.len(), 64);
        assert_eq!(hash, install_token_hash(token));
    }

    #[test]
    fn install_token_expiry_is_enforced() {
        let valid = InstallTokenOverride {
            token_hash: install_token_hash("it_valid"),
            expires_at: 100,
            ..Default::default()
        };
        let expired = InstallTokenOverride {
            token_hash: install_token_hash("it_expired"),
            expires_at: 99,
            ..Default::default()
        };

        assert!(install_token_valid_at(&valid, 100));
        assert!(!install_token_valid_at(&expired, 100));
    }

    #[test]
    fn expired_install_tokens_are_removed_from_settings() {
        let mut data = AdminData {
            install_tokens: HashMap::from([
                (
                    "expired".to_string(),
                    InstallTokenOverride {
                        gid: "default".to_string(),
                        token_hash: install_token_hash("it_expired"),
                        expires_at: 1,
                    },
                ),
                (
                    "valid".to_string(),
                    InstallTokenOverride {
                        gid: "default".to_string(),
                        token_hash: install_token_hash("it_valid"),
                        expires_at: u64::MAX,
                    },
                ),
            ]),
            ..Default::default()
        };

        normalize_admin_data(&mut data);

        assert_eq!(data.install_tokens.len(), 1);
        assert!(data.install_tokens.contains_key("valid"));
    }

    #[test]
    fn offline_alert_rules_do_not_keep_thresholds() {
        let mut rule = AlertRuleOverride {
            id: "rule".to_string(),
            name: "Offline".to_string(),
            metric: "offline".to_string(),
            threshold: Some(90.0),
            duration: 1,
            repeat_interval: 1,
            ..Default::default()
        };

        normalize_alert_rule(&mut rule);

        assert_eq!(rule.threshold, None);
        assert_eq!(rule.duration, 30);
        assert_eq!(rule.repeat_interval, 60);
    }

    #[test]
    fn percentage_alert_thresholds_are_bounded() {
        let mut rule = AlertRuleOverride {
            id: "rule".to_string(),
            name: "CPU".to_string(),
            metric: "cpu".to_string(),
            threshold: Some(180.0),
            ..Default::default()
        };

        normalize_alert_rule(&mut rule);

        assert_eq!(rule.threshold, Some(100.0));
    }
}
