use anyhow::Result;
use lettre::Address;
use minijinja::Environment;
use once_cell::sync::OnceCell;
use reqwest::header::{HeaderName, HeaderValue};
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

use crate::config::{Config, Host, HostGroup};
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
pub const ADMIN_NOTIFICATION_LOG_DIR: &str = "notifications";
const MAX_ADMIN_PATH_LEN: usize = 64;
pub const INSTALL_TOKEN_TTL_SECONDS: u64 = 15 * 60;

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
    pub uid: String,
    #[serde(default)]
    pub token_hash: String,
    #[serde(default)]
    pub expires_at: u64,
}

#[derive(Debug, Clone)]
pub struct InstallTokenIssue {
    pub token: String,
    pub expires_at: u64,
    pub expires_in: u64,
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
pub struct WechatOverride {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub corp_id: String,
    #[serde(default)]
    pub corp_secret: String,
    #[serde(default, skip_serializing_if = "is_false_bool")]
    pub clear_corp_secret: bool,
    #[serde(default, skip_deserializing, skip_serializing_if = "is_false_bool")]
    pub corp_secret_configured: bool,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub online_tpl: String,
    #[serde(default)]
    pub offline_tpl: String,
    #[serde(default)]
    pub expire_tpl: String,
    #[serde(default)]
    pub health_tpl: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmailOverride {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default, skip_serializing_if = "is_false_bool")]
    pub clear_password: bool,
    #[serde(default, skip_deserializing, skip_serializing_if = "is_false_bool")]
    pub password_configured: bool,
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub online_tpl: String,
    #[serde(default)]
    pub offline_tpl: String,
    #[serde(default)]
    pub expire_tpl: String,
    #[serde(default)]
    pub health_tpl: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebhookHeaderOverride {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default, skip_serializing_if = "is_false_bool")]
    pub clear_value: bool,
    #[serde(default, skip_deserializing, skip_serializing_if = "is_false_bool")]
    pub value_configured: bool,
}

fn default_webhook_timeout() -> u32 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredWebhookReceiver {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default, skip_serializing_if = "is_false_bool")]
    pub clear_url: bool,
    #[serde(default, skip_deserializing, skip_serializing_if = "is_false_bool")]
    pub url_configured: bool,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default, skip_serializing_if = "is_false_bool")]
    pub clear_password: bool,
    #[serde(default, skip_deserializing, skip_serializing_if = "is_false_bool")]
    pub password_configured: bool,
    #[serde(default = "default_webhook_timeout")]
    pub timeout: u32,
    #[serde(default)]
    pub headers: Vec<WebhookHeaderOverride>,
    #[serde(default)]
    pub body_tpl: String,
}

impl Default for StructuredWebhookReceiver {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            enabled: false,
            url: String::new(),
            clear_url: false,
            url_configured: false,
            username: String::new(),
            password: String::new(),
            clear_password: false,
            password_configured: false,
            timeout: default_webhook_timeout(),
            headers: Vec::new(),
            body_tpl: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StructuredWebhookOverride {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub receivers: Vec<StructuredWebhookReceiver>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogOverride {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub tpl: String,
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
    #[serde(default)]
    pub wechat: Option<WechatOverride>,
    #[serde(default)]
    pub email: Option<EmailOverride>,
    #[serde(default)]
    pub webhook: Option<StructuredWebhookOverride>,
    #[serde(default)]
    pub log: Option<LogOverride>,
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
    replace_state_data(state, data)
}

fn replace_state_data(state: &AdminState, data: AdminData) -> Result<AdminData> {
    let current = state
        .data
        .lock()
        .ok()
        .map(|current| current.clone())
        .unwrap_or_default();
    let data = prepare_replacement(data, &current)?;
    write_data(state, data)
}

fn prepare_replacement(mut data: AdminData, current: &AdminData) -> Result<AdminData> {
    normalize_sensitive_field_identities(&mut data);
    validate_sensitive_field_identities(&data)?;
    merge_sensitive_fields(&mut data, current);
    normalize_admin_data(&mut data);
    validate_admin_path(&data.admin_path).map_err(|err| anyhow::anyhow!("{err}"))?;
    validate_admin_data(&data)?;
    data.access_base_url = data.access_base_url.trim().trim_end_matches('/').to_string();
    data.agent_base_url = data.agent_base_url.trim().trim_end_matches('/').to_string();
    Ok(data)
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
    public_admin_data(&snapshot())
}

fn public_admin_data(data: &AdminData) -> AdminData {
    let mut data = data.clone();
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
    if let Some(wechat) = &mut data.wechat {
        wechat.corp_secret_configured = !wechat.clear_corp_secret && is_configured_secret(&wechat.corp_secret);
        wechat.corp_secret.clear();
    }
    if let Some(email) = &mut data.email {
        email.password_configured = !email.clear_password && is_configured_secret(&email.password);
        email.password.clear();
    }
    if let Some(webhook) = &mut data.webhook {
        for receiver in &mut webhook.receivers {
            receiver.url_configured = !receiver.clear_url && is_configured_secret(&receiver.url);
            receiver.url.clear();
            receiver.password_configured = !receiver.clear_password && is_configured_secret(&receiver.password);
            receiver.password.clear();
            for header in &mut receiver.headers {
                header.value_configured = !header.clear_value && is_configured_secret(&header.value);
                header.value.clear();
            }
        }
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

pub fn clear_deleted_host_marker(name: &str) -> Result<bool> {
    let state = ADMIN_STATE.get().expect("admin state not initialized");
    let current = state
        .data
        .lock()
        .ok()
        .map(|current| current.clone())
        .unwrap_or_default();
    let mut data = current;
    if !remove_deleted_host_marker_from_data(&mut data, name) {
        return Ok(false);
    }
    write_data(state, data)?;
    Ok(true)
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

pub fn create_install_token(gid: &str, uid: &str) -> Result<InstallTokenIssue> {
    let state = ADMIN_STATE.get().expect("admin state not initialized");
    let token = random_install_token();
    let now = unix_ts();
    let expires_at = now.saturating_add(INSTALL_TOKEN_TTL_SECONDS);
    let token_data = InstallTokenOverride {
        gid: gid.trim().to_string(),
        uid: uid.trim().to_string(),
        token_hash: install_token_hash(&token),
        expires_at,
    };

    let mut data = state.data.lock().unwrap().clone();
    data.install_tokens.retain(|_, item| install_token_valid_at(item, now));
    data.install_tokens.insert(token_data.token_hash.clone(), token_data);
    write_data(state, data)?;
    Ok(InstallTokenIssue {
        token,
        expires_at,
        expires_in: INSTALL_TOKEN_TTL_SECONDS,
    })
}

pub fn consume_install_token(base: &HashMap<String, HostGroup>, token: &str, uid: &str) -> Result<Option<HostGroup>> {
    let state = ADMIN_STATE.get().expect("admin state not initialized");
    let mut data = state
        .data
        .lock()
        .ok()
        .map(|current| current.clone())
        .unwrap_or_default();
    let now = unix_ts();
    let group = consume_install_token_from_data(&mut data, base, token, uid, now);
    if group.is_some() {
        write_data(state, data)?;
    }
    Ok(group)
}

fn consume_install_token_from_data(
    data: &mut AdminData,
    base: &HashMap<String, HostGroup>,
    token: &str,
    uid: &str,
    now: u64,
) -> Option<HostGroup> {
    let token = token.trim();
    let uid = uid.trim();
    if token.is_empty() || uid.is_empty() {
        return None;
    }
    let token_hash = install_token_hash(token);
    data.install_tokens.retain(|_, item| install_token_valid_at(item, now));
    let token_key = data
        .install_tokens
        .iter()
        .find(|(_, item)| item.token_hash == token_hash && item.uid == uid && install_token_valid_at(item, now))
        .map(|(key, _)| key.clone())?;
    let item = data.install_tokens.get(&token_key)?;
    let group = effective_group_from_data(data, base, &item.gid)?;
    data.install_tokens.remove(&token_key);
    Some(group)
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
    effective_tgbot_config_from_data(&snapshot(), base)
}

pub fn effective_bark_config(base: &notifier::bark::Config) -> notifier::bark::Config {
    effective_bark_config_from_data(&snapshot(), base)
}

pub fn effective_wechat_config(base: &notifier::wechat::Config) -> notifier::wechat::Config {
    effective_wechat_config_from_data(&snapshot(), base)
}

fn effective_wechat_config_from_data(data: &AdminData, base: &notifier::wechat::Config) -> notifier::wechat::Config {
    let mut cfg = clone_wechat_config(base);
    if let Some(override_data) = &data.wechat {
        cfg.enabled = override_data.enabled;
        override_string(&mut cfg.corp_id, override_data.corp_id.clone());
        if override_data.clear_corp_secret {
            cfg.corp_secret.clear();
        } else {
            override_string(&mut cfg.corp_secret, override_data.corp_secret.clone());
        }
        override_string(&mut cfg.agent_id, override_data.agent_id.clone());
        override_string(&mut cfg.title, override_data.title.clone());
        override_string(&mut cfg.online_tpl, override_data.online_tpl.clone());
        override_string(&mut cfg.offline_tpl, override_data.offline_tpl.clone());
        override_string(&mut cfg.expire_tpl, override_data.expire_tpl.clone());
        override_string(&mut cfg.health_tpl, override_data.health_tpl.clone());
    }
    cfg
}

fn clone_wechat_config(base: &notifier::wechat::Config) -> notifier::wechat::Config {
    notifier::wechat::Config {
        enabled: base.enabled,
        corp_id: base.corp_id.clone(),
        corp_secret: base.corp_secret.clone(),
        agent_id: base.agent_id.clone(),
        title: base.title.clone(),
        online_tpl: base.online_tpl.clone(),
        offline_tpl: base.offline_tpl.clone(),
        custom_tpl: base.custom_tpl.clone(),
        expire_tpl: base.expire_tpl.clone(),
        health_tpl: base.health_tpl.clone(),
    }
}

pub fn effective_email_config(base: &notifier::email::Config) -> notifier::email::Config {
    effective_email_config_from_data(&snapshot(), base)
}

fn effective_email_config_from_data(data: &AdminData, base: &notifier::email::Config) -> notifier::email::Config {
    let mut cfg = clone_email_config(base);
    if let Some(override_data) = &data.email {
        cfg.enabled = override_data.enabled;
        override_string(&mut cfg.server, override_data.server.clone());
        override_string(&mut cfg.username, override_data.username.clone());
        if override_data.clear_password {
            cfg.password.clear();
        } else {
            override_string(&mut cfg.password, override_data.password.clone());
        }
        override_string(&mut cfg.to, override_data.to.clone());
        override_string(&mut cfg.subject, override_data.subject.clone());
        override_string(&mut cfg.title, override_data.title.clone());
        override_string(&mut cfg.online_tpl, override_data.online_tpl.clone());
        override_string(&mut cfg.offline_tpl, override_data.offline_tpl.clone());
        override_string(&mut cfg.expire_tpl, override_data.expire_tpl.clone());
        override_string(&mut cfg.health_tpl, override_data.health_tpl.clone());
    }
    cfg
}

fn clone_email_config(base: &notifier::email::Config) -> notifier::email::Config {
    notifier::email::Config {
        enabled: base.enabled,
        server: base.server.clone(),
        username: base.username.clone(),
        password: base.password.clone(),
        to: base.to.clone(),
        subject: base.subject.clone(),
        title: base.title.clone(),
        online_tpl: base.online_tpl.clone(),
        offline_tpl: base.offline_tpl.clone(),
        custom_tpl: base.custom_tpl.clone(),
        expire_tpl: base.expire_tpl.clone(),
        health_tpl: base.health_tpl.clone(),
    }
}

#[derive(Debug, Clone)]
pub enum EffectiveWebhookConfig {
    Legacy(notifier::webhook::Config),
    Structured(StructuredWebhookOverride),
}

impl EffectiveWebhookConfig {
    pub fn is_ready(&self) -> bool {
        match self {
            Self::Legacy(config) => config.is_ready(),
            Self::Structured(config) => config.is_ready(),
        }
    }
}

pub fn effective_webhook_override(base: &notifier::webhook::Config) -> EffectiveWebhookConfig {
    effective_webhook_override_from_data(&snapshot(), base)
}

fn effective_webhook_override_from_data(data: &AdminData, base: &notifier::webhook::Config) -> EffectiveWebhookConfig {
    data.webhook.as_ref().map_or_else(
        || EffectiveWebhookConfig::Legacy(base.clone()),
        |config| EffectiveWebhookConfig::Structured(config.clone()),
    )
}

pub fn effective_log_config(base: &notifier::log::Config) -> notifier::log::Config {
    effective_log_config_from_data(&snapshot(), base)
}

fn effective_log_config_from_data(data: &AdminData, base: &notifier::log::Config) -> notifier::log::Config {
    let mut cfg = notifier::log::Config {
        enabled: base.enabled,
        log_dir: base.log_dir.clone(),
        tpl: base.tpl.clone(),
    };
    if let Some(override_data) = &data.log {
        cfg.enabled = override_data.enabled;
        cfg.log_dir = ADMIN_NOTIFICATION_LOG_DIR.to_string();
        override_string(&mut cfg.tpl, override_data.tpl.clone());
    }
    cfg
}

impl notifier::tgbot::Config {
    pub fn is_ready(&self) -> bool {
        self.enabled && is_configured_secret(&self.bot_token) && is_configured_secret(&self.chat_id)
    }
}

impl notifier::bark::Config {
    pub fn is_ready(&self) -> bool {
        self.enabled && (is_configured_secret(&self.device_key) || split_bark_server_and_key(&self.server).is_some())
    }
}

impl notifier::wechat::Config {
    pub fn is_ready(&self) -> bool {
        self.enabled
            && is_configured_secret(&self.corp_id)
            && is_configured_secret(&self.corp_secret)
            && is_configured_secret(&self.agent_id)
    }
}

impl notifier::email::Config {
    pub fn is_ready(&self) -> bool {
        self.enabled
            && valid_smtp_relay(&self.server)
            && valid_email_address(&self.username)
            && is_configured_secret(&self.password)
            && valid_email_recipients(&self.to)
    }
}

impl notifier::webhook::Config {
    pub fn is_ready(&self) -> bool {
        self.enabled
            && self.receiver.iter().any(|receiver| {
                receiver.enabled
                    && !receiver.url.trim().is_empty()
                    && (1..=60).contains(&receiver.timeout)
                    && !receiver.script.trim().is_empty()
            })
    }
}

impl StructuredWebhookOverride {
    pub fn is_ready(&self) -> bool {
        self.enabled && self.receivers.iter().any(StructuredWebhookReceiver::is_ready)
    }
}

impl StructuredWebhookReceiver {
    pub fn is_ready(&self) -> bool {
        self.enabled
            && is_configured_secret(&self.url)
            && (1..=60).contains(&self.timeout)
            && !self.body_tpl.trim().is_empty()
    }
}

impl notifier::log::Config {
    pub fn is_ready(&self) -> bool {
        self.enabled && !self.log_dir.trim().is_empty() && !self.tpl.trim().is_empty()
    }
}

pub fn configured_notification_methods(cfg: &Config) -> Vec<String> {
    configured_notification_methods_from_data(&snapshot(), cfg)
}

fn configured_notification_methods_from_data(data: &AdminData, cfg: &Config) -> Vec<String> {
    let mut methods = Vec::new();
    if effective_tgbot_config_from_data(data, &cfg.tgbot).is_ready() {
        methods.push("tg".to_string());
    }
    if effective_bark_config_from_data(data, &cfg.bark).is_ready() {
        methods.push("bark".to_string());
    }
    if effective_wechat_config_from_data(data, &cfg.wechat).is_ready() {
        methods.push("wechat".to_string());
    }
    if effective_email_config_from_data(data, &cfg.email).is_ready() {
        methods.push("email".to_string());
    }
    if effective_webhook_override_from_data(data, &cfg.webhook).is_ready() {
        methods.push("webhook".to_string());
    }
    if effective_log_config_from_data(data, &cfg.log).is_ready() {
        methods.push("log".to_string());
    }
    methods
}

fn effective_tgbot_config_from_data(data: &AdminData, base: &notifier::tgbot::Config) -> notifier::tgbot::Config {
    let mut cfg = base.clone();
    if let Some(override_data) = &data.tgbot {
        cfg.enabled = override_data.enabled;
        if override_data.clear_bot_token {
            cfg.bot_token.clear();
        } else {
            override_string(&mut cfg.bot_token, override_data.bot_token.clone());
        }
        if override_data.clear_chat_id {
            cfg.chat_id.clear();
        } else {
            override_string(&mut cfg.chat_id, override_data.chat_id.clone());
        }
        override_string(&mut cfg.title, override_data.title.clone());
        override_string(&mut cfg.expire_tpl, override_data.expire_tpl.clone());
        override_string(&mut cfg.health_tpl, override_data.health_tpl.clone());
    }
    cfg
}

fn effective_bark_config_from_data(data: &AdminData, base: &notifier::bark::Config) -> notifier::bark::Config {
    let mut cfg = base.clone();
    if let Some(override_data) = &data.bark {
        cfg.enabled = override_data.enabled;
        override_string(&mut cfg.server, override_data.server.clone());
        if override_data.clear_device_key {
            cfg.device_key.clear();
        } else {
            override_string(&mut cfg.device_key, override_data.device_key.clone());
        }
        override_string(&mut cfg.title, override_data.title.clone());
        override_string(&mut cfg.group, override_data.group.clone());
        override_string(&mut cfg.icon, override_data.icon.clone());
        override_string(&mut cfg.sound, override_data.sound.clone());
        override_string(&mut cfg.url, override_data.url.clone());
        override_string(&mut cfg.expire_tpl, override_data.expire_tpl.clone());
        override_string(&mut cfg.health_tpl, override_data.health_tpl.clone());
        if let Some(timeout) = override_data.timeout {
            cfg.timeout = timeout;
        }
    }
    cfg
}

pub fn normalize_bark_override(config: &mut BarkOverride) {
    config.server = config.server.trim().trim_end_matches('/').to_string();
    normalize_secret(&mut config.device_key, &mut config.clear_device_key);
    if config.clear_device_key {
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
    if let (Some(next), Some(prev)) = (&mut data.wechat, &current.wechat) {
        if !next.clear_corp_secret && (next.corp_secret.trim().is_empty() || is_secret_mask(&next.corp_secret)) {
            next.corp_secret.clone_from(&prev.corp_secret);
            next.clear_corp_secret = prev.clear_corp_secret;
        }
    }
    if let (Some(next), Some(prev)) = (&mut data.email, &current.email) {
        if !next.clear_password && (next.password.trim().is_empty() || is_secret_mask(&next.password)) {
            next.password.clone_from(&prev.password);
            next.clear_password = prev.clear_password;
        }
    }
    if let (Some(next), Some(prev)) = (&mut data.webhook, &current.webhook) {
        merge_webhook_secrets(next, prev);
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
    if let Some(wechat) = &mut data.wechat {
        normalize_wechat_override(wechat);
    }
    if let Some(email) = &mut data.email {
        normalize_email_override(email);
    }
    if let Some(webhook) = &mut data.webhook {
        normalize_webhook_override(webhook);
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
        !token.trim().is_empty()
            && !item.gid.trim().is_empty()
            && !item.uid.trim().is_empty()
            && install_token_valid_at(item, now)
    });
}

fn remove_deleted_host_marker_from_data(data: &mut AdminData, name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    let before = data.deleted_hosts.len();
    data.deleted_hosts.retain(|host| host != name);
    before != data.deleted_hosts.len()
}

pub(crate) fn normalize_tgbot_override(config: &mut TgbotOverride) {
    normalize_secret(&mut config.bot_token, &mut config.clear_bot_token);
    normalize_secret(&mut config.chat_id, &mut config.clear_chat_id);
}

fn merge_webhook_secrets(next: &mut StructuredWebhookOverride, previous: &StructuredWebhookOverride) {
    for receiver in &mut next.receivers {
        let Some(previous_receiver) = previous
            .receivers
            .iter()
            .find(|candidate| candidate.id.trim() == receiver.id)
        else {
            continue;
        };
        if !receiver.clear_url && (receiver.url.trim().is_empty() || is_secret_mask(&receiver.url)) {
            receiver.url.clone_from(&previous_receiver.url);
            receiver.clear_url = previous_receiver.clear_url;
        }
        if !receiver.clear_password && (receiver.password.trim().is_empty() || is_secret_mask(&receiver.password)) {
            receiver.password.clone_from(&previous_receiver.password);
            receiver.clear_password = previous_receiver.clear_password;
        }
        for header in &mut receiver.headers {
            let Some(previous_header) = previous_receiver
                .headers
                .iter()
                .find(|candidate| candidate.name.trim().eq_ignore_ascii_case(&header.name))
            else {
                continue;
            };
            if !header.clear_value && (header.value.trim().is_empty() || is_secret_mask(&header.value)) {
                header.value.clone_from(&previous_header.value);
                header.clear_value = previous_header.clear_value;
            }
        }
    }
}

fn normalize_sensitive_field_identities(data: &mut AdminData) {
    if let Some(webhook) = &mut data.webhook {
        for receiver in &mut webhook.receivers {
            receiver.id = receiver.id.trim().to_string();
            for header in &mut receiver.headers {
                header.name = header.name.trim().to_string();
            }
        }
    }
}

fn validate_sensitive_field_identities(data: &AdminData) -> Result<()> {
    if let Some(webhook) = &data.webhook {
        validate_webhook_identities(webhook)?;
    }
    Ok(())
}

fn normalize_wechat_override(config: &mut WechatOverride) {
    config.corp_id = config.corp_id.trim().to_string();
    config.agent_id = config.agent_id.trim().to_string();
    config.title = config.title.trim().to_string();
    normalize_secret(&mut config.corp_secret, &mut config.clear_corp_secret);
}

fn normalize_email_override(config: &mut EmailOverride) {
    config.server = config.server.trim().to_string();
    config.username = config.username.trim().to_string();
    config.to = config.to.trim().to_string();
    config.subject = config.subject.trim().to_string();
    config.title = config.title.trim().to_string();
    normalize_secret(&mut config.password, &mut config.clear_password);
}

fn normalize_webhook_override(config: &mut StructuredWebhookOverride) {
    for receiver in &mut config.receivers {
        receiver.id = receiver.id.trim().to_string();
        receiver.name = receiver.name.trim().to_string();
        receiver.url = receiver.url.trim().to_string();
        receiver.username = receiver.username.trim().to_string();
        normalize_secret(&mut receiver.url, &mut receiver.clear_url);
        normalize_secret(&mut receiver.password, &mut receiver.clear_password);
        for header in &mut receiver.headers {
            header.name = header.name.trim().to_string();
            normalize_secret(&mut header.value, &mut header.clear_value);
        }
    }
}

fn normalize_secret(value: &mut String, clear: &mut bool) {
    if *clear {
        value.clear();
        return;
    }
    if value.trim().is_empty() || is_secret_mask(value) {
        value.clear();
        return;
    }
    *clear = false;
}

fn validate_admin_data(data: &AdminData) -> Result<()> {
    if let Some(wechat) = &data.wechat {
        for template in [
            &wechat.online_tpl,
            &wechat.offline_tpl,
            &wechat.expire_tpl,
            &wechat.health_tpl,
        ] {
            validate_minijinja_template(template, "WeChat")?;
        }
    }
    if let Some(email) = &data.email {
        validate_email_override(email)?;
    }
    if let Some(webhook) = &data.webhook {
        validate_structured_webhook(webhook)?;
    }
    if let Some(log) = &data.log {
        validate_log_override(log)?;
    }
    Ok(())
}

fn validate_email_override(config: &EmailOverride) -> Result<()> {
    if !config.server.is_empty() && !valid_smtp_relay(&config.server) {
        anyhow::bail!("Email SMTP relay is invalid");
    }
    if !config.username.is_empty() && !valid_email_address(&config.username) {
        anyhow::bail!("Email username is not a valid address");
    }
    if !config.to.is_empty() && !valid_email_recipients(&config.to) {
        anyhow::bail!("Email recipients are invalid");
    }
    for template in [
        &config.online_tpl,
        &config.offline_tpl,
        &config.expire_tpl,
        &config.health_tpl,
    ] {
        validate_minijinja_template(template, "Email")?;
    }
    Ok(())
}

fn validate_structured_webhook(config: &StructuredWebhookOverride) -> Result<()> {
    validate_webhook_identities(config)?;
    for receiver in &config.receivers {
        if receiver.name.is_empty() {
            anyhow::bail!("Webhook receiver name is required");
        }
        if !(1..=60).contains(&receiver.timeout) {
            anyhow::bail!("Webhook timeout must be between 1 and 60 seconds");
        }
        if !receiver.url.is_empty() {
            validate_http_url(&receiver.url)?;
        } else if receiver.enabled {
            anyhow::bail!("Enabled Webhook receiver URL is required");
        }
        if !receiver.body_tpl.is_empty() {
            validate_minijinja_template(&receiver.body_tpl, "Webhook")?;
        } else if receiver.enabled {
            anyhow::bail!("Enabled Webhook receiver template is required");
        }

        for header in &receiver.headers {
            if !header.value.is_empty() {
                HeaderValue::from_str(&header.value).map_err(|_| anyhow::anyhow!("Webhook header value is invalid"))?;
            }
        }
    }
    Ok(())
}

fn validate_webhook_identities(config: &StructuredWebhookOverride) -> Result<()> {
    let mut receiver_ids = HashSet::new();
    for receiver in &config.receivers {
        if !is_stable_id(&receiver.id) {
            anyhow::bail!("Webhook receiver ID is invalid");
        }
        if !receiver_ids.insert(receiver.id.as_str()) {
            anyhow::bail!("Webhook receiver IDs must be unique");
        }

        let mut header_names = HashSet::new();
        for header in &receiver.headers {
            let normalized_name = header.name.to_ascii_lowercase();
            if normalized_name.is_empty() || !header_names.insert(normalized_name) {
                anyhow::bail!("Webhook header names must be non-empty and unique");
            }
            header
                .name
                .parse::<HeaderName>()
                .map_err(|_| anyhow::anyhow!("Webhook header name is invalid"))?;
        }
    }
    Ok(())
}

fn validate_http_url(value: &str) -> Result<()> {
    let parsed = url::Url::parse(value).map_err(|_| anyhow::anyhow!("Webhook URL is invalid"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        anyhow::bail!("Webhook URL must use http or https");
    }
    Ok(())
}

fn validate_log_override(config: &LogOverride) -> Result<()> {
    if config.enabled && config.tpl.trim().is_empty() {
        anyhow::bail!("Log template is required when enabled");
    }
    validate_minijinja_template(&config.tpl, "Log")?;
    if template_uses_loader(&config.tpl) {
        anyhow::bail!("Log template cannot load external templates");
    }
    Ok(())
}

fn valid_smtp_relay(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && url::Host::parse(value).is_ok()
}

fn valid_email_address(value: &str) -> bool {
    value.trim().parse::<Address>().is_ok()
}

fn valid_email_recipients(value: &str) -> bool {
    let mut recipients = value
        .split([';', ','])
        .map(str::trim)
        .filter(|recipient| !recipient.is_empty())
        .peekable();
    recipients.peek().is_some() && recipients.all(valid_email_address)
}

fn template_uses_loader(template: &str) -> bool {
    let mut remaining = template;
    while let Some(start) = remaining.find("{%") {
        remaining = &remaining[start + 2..];
        let Some(end) = remaining.find("%}") else {
            break;
        };
        let statement = remaining[..end].trim().trim_start_matches(['-', '+']).trim_start();
        if statement
            .split_whitespace()
            .next()
            .is_some_and(|keyword| matches!(keyword, "include" | "extends" | "import" | "from"))
        {
            return true;
        }
        remaining = &remaining[end + 2..];
    }
    false
}

fn validate_minijinja_template(template: &str, label: &str) -> Result<()> {
    if template.is_empty() {
        return Ok(());
    }
    if template.len() > 64 * 1024 {
        anyhow::bail!("{label} template is too large");
    }
    Environment::new()
        .template_from_str(template)
        .map(|_| ())
        .map_err(|err| anyhow::anyhow!("{label} template is invalid: {err}"))
}

fn is_stable_id(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_alphanumeric())
        && value.len() <= 64
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
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
    fn public_snapshot_masks_all_new_notification_secrets() {
        let data = AdminData {
            wechat: Some(WechatOverride {
                corp_secret: "wechat-secret".into(),
                ..Default::default()
            }),
            email: Some(EmailOverride {
                password: "smtp-secret".into(),
                ..Default::default()
            }),
            webhook: Some(StructuredWebhookOverride {
                receivers: vec![StructuredWebhookReceiver {
                    url: "https://hooks.example/secret".into(),
                    password: "basic-secret".into(),
                    headers: vec![WebhookHeaderOverride {
                        name: "Authorization".into(),
                        value: "Bearer secret".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };

        let public = public_admin_data(&data);
        let json = serde_json::to_string(&public).unwrap();
        for secret in [
            "wechat-secret",
            "smtp-secret",
            "hooks.example/secret",
            "basic-secret",
            "Bearer secret",
        ] {
            assert!(!json.contains(secret));
        }
    }

    fn structured_receiver(id: &str) -> StructuredWebhookReceiver {
        StructuredWebhookReceiver {
            id: id.into(),
            name: "Operations".into(),
            enabled: true,
            url: "https://hooks.example/private".into(),
            username: "operator".into(),
            password: "basic-secret".into(),
            timeout: 10,
            headers: vec![WebhookHeaderOverride {
                name: "Authorization".into(),
                value: "Bearer private".into(),
                ..Default::default()
            }],
            body_tpl: "{{ event }}".into(),
            ..Default::default()
        }
    }

    #[test]
    fn new_notification_overrides_round_trip_in_private_storage() {
        let data = AdminData {
            wechat: Some(WechatOverride {
                corp_secret: "wechat-secret".into(),
                ..Default::default()
            }),
            email: Some(EmailOverride {
                password: "smtp-secret".into(),
                ..Default::default()
            }),
            webhook: Some(StructuredWebhookOverride {
                enabled: true,
                receivers: vec![structured_receiver("ops-primary")],
            }),
            log: Some(LogOverride {
                enabled: true,
                tpl: "{{ event }}".into(),
            }),
            ..Default::default()
        };

        let json = serde_json::to_string(&data).unwrap();
        let decoded: AdminData = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.wechat.unwrap().corp_secret, "wechat-secret");
        assert_eq!(decoded.email.unwrap().password, "smtp-secret");
        let receiver = &decoded.webhook.unwrap().receivers[0];
        assert_eq!(receiver.id, "ops-primary");
        assert_eq!(receiver.url, "https://hooks.example/private");
        assert_eq!(receiver.headers[0].value, "Bearer private");
        assert_eq!(decoded.log.unwrap().tpl, "{{ event }}");
    }

    #[test]
    fn webhook_secrets_merge_by_receiver_id_and_case_insensitive_header_name() {
        let current = AdminData {
            webhook: Some(StructuredWebhookOverride {
                enabled: true,
                receivers: vec![structured_receiver("ops-primary")],
            }),
            ..Default::default()
        };
        let mut next_receiver = structured_receiver("ops-primary");
        next_receiver.url.clear();
        next_receiver.password = "********".into();
        next_receiver.headers[0].name = "authorization".into();
        next_receiver.headers[0].value.clear();
        let mut next = AdminData {
            webhook: Some(StructuredWebhookOverride {
                enabled: true,
                receivers: vec![next_receiver],
            }),
            ..Default::default()
        };

        merge_sensitive_fields(&mut next, &current);
        normalize_admin_data(&mut next);

        let receiver = &next.webhook.unwrap().receivers[0];
        assert_eq!(receiver.url, "https://hooks.example/private");
        assert_eq!(receiver.password, "basic-secret");
        assert_eq!(receiver.headers[0].value, "Bearer private");
    }

    #[test]
    fn webhook_secret_merge_normalizes_receiver_and_header_identity_first() {
        let current = AdminData {
            webhook: Some(StructuredWebhookOverride {
                enabled: true,
                receivers: vec![structured_receiver("ops-primary")],
            }),
            ..Default::default()
        };
        let mut next_receiver = structured_receiver(" ops-primary ");
        next_receiver.url.clear();
        next_receiver.password.clear();
        next_receiver.headers[0].name = " authorization ".into();
        next_receiver.headers[0].value.clear();
        let next = AdminData {
            webhook: Some(StructuredWebhookOverride {
                enabled: true,
                receivers: vec![next_receiver],
            }),
            ..Default::default()
        };

        let prepared = prepare_replacement(next, &current).unwrap();
        let receiver = &prepared.webhook.unwrap().receivers[0];
        assert_eq!(receiver.id, "ops-primary");
        assert_eq!(receiver.url, "https://hooks.example/private");
        assert_eq!(receiver.password, "basic-secret");
        assert_eq!(receiver.headers[0].name, "authorization");
        assert_eq!(receiver.headers[0].value, "Bearer private");
    }

    #[test]
    fn smtp_password_preserves_surrounding_whitespace() {
        let next = AdminData {
            email: Some(EmailOverride {
                password: "  smtp-secret\t".into(),
                ..Default::default()
            }),
            ..Default::default()
        };

        let prepared = prepare_replacement(next, &AdminData::default()).unwrap();
        assert_eq!(prepared.email.unwrap().password, "  smtp-secret\t");
    }

    #[test]
    fn webhook_basic_auth_password_preserves_surrounding_whitespace() {
        let mut receiver = structured_receiver("ops-primary");
        receiver.password = "  basic-secret\t".into();
        let next = AdminData {
            webhook: Some(StructuredWebhookOverride {
                enabled: true,
                receivers: vec![receiver],
            }),
            ..Default::default()
        };

        let prepared = prepare_replacement(next, &AdminData::default()).unwrap();
        assert_eq!(prepared.webhook.unwrap().receivers[0].password, "  basic-secret\t");
    }

    #[test]
    fn webhook_header_value_preserves_surrounding_whitespace() {
        let mut receiver = structured_receiver("ops-primary");
        receiver.headers[0].value = "  Bearer private  ".into();
        let next = AdminData {
            webhook: Some(StructuredWebhookOverride {
                enabled: true,
                receivers: vec![receiver],
            }),
            ..Default::default()
        };

        let prepared = prepare_replacement(next, &AdminData::default()).unwrap();
        assert_eq!(
            prepared.webhook.unwrap().receivers[0].headers[0].value,
            "  Bearer private  "
        );
    }

    #[test]
    fn telegram_bark_and_wechat_secrets_preserve_surrounding_whitespace() {
        let next = AdminData {
            tgbot: Some(TgbotOverride {
                bot_token: "  bot-token\t".into(),
                chat_id: " chat-id ".into(),
                ..Default::default()
            }),
            bark: Some(BarkOverride {
                device_key: "  device-key\t".into(),
                ..Default::default()
            }),
            wechat: Some(WechatOverride {
                corp_secret: "  corp-secret\t".into(),
                ..Default::default()
            }),
            ..Default::default()
        };

        let prepared = prepare_replacement(next, &AdminData::default()).unwrap();
        let tgbot = prepared.tgbot.unwrap();
        assert_eq!(tgbot.bot_token, "  bot-token\t");
        assert_eq!(tgbot.chat_id, " chat-id ");
        assert_eq!(prepared.bark.unwrap().device_key, "  device-key\t");
        assert_eq!(prepared.wechat.unwrap().corp_secret, "  corp-secret\t");
    }

    #[test]
    fn new_notification_secrets_support_explicit_clear() {
        let current = AdminData {
            wechat: Some(WechatOverride {
                corp_secret: "wechat-secret".into(),
                ..Default::default()
            }),
            email: Some(EmailOverride {
                password: "smtp-secret".into(),
                ..Default::default()
            }),
            webhook: Some(StructuredWebhookOverride {
                enabled: true,
                receivers: vec![structured_receiver("ops-primary")],
            }),
            ..Default::default()
        };
        let mut receiver = structured_receiver("ops-primary");
        receiver.enabled = false;
        receiver.clear_url = true;
        receiver.clear_password = true;
        receiver.headers[0].clear_value = true;
        let next = AdminData {
            wechat: Some(WechatOverride {
                clear_corp_secret: true,
                ..Default::default()
            }),
            email: Some(EmailOverride {
                clear_password: true,
                ..Default::default()
            }),
            webhook: Some(StructuredWebhookOverride {
                enabled: true,
                receivers: vec![receiver],
            }),
            ..Default::default()
        };

        let prepared = prepare_replacement(next, &current).unwrap();

        assert!(prepared.wechat.unwrap().corp_secret.is_empty());
        assert!(prepared.email.unwrap().password.is_empty());
        let receiver = &prepared.webhook.unwrap().receivers[0];
        assert!(!receiver.enabled);
        assert!(receiver.url.is_empty());
        assert!(receiver.password.is_empty());
        assert!(receiver.headers[0].value.is_empty());
    }

    #[test]
    fn structured_webhook_validation_rejects_unsafe_or_ambiguous_receivers() {
        let mut data = AdminData {
            webhook: Some(StructuredWebhookOverride {
                enabled: true,
                receivers: vec![structured_receiver("bad id")],
            }),
            ..Default::default()
        };
        assert!(validate_admin_data(&data).is_err());

        let receiver = &mut data.webhook.as_mut().unwrap().receivers[0];
        receiver.id = "valid-id".into();
        receiver.url = "ftp://hooks.example/private".into();
        assert!(validate_admin_data(&data).is_err());

        let receiver = &mut data.webhook.as_mut().unwrap().receivers[0];
        receiver.url = "https://hooks.example/private".into();
        receiver.timeout = 61;
        assert!(validate_admin_data(&data).is_err());

        let receiver = &mut data.webhook.as_mut().unwrap().receivers[0];
        receiver.timeout = 5;
        receiver.headers.push(WebhookHeaderOverride {
            name: "authorization".into(),
            value: "duplicate".into(),
            ..Default::default()
        });
        assert!(validate_admin_data(&data).is_err());

        let receiver = &mut data.webhook.as_mut().unwrap().receivers[0];
        receiver.headers.pop();
        receiver.body_tpl = "{% if %}".into();
        assert!(validate_admin_data(&data).is_err());
    }

    #[test]
    fn email_and_log_validation_rejects_invalid_values() {
        let mut data = AdminData {
            email: Some(EmailOverride {
                username: "not-an-email".into(),
                to: "valid@example.com; invalid".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(validate_admin_data(&data).is_err());

        data.email = None;
        data.log = Some(LogOverride {
            enabled: true,
            tpl: "{% include '../secret' %}".into(),
        });
        assert!(validate_admin_data(&data).is_err());

        data.log.as_mut().unwrap().tpl = "{%   include '../secret' %}".into();
        assert!(validate_admin_data(&data).is_err());
    }

    #[test]
    fn invalid_stateful_replacement_keeps_memory_and_disk_unchanged() {
        let path = std::env::temp_dir().join(format!("serverstatus-admin-invalid-save-{}.json", uuid::Uuid::new_v4()));
        let current = AdminData {
            email: Some(EmailOverride {
                username: "valid@example.com".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let invalid = AdminData {
            email: Some(EmailOverride {
                username: "invalid".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let state = AdminState {
            path: path.to_string_lossy().into_owned(),
            data: Mutex::new(current.clone()),
        };
        write_data(&state, current).unwrap();
        let memory_before = serde_json::to_string(&*state.data.lock().unwrap()).unwrap();
        let disk_before = fs::read_to_string(&path).unwrap();

        assert!(replace_state_data(&state, invalid).is_err());
        assert_eq!(
            serde_json::to_string(&*state.data.lock().unwrap()).unwrap(),
            memory_before
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), disk_before);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn effective_configs_merge_base_with_admin_overrides() {
        let data = AdminData {
            wechat: Some(WechatOverride {
                enabled: true,
                corp_id: "admin-corp".into(),
                ..Default::default()
            }),
            email: Some(EmailOverride {
                enabled: true,
                subject: "Admin subject".into(),
                ..Default::default()
            }),
            log: Some(LogOverride {
                enabled: true,
                tpl: "{{ event }}".into(),
            }),
            ..Default::default()
        };
        let base_wechat = notifier::wechat::Config {
            corp_id: "base-corp".into(),
            corp_secret: "base-secret".into(),
            agent_id: "1001".into(),
            ..Default::default()
        };
        let base_email = notifier::email::Config {
            server: "smtp.example.com".into(),
            username: "sender@example.com".into(),
            password: "base-password".into(),
            to: "ops@example.com".into(),
            subject: "Base subject".into(),
            ..Default::default()
        };
        let base_log = notifier::log::Config {
            enabled: false,
            log_dir: "/legacy/logs".into(),
            tpl: "legacy".into(),
        };

        let wechat = effective_wechat_config_from_data(&data, &base_wechat);
        let email = effective_email_config_from_data(&data, &base_email);
        let log = effective_log_config_from_data(&data, &base_log);

        assert!(wechat.enabled);
        assert_eq!(wechat.corp_id, "admin-corp");
        assert_eq!(wechat.corp_secret, "base-secret");
        assert_eq!(email.subject, "Admin subject");
        assert_eq!(email.password, "base-password");
        assert!(log.enabled);
        assert_eq!(log.log_dir, ADMIN_NOTIFICATION_LOG_DIR);
        assert_eq!(log.tpl, "{{ event }}");

        assert_eq!(effective_wechat_config(&base_wechat).corp_id, "base-corp");
        assert_eq!(effective_email_config(&base_email).subject, "Base subject");
        assert_eq!(effective_log_config(&base_log).log_dir, "/legacy/logs");
    }

    #[test]
    fn effective_webhook_preserves_legacy_until_admin_override_exists() {
        let legacy = notifier::webhook::Config {
            enabled: true,
            receiver: vec![notifier::webhook::Receiver {
                enabled: true,
                url: "https://legacy.example/hook".into(),
                timeout: 5,
                script: "[true, #{}]".into(),
                ..Default::default()
            }],
        };

        assert!(matches!(
            effective_webhook_override_from_data(&AdminData::default(), &legacy),
            EffectiveWebhookConfig::Legacy(_)
        ));
        assert!(matches!(
            effective_webhook_override(&legacy),
            EffectiveWebhookConfig::Legacy(_)
        ));

        let data = AdminData {
            webhook: Some(StructuredWebhookOverride {
                enabled: true,
                receivers: vec![structured_receiver("ops-primary")],
            }),
            ..Default::default()
        };
        assert!(matches!(
            effective_webhook_override_from_data(&data, &legacy),
            EffectiveWebhookConfig::Structured(_)
        ));
    }

    #[test]
    fn configured_notification_methods_returns_all_six_ready_methods() {
        let mut cfg: crate::config::Config = toml::from_str("").unwrap();
        cfg.tgbot.enabled = true;
        cfg.tgbot.bot_token = "token".into();
        cfg.tgbot.chat_id = "chat".into();
        cfg.bark.enabled = true;
        cfg.bark.device_key = "device".into();
        cfg.wechat.enabled = true;
        cfg.wechat.corp_id = "corp".into();
        cfg.wechat.corp_secret = "secret".into();
        cfg.wechat.agent_id = "1001".into();
        cfg.email.enabled = true;
        cfg.email.server = "smtp.example.com".into();
        cfg.email.username = "sender@example.com".into();
        cfg.email.password = "password".into();
        cfg.email.to = "ops@example.com".into();
        cfg.log.enabled = true;
        cfg.log.log_dir = "/legacy/logs".into();
        cfg.log.tpl = "{{ event }}".into();

        let data = AdminData {
            webhook: Some(StructuredWebhookOverride {
                enabled: true,
                receivers: vec![structured_receiver("ops-primary")],
            }),
            ..Default::default()
        };

        assert_eq!(
            configured_notification_methods_from_data(&data, &cfg),
            ["tg", "bark", "wechat", "email", "webhook", "log"]
        );
        assert_eq!(configured_notification_methods(&cfg).len(), 5);

        cfg.email.username = "not-an-email".into();
        assert!(!configured_notification_methods_from_data(&data, &cfg)
            .iter()
            .any(|method| method == "email"));
    }

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
    fn deleted_host_marker_is_removed_when_host_reports_again() {
        let mut data = AdminData {
            deleted_hosts: vec!["srv-return".to_string(), "srv-other".to_string()],
            ..Default::default()
        };

        assert!(remove_deleted_host_marker_from_data(&mut data, "srv-return"));
        assert_eq!(data.deleted_hosts, vec!["srv-other"]);
        assert!(!remove_deleted_host_marker_from_data(&mut data, "srv-return"));
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
                        uid: "srv-expired".to_string(),
                        token_hash: install_token_hash("it_expired"),
                        expires_at: 1,
                    },
                ),
                (
                    "valid".to_string(),
                    InstallTokenOverride {
                        gid: "default".to_string(),
                        uid: "srv-valid".to_string(),
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
    fn install_tokens_are_bound_to_uid_and_consumed_once() {
        let raw_token = "it_valid";
        let mut data = AdminData {
            access_keys: HashMap::from([(
                "default".to_string(),
                AccessKeyOverride {
                    source_gid: "default".to_string(),
                    password: "default-pass".to_string(),
                    ..Default::default()
                },
            )]),
            install_tokens: HashMap::from([(
                "valid".to_string(),
                InstallTokenOverride {
                    gid: "default".to_string(),
                    uid: "srv-1".to_string(),
                    token_hash: install_token_hash(raw_token),
                    expires_at: 100,
                },
            )]),
            ..Default::default()
        };

        assert!(consume_install_token_from_data(&mut data, &HashMap::new(), raw_token, "srv-2", 50).is_none());
        assert!(data.install_tokens.contains_key("valid"));

        let group = consume_install_token_from_data(&mut data, &HashMap::new(), raw_token, "srv-1", 50)
            .expect("token should resolve for the bound uid");
        assert_eq!(group.gid, "default");
        assert_eq!(group.password, "default-pass");
        assert!(data.install_tokens.is_empty());

        assert!(consume_install_token_from_data(&mut data, &HashMap::new(), raw_token, "srv-1", 50).is_none());
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
