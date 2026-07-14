use crate::assets::Asset;
use axum::extract::{Path, Query};
use axum::{
    body::Bytes,
    http::{header, header::HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use minijinja::context;
use prettytable::Table;
use prost::Message;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Write as _;

use stat_common::{server_status::StatRequest, utils::bytes2human};

use crate::admin;
use crate::auth;
use crate::jinja;
use crate::jwt;
use crate::G_CONFIG;
use crate::G_STATS_MGR;

const KIND: &str = "http";

pub async fn get_stats_json() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        G_STATS_MGR.get().unwrap().get_stats_json(),
    )
}

#[allow(unused)]
pub fn get_site_config_json() -> impl IntoResponse {
    // TODO
    ([(header::CONTENT_TYPE, "application/json")], "{}")
}

pub async fn admin_api(_claims: jwt::Claims, Path(path): Path<String>) -> Json<Value> {
    match path.as_str() {
        "stats.json" => {
            let resp = G_STATS_MGR.get().unwrap().get_all_info().unwrap();
            return Json(resp);
        }
        "config.json" => {
            let resp = G_CONFIG.get().unwrap().to_admin_json_value();
            return Json(resp);
        }
        _ => {
            //
        }
    }

    Json(json!({ "code": 0, "message": "ok" }))
}

pub async fn admin_settings(_claims: jwt::Claims) -> Json<Value> {
    Json(json!({
        "code": 0,
        "message": "ok",
        "data": admin::public_snapshot(),
    }))
}

pub async fn save_admin_settings(_claims: jwt::Claims, Json(payload): Json<admin::AdminData>) -> impl IntoResponse {
    if admin::validate_replacement(&payload).is_err() {
        return json_error(StatusCode::BAD_REQUEST, "settings validation failed");
    }

    match admin::replace(payload) {
        Ok(_) => {
            if let Some(mgr) = G_STATS_MGR.get() {
                mgr.refresh_admin_overrides();
            }
            Json(json!({
                "code": 0,
                "message": "saved",
                "data": admin::public_snapshot(),
            }))
            .into_response()
        }
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "settings could not be saved"),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct NotifyTestPayload {
    #[serde(default)]
    tgbot: Option<admin::TgbotOverride>,
    #[serde(default)]
    bark: Option<admin::BarkOverride>,
    #[serde(default)]
    wechat: Option<admin::WechatOverride>,
    #[serde(default)]
    email: Option<admin::EmailOverride>,
    #[serde(default)]
    webhook: Option<admin::StructuredWebhookOverride>,
    #[serde(default)]
    log: Option<admin::LogOverride>,
}

pub async fn test_admin_notification(
    _claims: jwt::Claims,
    Path(kind): Path<String>,
    Json(payload): Json<NotifyTestPayload>,
) -> impl IntoResponse {
    let cfg = G_CONFIG.get().unwrap();
    let result = match kind.as_str() {
        "tgbot" | "telegram" | "tg" => {
            if let Some(mut override_data) = payload.tgbot {
                let mut config = admin::effective_tgbot_config(&cfg.tgbot);
                admin::normalize_tgbot_override(&mut override_data);
                config.enabled = override_data.enabled;
                if override_data.clear_bot_token {
                    config.bot_token.clear();
                } else {
                    override_nonempty_string(&mut config.bot_token, override_data.bot_token);
                }
                if override_data.clear_chat_id {
                    config.chat_id.clear();
                } else {
                    override_nonempty_string(&mut config.chat_id, override_data.chat_id);
                }
                override_nonempty_string(&mut config.title, override_data.title);
                override_nonempty_string(&mut config.expire_tpl, override_data.expire_tpl);
                override_nonempty_string(&mut config.health_tpl, override_data.health_tpl);
                crate::notifier::tgbot::test(&config).await
            } else {
                crate::notifier::test_effective_notification("tgbot", cfg).await
            }
        }
        "bark" => {
            if let Some(mut override_data) = payload.bark {
                let mut config = admin::effective_bark_config(&cfg.bark);
                admin::normalize_bark_override(&mut override_data);
                config.enabled = override_data.enabled;
                override_nonempty_string(&mut config.server, override_data.server);
                if override_data.clear_device_key {
                    config.device_key.clear();
                } else {
                    override_nonempty_string(&mut config.device_key, override_data.device_key);
                }
                override_nonempty_string(&mut config.title, override_data.title);
                override_nonempty_string(&mut config.group, override_data.group);
                override_nonempty_string(&mut config.icon, override_data.icon);
                override_nonempty_string(&mut config.sound, override_data.sound);
                override_nonempty_string(&mut config.url, override_data.url);
                override_nonempty_string(&mut config.expire_tpl, override_data.expire_tpl);
                override_nonempty_string(&mut config.health_tpl, override_data.health_tpl);
                if let Some(timeout) = override_data.timeout {
                    config.timeout = timeout;
                }
                crate::notifier::bark::test(&config).await
            } else {
                crate::notifier::test_effective_notification("bark", cfg).await
            }
        }
        "wechat" => {
            if let Some(mut override_data) = payload.wechat {
                let mut config = admin::effective_wechat_config(&cfg.wechat);
                admin::normalize_wechat_override(&mut override_data);
                config.enabled = override_data.enabled;
                override_nonempty_string(&mut config.corp_id, override_data.corp_id);
                if override_data.clear_corp_secret {
                    config.corp_secret.clear();
                } else {
                    override_nonempty_string(&mut config.corp_secret, override_data.corp_secret);
                }
                override_nonempty_string(&mut config.agent_id, override_data.agent_id);
                override_nonempty_string(&mut config.title, override_data.title);
                override_nonempty_string(&mut config.online_tpl, override_data.online_tpl);
                override_nonempty_string(&mut config.offline_tpl, override_data.offline_tpl);
                override_nonempty_string(&mut config.expire_tpl, override_data.expire_tpl);
                override_nonempty_string(&mut config.health_tpl, override_data.health_tpl);
                crate::notifier::wechat::test(&config).await
            } else {
                crate::notifier::test_effective_notification("wechat", cfg).await
            }
        }
        "email" => {
            if let Some(mut override_data) = payload.email {
                let mut config = admin::effective_email_config(&cfg.email);
                admin::normalize_email_override(&mut override_data);
                config.enabled = override_data.enabled;
                override_nonempty_string(&mut config.server, override_data.server);
                override_nonempty_string(&mut config.username, override_data.username);
                if override_data.clear_password {
                    config.password.clear();
                } else {
                    override_nonempty_string(&mut config.password, override_data.password);
                }
                override_nonempty_string(&mut config.to, override_data.to);
                override_nonempty_string(&mut config.subject, override_data.subject);
                override_nonempty_string(&mut config.title, override_data.title);
                override_nonempty_string(&mut config.online_tpl, override_data.online_tpl);
                override_nonempty_string(&mut config.offline_tpl, override_data.offline_tpl);
                override_nonempty_string(&mut config.expire_tpl, override_data.expire_tpl);
                override_nonempty_string(&mut config.health_tpl, override_data.health_tpl);
                crate::notifier::email::test(&config).await
            } else {
                crate::notifier::test_effective_notification("email", cfg).await
            }
        }
        "webhook" => {
            if let Some(mut override_data) = payload.webhook {
                let mut override_config = match admin::effective_webhook_override(&cfg.webhook) {
                    admin::EffectiveWebhookConfig::Structured(config) => config,
                    admin::EffectiveWebhookConfig::Legacy(_) => admin::StructuredWebhookOverride::default(),
                };
                admin::normalize_webhook_override(&mut override_data);
                admin::merge_webhook_secrets(&mut override_data, &override_config);
                override_config.enabled = override_data.enabled;
                if !override_data.receivers.is_empty() {
                    override_config.receivers = override_data.receivers;
                }
                crate::notifier::webhook::test(&admin::EffectiveWebhookConfig::Structured(override_config)).await
            } else {
                crate::notifier::test_effective_notification("webhook", cfg).await
            }
        }
        "log" => {
            if let Some(override_data) = payload.log {
                let mut config = admin::effective_log_config(&cfg.log);
                config.enabled = override_data.enabled;
                override_nonempty_string(&mut config.tpl, override_data.tpl);
                crate::notifier::log::test(&config).await
            } else {
                crate::notifier::test_effective_notification("log", cfg).await
            }
        }
        _ => return json_error(StatusCode::NOT_FOUND, "notification type is unsupported"),
    };

    match result {
        Ok(()) => notify_test_ok(),
        Err(crate::notifier::NotificationTestError::UnsupportedKind) => {
            json_error(StatusCode::NOT_FOUND, "notification type is unsupported")
        }
        Err(crate::notifier::NotificationTestError::InvalidConfiguration) => {
            json_error(StatusCode::BAD_REQUEST, "notification configuration is invalid")
        }
        Err(crate::notifier::NotificationTestError::DeliveryFailed) => {
            json_error(StatusCode::BAD_GATEWAY, "notification delivery failed")
        }
    }
}

fn override_nonempty_string(target: &mut String, value: String) {
    if !value.trim().is_empty() {
        *target = value;
    }
}

fn notify_test_ok() -> Response {
    Json(json!({
        "code": 0,
        "message": "notification test sent",
    }))
    .into_response()
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({
            "code": 1,
            "message": message,
        })),
    )
        .into_response()
}

pub async fn purge_deleted_host(_claims: jwt::Claims, Path(name): Path<String>) -> impl IntoResponse {
    purge_deleted_hosts(vec![name])
}

pub async fn clear_deleted_hosts(_claims: jwt::Claims) -> impl IntoResponse {
    purge_deleted_hosts(admin::snapshot().deleted_hosts)
}

fn purge_deleted_hosts(names: Vec<String>) -> Response {
    let purge_set: HashSet<String> = names
        .iter()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    let result = if let Some(stats_mgr) = G_STATS_MGR.get() {
        stats_mgr.purge_hosts_transaction(&purge_set, || admin::purge_deleted_hosts(&names))
    } else {
        admin::purge_deleted_hosts(&names)
    };
    purge_deleted_hosts_response(result)
}

fn purge_deleted_hosts_response(result: anyhow::Result<admin::AdminData>) -> Response {
    match result {
        Ok(data) => Json(json!({
            "code": 0,
            "message": "deleted hosts purged",
            "data": data,
        }))
        .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "code": 1,
                "message": err.to_string(),
            })),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct AdminPasswordPayload {
    current_password: String,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    new_password: Option<String>,
    #[serde(default)]
    admin_path: Option<String>,
}

pub async fn change_admin_password(
    _claims: jwt::Claims,
    Json(payload): Json<AdminPasswordPayload>,
) -> impl IntoResponse {
    let cfg = G_CONFIG.get().unwrap();
    match admin::update_admin_credentials(
        cfg.admin_user.as_deref(),
        cfg.admin_pass.as_deref(),
        &payload.current_password,
        payload.username.as_deref(),
        payload.new_password.as_deref(),
        payload.admin_path.as_deref(),
    ) {
        Ok(()) => Json(json!({
            "code": 0,
            "message": "admin credentials updated",
        }))
        .into_response(),
        Err(err) => {
            let (status, message) = match err {
                admin::PasswordUpdateError::InvalidUsername => (
                    StatusCode::BAD_REQUEST,
                    "用户名只能包含字母、数字、下划线、横线、点和 @，最长 64 字节",
                ),
                admin::PasswordUpdateError::InvalidAdminPath => (
                    StatusCode::BAD_REQUEST,
                    "后台入口只能是一段路径，可包含字母、数字、横线和下划线",
                ),
                admin::PasswordUpdateError::WrongCurrentPassword => (StatusCode::BAD_REQUEST, "当前密码不正确"),
                admin::PasswordUpdateError::NewPasswordTooShort => {
                    (StatusCode::BAD_REQUEST, "新密码至少需要 12 个字符")
                }
                admin::PasswordUpdateError::NewPasswordTooLong => {
                    (StatusCode::BAD_REQUEST, "新密码不能超过 256 个字节")
                }
                admin::PasswordUpdateError::NewPasswordUnchanged => {
                    (StatusCode::BAD_REQUEST, "新密码不能和当前密码相同")
                }
                admin::PasswordUpdateError::NothingChanged => (StatusCode::BAD_REQUEST, "没有需要保存的账号更改"),
                admin::PasswordUpdateError::HashFailed | admin::PasswordUpdateError::SaveFailed => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "修改密码失败")
                }
            };
            (
                status,
                Json(json!({
                    "code": 1,
                    "message": message,
                })),
            )
                .into_response()
        }
    }
}

pub async fn admin_access_command(
    _claims: jwt::Claims,
    Path(gid): Path<String>,
    req_header: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let cfg = G_CONFIG.get().unwrap();
    let Some(group) = admin::effective_group(&cfg.hosts_group_map, &gid) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "code": 1,
                "message": "access key not found",
            })),
        )
            .into_response();
    };

    access_command_response(group, cfg, &req_header, &params)
}

pub async fn admin_default_access_command(
    _claims: jwt::Claims,
    req_header: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let cfg = G_CONFIG.get().unwrap();
    match admin::ensure_default_access_key() {
        Ok(group) => access_command_response(group, cfg, &req_header, &params),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "code": 1,
                "message": err.to_string(),
            })),
        )
            .into_response(),
    }
}

fn access_command_response(
    group: crate::config::HostGroup,
    cfg: &crate::config::Config,
    req_header: &HeaderMap,
    params: &HashMap<String, String>,
) -> Response {
    if let Some(message) = validate_u32_param(params, "interval", 1, 86_400) {
        return json_error(StatusCode::BAD_REQUEST, &message);
    }
    if let Some(message) = validate_u32_param(params, "weight", 1, 1_000_000) {
        return json_error(StatusCode::BAD_REQUEST, &message);
    }
    let panel_url = panel_base_url(cfg, req_header);
    let agent_url = agent_base_url(cfg, req_header);
    let uid = query_text(params, "uid").unwrap_or_else(random_server_id);
    let alias = query_text(params, "alias");
    let interval = query_u32(&params, "interval", 1, 1, 86_400).to_string();
    let install_token = match admin::create_install_token(&group.gid, &uid) {
        Ok(token) => token,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    };
    let mut query = Vec::new();
    push_query_pair(&mut query, "gid", &group.gid);
    push_query_pair(&mut query, "token", &install_token.token);
    push_query_pair(&mut query, "uid", &uid);
    if let Some(alias) = &alias {
        push_query_pair(&mut query, "alias", alias);
    }
    push_query_pair(&mut query, "interval", &interval);

    if let Some(location) = query_text(&params, "loc") {
        push_query_pair(&mut query, "loc", &location);
    }
    if let Some(host_type) = query_text(&params, "type") {
        push_query_pair(&mut query, "type", &host_type);
    }
    if let Some(weight) = query_u32_opt(&params, "weight", 1, 1_000_000) {
        push_query_pair(&mut query, "weight", &weight.to_string());
    }
    for key in ["ping", "tupd", "extra", "notify", "vnstat", "cn"] {
        if let Some(value) = query_toggle(&params, key) {
            push_query_pair(&mut query, key, value);
        }
    }

    let install_url = format!("{}/i?{}", panel_url.trim_end_matches('/'), query.join("&"));
    let script = format!("curl -fsSL {} | bash", shell_quote(&install_url));

    Json(json!({
        "code": 0,
        "message": "ok",
        "data": {
            "gid": group.gid,
            "panel_url": panel_url,
            "agent_url": agent_url,
            "install_url": install_url,
            "script": script,
            "token_expires_in": install_token.expires_in,
            "token_expires_at": install_token.expires_at,
            "params": {
                "uid": uid,
                "alias": alias,
                "interval": interval,
            },
        },
    }))
    .into_response()
}

pub async fn admin_access_secret(_claims: jwt::Claims, Path(gid): Path<String>) -> impl IntoResponse {
    let cfg = G_CONFIG.get().unwrap();
    let Some(group) = admin::effective_group(&cfg.hosts_group_map, &gid) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "code": 1,
                "message": "access key not found",
            })),
        )
            .into_response();
    };

    Json(json!({
        "code": 0,
        "message": "ok",
        "data": {
            "gid": group.gid,
            "password": group.password,
        },
    }))
    .into_response()
}

fn panel_base_url(cfg: &crate::config::Config, req_header: &HeaderMap) -> String {
    if let Some(url) = admin::access_base_url() {
        return normalize_base_url(&url);
    }
    if !cfg.server_url.trim().is_empty() {
        return normalize_base_url(&cfg.server_url);
    }

    forwarded_base_url(req_header)
}

fn agent_base_url(cfg: &crate::config::Config, req_header: &HeaderMap) -> String {
    if let Some(url) = admin::agent_base_url() {
        return normalize_base_url(&url);
    }
    if let Some(url) = admin::access_base_url() {
        return normalize_base_url(&url);
    }
    if !cfg.server_url.trim().is_empty() {
        return normalize_base_url(&cfg.server_url);
    }

    forwarded_base_url(req_header)
}

fn forwarded_base_url(req_header: &HeaderMap) -> String {
    let mut scheme = "http".to_string();
    let mut domain = "127.0.0.1:8080".to_string();
    if let Some(value) = req_header.get("x-forwarded-proto") {
        if let Ok(value) = value.to_str() {
            scheme = value.to_string();
        }
    }
    if let Some(value) = req_header.get("host") {
        if let Ok(value) = value.to_str() {
            domain = value.to_string();
        }
    }
    if let Some(value) = req_header.get("x-forwarded-host") {
        if let Ok(value) = value.to_str() {
            domain = value.to_string();
        }
    }
    format!("{scheme}://{domain}")
}

fn normalize_base_url(value: &str) -> String {
    let mut url = value.trim().trim_end_matches('/').to_string();
    if url.ends_with("/report") {
        url.truncate(url.len() - "/report".len());
    }
    if url.is_empty() {
        return url;
    }
    if !url.contains("://") {
        url = format!("http://{url}");
    }
    url
}

fn random_server_id() -> String {
    let value = uuid::Uuid::new_v4().simple().to_string();
    format!("srv-{}", &value[..8])
}

fn query_text(params: &HashMap<String, String>, key: &str) -> Option<String> {
    params
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn query_u32(params: &HashMap<String, String>, key: &str, default: u32, min: u32, max: u32) -> u32 {
    query_u32_opt(params, key, min, max).unwrap_or(default)
}

fn query_u32_opt(params: &HashMap<String, String>, key: &str, min: u32, max: u32) -> Option<u32> {
    params
        .get(key)
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|value| (min..=max).contains(value))
}

fn validate_u32_param(params: &HashMap<String, String>, key: &str, min: u32, max: u32) -> Option<String> {
    let value = params
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())?;
    match value.parse::<u32>() {
        Ok(value) if (min..=max).contains(&value) => None,
        _ => Some(format!("{key} 必须是 {min} 到 {max} 之间的整数")),
    }
}

fn query_toggle<'a>(params: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    match params.get(key).map(|value| value.trim().to_ascii_lowercase()) {
        Some(value) if matches!(value.as_str(), "1" | "true" | "yes" | "on") => Some("1"),
        Some(value) if matches!(value.as_str(), "0" | "false" | "no" | "off") => Some("0"),
        _ => None,
    }
}

fn push_query_pair(query: &mut Vec<String>, key: &str, value: &str) {
    query.push(format!("{}={}", query_encode(key), query_encode(value)));
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '/' | ':' | '_' | '-' | '='))
    {
        return value.to_string();
    }
    shell_export_value(value)
}

fn shell_export_value(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn systemd_exec_arg(value: &str) -> String {
    let safe = !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '/' | ':' | '_' | '-' | '=' | ','));
    if safe {
        return value.to_string();
    }
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '%' => escaped.push_str("%%"),
            '$' => escaped.push_str("$$"),
            '\n' | '\r' => escaped.push(' '),
            _ => escaped.push(ch),
        }
    }
    format!("\"{escaped}\"")
}

fn query_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[allow(clippy::unnecessary_wraps)]
pub fn init_jinja_tpl() -> Result<(), anyhow::Error> {
    let detail_data = Asset::get("/jinja/detail.jinja.html").expect("detail.jinja.html not found");
    let detail_html: String = String::from_utf8(detail_data.data.into()).unwrap();
    jinja::add_template(KIND, "detail", detail_html);

    let map_data = Asset::get("/jinja/map.jinja.html").expect("map.jinja.html not found");
    let map_html: String = String::from_utf8(map_data.data.into()).unwrap();
    jinja::add_template(KIND, "map", map_html);

    let client_init_sh = Asset::get("/jinja/client-init.jinja.sh").expect("client-init.jinja.sh not found");
    let client_init_sh_s: String = String::from_utf8(client_init_sh.data.into()).unwrap();
    jinja::add_template(KIND, "client-init", client_init_sh_s);
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub async fn init_client(uri: Uri, req_header: HeaderMap, Query(params): Query<HashMap<String, String>>) -> Response {
    // dbg!(&params);

    // query args
    let invalid = String::new();
    let mut pass = params.get("pass").unwrap_or(&invalid).to_string();
    let uid = params.get("uid").unwrap_or(&invalid);
    let mut gid = params.get("gid").unwrap_or(&invalid).to_string();
    let alias = params.get("alias").unwrap_or(&invalid);
    let install_token = params
        .get("token")
        .or_else(|| params.get("install_token"))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());

    if let Some(token) = install_token {
        let Some(cfg) = G_CONFIG.get() else {
            return script_error(StatusCode::UNAUTHORIZED, "接入令牌无效或已过期");
        };
        let Some(group) = (match admin::consume_install_token(&cfg.hosts_group_map, token, uid) {
            Ok(group) => group,
            Err(err) => {
                error!("consume install token failed => {err:?}");
                None
            }
        }) else {
            return script_error(StatusCode::UNAUTHORIZED, "接入令牌无效或已过期");
        };
        gid = group.gid;
        pass = group.password;
    }

    if pass.is_empty() || (uid.is_empty() && gid.is_empty()) || (uid.is_empty() && alias.is_empty()) {
        return script_error(StatusCode::UNAUTHORIZED, "缺少接入参数，请从后台复制完整接入指令");
    }

    // auth
    let mut auth_ok = false;
    if let Some(cfg) = G_CONFIG.get() {
        if install_token.is_some() {
            auth_ok = true;
        } else if gid.is_empty() {
            auth_ok = cfg.auth(uid, &pass);
        } else {
            auth_ok = cfg.group_auth(&gid, &pass);
        }
    }
    if !auth_ok {
        return script_error(StatusCode::UNAUTHORIZED, "接入密钥无效，请重新复制后台接入指令");
    }

    let mut domain = "localhost".to_string();
    let mut scheme = "http".to_string();
    let mut server_url = String::new();
    let mut workspace = String::new();

    // load deploy config
    if let Some(cfg) = G_CONFIG.get() {
        if let Some(url) = admin::agent_base_url() {
            server_url = format!("{}/report", normalize_base_url(&url).trim_end_matches('/'));
        } else {
            server_url.clone_from(&cfg.server_url);
        }
        workspace.clone_from(&cfg.workspace);
    }
    // build server url
    if server_url.is_empty() {
        if let Some(v) = uri.scheme() {
            scheme = v.to_string();
            debug!("Http Scheme => {scheme}");
        }
        req_header.get("x-forwarded-proto").map(|v| {
            v.to_str().map(|s| {
                debug!("x-forwarded-proto => {s}");
                scheme = s.to_string();
            })
        });

        req_header.get("Host").map(|v| {
            v.to_str().map(|host| {
                debug!("Http Host => {host}");
                domain = host.to_string();
            })
        });
        req_header.get("x-forwarded-host").map(|v| {
            v.to_str().map(|host| {
                debug!("x-forwarded-host => {host}");
                domain = host.to_string();
            })
        });
        server_url = format!("{scheme}://{domain}/report");
    }

    let debug = params.get("debug").is_some_and(|p| p.eq("1"));
    let vnstat = params.get("vnstat").is_some_and(|p| p.eq("1"));
    let disable_ping = params.get("ping").is_some_and(|p| p.eq("0"));
    let disable_tupd = params.get("tupd").is_some_and(|p| p.eq("0"));
    let disable_extra = params.get("extra").is_some_and(|p| p.eq("0"));
    let cn = params.get("cn").is_some_and(|p| p.eq("1"));
    let weight = params
        .get("weight")
        .map_or(0_u64, |p| p.parse::<u64>().unwrap_or(0_u64));
    let vnstat_mr = params
        .get("vnstat-mr")
        .map_or(1_u32, |p| p.parse::<u32>().unwrap_or(1_u32));
    let interval = params
        .get("interval")
        .map_or(1_u32, |p| p.parse::<u32>().unwrap_or(1_u32));

    let notify = params.get("notify").is_none_or(|p| !p.eq("0"));
    let host_type = params.get("type").unwrap_or(&invalid);
    let location = params.get("loc").unwrap_or(&invalid);

    // cm, ct, cu
    let cm = params.get("cm").unwrap_or(&invalid);
    let ct = params.get("ct").unwrap_or(&invalid);
    let cu = params.get("cu").unwrap_or(&invalid);

    let iface = params.get("iface").unwrap_or(&invalid);
    let exclude_iface = params.get("exclude-iface").unwrap_or(&invalid);

    // build client opts for systemd ExecStart
    let mut client_args = vec![
        "-a".to_string(),
        systemd_exec_arg(&server_url),
        "-p".to_string(),
        systemd_exec_arg(&pass),
    ];
    if debug {
        client_args.push("-d".to_string());
    }
    if vnstat {
        client_args.push("-n".to_string());
    }
    if 1 < vnstat_mr && vnstat_mr <= 28 {
        client_args.push("--vnstat-mr".to_string());
        client_args.push(vnstat_mr.to_string());
    }
    if disable_ping {
        client_args.push("--disable-ping".to_string());
    }
    if disable_tupd {
        client_args.push("--disable-tupd".to_string());
    }
    if disable_extra {
        client_args.push("--disable-extra".to_string());
    }
    if weight > 0 {
        client_args.push("-w".to_string());
        client_args.push(weight.to_string());
    }
    if !gid.is_empty() {
        client_args.push("-g".to_string());
        client_args.push(systemd_exec_arg(&gid));
        client_args.push("--alias".to_string());
        client_args.push(systemd_exec_arg(alias));
    }
    if !uid.is_empty() {
        client_args.push("-u".to_string());
        client_args.push(systemd_exec_arg(uid));
    }
    if !notify {
        client_args.push("--disable-notify".to_string());
    }
    if !host_type.is_empty() {
        client_args.push("-t".to_string());
        client_args.push(systemd_exec_arg(host_type));
    }
    if !location.is_empty() {
        client_args.push("--location".to_string());
        client_args.push(systemd_exec_arg(location));
    }
    if !cm.is_empty() && cm.contains(':') {
        client_args.push("--cm".to_string());
        client_args.push(systemd_exec_arg(cm));
    }
    if !ct.is_empty() && ct.contains(':') {
        client_args.push("--ct".to_string());
        client_args.push(systemd_exec_arg(ct));
    }
    if !cu.is_empty() && cu.contains(':') {
        client_args.push("--cu".to_string());
        client_args.push(systemd_exec_arg(cu));
    }

    if !iface.is_empty() {
        client_args.push("--iface".to_string());
        client_args.push(systemd_exec_arg(iface));
    }
    if !exclude_iface.is_empty() {
        client_args.push("--exclude-iface".to_string());
        client_args.push(systemd_exec_arg(exclude_iface));
    }

    if interval > 0 {
        client_args.push("--interval".to_string());
        client_args.push(interval.to_string());
    }

    let ip_source = params.get("ip-source").unwrap_or(&invalid);
    if !ip_source.is_empty() {
        client_args.push("--ip-source".to_string());
        client_args.push(systemd_exec_arg(ip_source));
    }
    let client_opts = client_args.join(" ");
    let workspace_path = workspace.trim_end_matches('/');
    let stat_client_path = if workspace_path.is_empty() {
        "stat_client".to_string()
    } else {
        format!("{workspace_path}/stat_client")
    };

    jinja::render_template(
        KIND,
        "client-init",
        context!(
            pass => shell_export_value(&pass),
            uid => shell_export_value(uid),
            gid => shell_export_value(&gid),
            alias => shell_export_value(alias),
            vnstat => shell_export_value(&vnstat.to_string()),
            weight => shell_export_value(&weight.to_string()),
            cn => shell_export_value(&cn.to_string()),
            domain => shell_export_value(&domain),
            scheme => shell_export_value(&scheme),
            server_url => shell_export_value(&server_url),
            workspace => shell_export_value(&workspace),
            workspace_exec => systemd_exec_arg(&workspace),
            stat_client_exec => systemd_exec_arg(&stat_client_path),
            client_opts => client_opts,
            client_opts_export => shell_export_value(&client_opts),
            pkg_version => env!("CARGO_PKG_VERSION"),
        ),
        false,
    )
    .map(|contents| {
        (
            [
                (header::CONTENT_TYPE, "text/x-sh"),
                (
                    header::CONTENT_DISPOSITION,
                    r#"attachment; filename="ssr-client-init.sh""#,
                ),
            ],
            contents,
        )
            .into_response()
    })
    .unwrap_or(
        //
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::INTERNAL_SERVER_ERROR.to_string(),
        )
            .into_response(),
    )
}

fn script_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        message.to_string(),
    )
        .into_response()
}

fn render_jinja_ht_tpl(tag: &'static str) -> Response {
    let o = G_STATS_MGR.get().unwrap().get_all_info().unwrap();

    jinja::render_template(KIND, tag, context!(resp => &o), false)
        .map(|contents| {
            //
            ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], contents).into_response()
        })
        .unwrap_or(
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                StatusCode::INTERNAL_SERVER_ERROR.to_string(),
            )
                .into_response(),
        )
}

pub async fn get_map(_claims: jwt::Claims) -> Response {
    render_jinja_ht_tpl("map")
}

#[allow(clippy::too_many_lines)]
pub async fn get_detail(_claims: jwt::Claims) -> Response {
    let resp = G_STATS_MGR.get().unwrap().get_stats();
    let o = resp.lock().unwrap();

    let mut table = Table::new();
    table.set_titles(row![
        "#",
        "Id",
        "节点名",
        "位置",
        "在线时间",
        "IP",
        "系统信息",
        "IP信息",
        "磁盘信息"
    ]);
    for (idx, host) in o.servers.iter().enumerate() {
        let sys_info = host
            .sys_info
            .as_ref()
            .map(|o| {
                let mut s = String::new();
                let _ = writeln!(s, "version:        {}", o.version);
                let _ = writeln!(s, "host_name:      {}", o.host_name);
                let _ = writeln!(s, "os_name:        {}", o.os_name);
                let _ = writeln!(s, "os_arch:        {}", o.os_arch);
                let _ = writeln!(s, "os_family:      {}", o.os_family);
                let _ = writeln!(s, "os_release:     {}", o.os_release);
                let _ = writeln!(s, "kernel_version: {}", o.kernel_version);
                let _ = writeln!(s, "cpu_num:        {}", o.cpu_num);
                let _ = writeln!(s, "cpu_brand:      {}", o.cpu_brand);
                let _ = write!(s, "cpu_vender_id:  {}", o.cpu_vender_id);
                s
            })
            .unwrap_or_default();

        let mut di: String = String::new();
        if !host.disks.is_empty() {
            let mut t = Table::new();
            t.set_titles(row!["name", "mp", "fs", "total", "used", "free"]);
            for disk in &host.disks {
                t.add_row(row![
                    disk.name,
                    disk.mount_point,
                    disk.file_system,
                    bytes2human(disk.total, 2, host.si),
                    bytes2human(disk.used, 2, host.si),
                    bytes2human(disk.free, 2, host.si),
                ]);
            }
            di = t.to_string();
        }

        if let Some(ip_info) = &host.ip_info {
            let addrs = [
                ip_info.continent.as_str(),
                ip_info.country.as_str(),
                ip_info.region_name.as_str(),
                ip_info.city.as_str(),
            ]
            .iter()
            .map(|s| s.trim())
            .filter(|&s| !s.is_empty())
            .collect::<Vec<&str>>()
            .join("/");

            let isp = [
                ip_info.isp.as_str(),
                ip_info.org.as_str(),
                ip_info.r#as.as_str(),
                ip_info.asname.as_str(),
            ]
            .iter()
            .map(|s| s.trim())
            .filter(|&s| !s.is_empty())
            .collect::<Vec<&str>>()
            .join("\n");

            table.add_row(row![
                idx.to_string(),
                host.name,
                host.alias,
                host.location,
                host.uptime_str,
                ip_info.query,
                sys_info,
                format!("{addrs}\n{isp}"),
                di
            ]);
        } else {
            table.add_row(row![
                idx.to_string(),
                host.name,
                host.alias,
                host.location,
                host.uptime_str,
                "xx.xx.xx.xx".to_string(),
                sys_info,
                String::new(),
                di
            ]);
        }
    }
    // table.printstd();

    jinja::render_template(KIND, "detail", context!(pretty_content => table.to_string()), true)
        .map(|contents| {
            //
            ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], contents).into_response()
        })
        .unwrap_or(
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                StatusCode::INTERNAL_SERVER_ERROR.to_string(),
            )
                .into_response(),
        )
}

// report
pub async fn report(auth: auth::BasicAuth, req_header: HeaderMap, body: Bytes) -> impl IntoResponse {
    let mut json_data: Option<serde_json::Value> = None;

    let content_type_header = req_header.get(header::CONTENT_TYPE);
    let content_type = content_type_header.and_then(|value| value.to_str().ok());
    if let Some(content_type) = content_type {
        if content_type.starts_with("application/octet-stream") {
            if let Ok(stat) = StatRequest::decode(body) {
                match serde_json::to_value(stat) {
                    Ok(v) => {
                        json_data = Some(v);
                    }
                    Err(err) => {
                        error!("Invalid pb data! {err:?}");
                    }
                }
            }
        } else if content_type.starts_with("application/json") {
            match serde_json::from_slice(&body) {
                Ok(v) => {
                    json_data = Some(v);
                }
                Err(err) => {
                    error!("Invalid json data! {err:?}");
                }
            }
        } else {
            return StatusCode::UNSUPPORTED_MEDIA_TYPE;
        }
    }

    if json_data.is_none() {
        error!("{}", "Invalid json data!");
        return StatusCode::BAD_REQUEST;
    }
    let mut json_data = json_data.unwrap();

    if !authorize_report_payload(&auth, &req_header, &mut json_data) {
        return StatusCode::UNAUTHORIZED;
    }

    if let Some(mgr) = G_STATS_MGR.get() {
        if mgr.report(json_data).is_err() {
            return StatusCode::BAD_REQUEST;
        }
    }

    StatusCode::OK
}

fn authorize_report_payload(auth: &auth::BasicAuth, req_header: &HeaderMap, json_data: &mut Value) -> bool {
    let Some(cfg) = G_CONFIG.get() else {
        return false;
    };
    let (name, gid) = report_payload_identity(json_data);
    let group_auth = req_header
        .get("ssr-auth")
        .and_then(|header| header.to_str().ok())
        .is_some_and(|value| value == "group");
    let existing_gid = G_STATS_MGR.get().and_then(|mgr| mgr.active_host_gid(&name));
    let Some(decision) = auth::verify_report_auth(
        cfg,
        &auth.username,
        &auth.password,
        group_auth,
        &name,
        &gid,
        existing_gid.as_deref(),
    ) else {
        return false;
    };
    if let Some(gid) = decision.override_gid {
        if let Value::Object(map) = json_data {
            map.insert("gid".to_string(), Value::String(gid));
        }
    }
    true
}

fn report_payload_identity(json_data: &Value) -> (String, String) {
    let name = json_data
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let gid = json_data
        .get("gid")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    (name, gid)
}

#[cfg(test)]
mod tests {
    #[test]
    fn shell_export_values_are_single_quoted() {
        assert_eq!(super::shell_export_value("plain"), "'plain'");
        assert_eq!(super::shell_export_value("a'b"), "'a'\"'\"'b'");
        assert_eq!(super::shell_export_value("$(touch /tmp/pwn)"), "'$(touch /tmp/pwn)'");
    }

    #[test]
    fn systemd_exec_args_are_quoted_and_escape_specifiers() {
        assert_eq!(super::systemd_exec_arg("plain"), "plain");
        assert_eq!(super::systemd_exec_arg("RN 1"), "\"RN 1\"");
        assert_eq!(super::systemd_exec_arg("50% node"), "\"50%% node\"");
        assert_eq!(super::systemd_exec_arg("quote\"back\\"), "\"quote\\\"back\\\\\"");
    }

    #[test]
    fn purge_failure_returns_internal_server_error() {
        let response = super::purge_deleted_hosts_response(Err(anyhow::anyhow!("runtime-state save failed")));

        assert_eq!(response.status(), axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    }
}
