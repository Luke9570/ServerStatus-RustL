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
use std::error::Error as _;
use std::fmt::Write as _;
use tokio::time::Duration;

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
    match admin::replace(payload) {
        Ok(_) => Json(json!({
            "code": 0,
            "message": "saved",
            "data": admin::public_snapshot(),
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

#[derive(Debug, Default, Deserialize)]
pub struct NotifyTestPayload {
    #[serde(default)]
    tgbot: Option<admin::TgbotOverride>,
    #[serde(default)]
    bark: Option<admin::BarkOverride>,
}

pub async fn test_admin_notification(
    _claims: jwt::Claims,
    Path(kind): Path<String>,
    Json(payload): Json<NotifyTestPayload>,
) -> impl IntoResponse {
    let cfg = G_CONFIG.get().unwrap();
    match kind.as_str() {
        "tgbot" | "telegram" | "tg" => {
            let mut config = admin::effective_tgbot_config(&cfg.tgbot);
            if let Some(override_data) = payload.tgbot {
                config.enabled = override_data.enabled;
                override_nonempty_string(&mut config.bot_token, override_data.bot_token);
                override_nonempty_string(&mut config.chat_id, override_data.chat_id);
                override_nonempty_string(&mut config.title, override_data.title);
                override_nonempty_string(&mut config.expire_tpl, override_data.expire_tpl);
                override_nonempty_string(&mut config.health_tpl, override_data.health_tpl);
            }
            send_tgbot_test(config).await
        }
        "bark" => {
            let mut config = admin::effective_bark_config(&cfg.bark);
            if let Some(mut override_data) = payload.bark {
                admin::normalize_bark_override(&mut override_data);
                config.enabled = override_data.enabled;
                override_nonempty_string(&mut config.server, override_data.server);
                override_nonempty_string(&mut config.device_key, override_data.device_key);
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
            }
            send_bark_test(config).await
        }
        _ => json_error(StatusCode::NOT_FOUND, "不支持的通知方式"),
    }
}

fn override_nonempty_string(target: &mut String, value: String) {
    if !value.trim().is_empty() {
        *target = value;
    }
}

async fn send_tgbot_test(config: crate::notifier::tgbot::Config) -> Response {
    if !config.enabled {
        return json_error(StatusCode::BAD_REQUEST, "请先启用 Telegram");
    }
    if config.bot_token.trim().is_empty() || config.chat_id.trim().is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Telegram Bot Token 和 Chat ID 不能为空");
    }

    let tg_url = format!("https://api.telegram.org/bot{}/sendMessage", config.bot_token.trim());
    let mut data = HashMap::new();
    data.insert("chat_id", config.chat_id);
    data.insert(
        "text",
        format!(
            "{}\nServerStatus 后台通知测试已发送",
            if config.title.trim().is_empty() {
                "ServerStatus"
            } else {
                config.title.trim()
            }
        ),
    );

    match reqwest::Client::new()
        .post(tg_url)
        .timeout(Duration::from_secs(10))
        .json(&data)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => notify_test_ok("Telegram"),
        Ok(resp) => json_error(StatusCode::BAD_GATEWAY, &format!("Telegram 接口返回 {}", resp.status())),
        Err(err) => json_error(
            StatusCode::BAD_GATEWAY,
            &format!("Telegram 测试失败: {}", request_error_detail(&err)),
        ),
    }
}

async fn send_bark_test(config: crate::notifier::bark::Config) -> Response {
    if !config.enabled {
        return json_error(StatusCode::BAD_REQUEST, "请先启用 Bark");
    }
    if config.device_key.trim().is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Bark Device Key 不能为空");
    }

    let server = config.server.trim_end_matches('/');
    let push_url = if server.ends_with("/push") {
        server.to_string()
    } else {
        format!("{server}/push")
    };
    let mut data = HashMap::new();
    data.insert("device_key".to_string(), config.device_key);
    data.insert(
        "title".to_string(),
        if config.title.trim().is_empty() {
            "ServerStatus".to_string()
        } else {
            config.title
        },
    );
    data.insert("body".to_string(), "ServerStatus 后台通知测试已发送".to_string());
    for (key, value) in [
        ("group", config.group),
        ("icon", config.icon),
        ("sound", config.sound),
        ("url", config.url),
    ] {
        if !value.trim().is_empty() {
            data.insert(key.to_string(), value);
        }
    }

    match reqwest::Client::new()
        .post(push_url)
        .timeout(Duration::from_secs(config.timeout.max(1)))
        .json(&data)
        .send()
        .await
    {
        Ok(resp) => bark_test_response(resp).await,
        Err(err) => json_error(
            StatusCode::BAD_GATEWAY,
            &format!("Bark 测试失败: {}", request_error_detail(&err)),
        ),
    }
}

fn request_error_detail(err: &reqwest::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut source = err.source();
    while let Some(err) = source {
        let detail = err.to_string();
        if !parts.iter().any(|part| part == &detail) {
            parts.push(detail);
        }
        source = err.source();
    }
    parts.join(": ")
}

async fn bark_test_response(resp: reqwest::Response) -> Response {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let detail = short_response_body(&body);
        return if detail.is_empty() {
            json_error(StatusCode::BAD_GATEWAY, &format!("Bark 接口返回 {status}"))
        } else {
            json_error(StatusCode::BAD_GATEWAY, &format!("Bark 接口返回 {status}: {detail}"))
        };
    }

    let body = body.trim();
    if body.is_empty() {
        return notify_test_ok("Bark");
    }

    let Ok(payload) = serde_json::from_str::<Value>(body) else {
        return notify_test_ok("Bark");
    };
    let Some(code) = payload.get("code") else {
        return notify_test_ok("Bark");
    };
    if bark_success_code(code) {
        return notify_test_ok("Bark");
    }

    let message = payload
        .get("message")
        .or_else(|| payload.get("error"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map_or_else(|| short_response_body(body), ToString::to_string);
    json_error(StatusCode::BAD_GATEWAY, &format!("Bark 推送失败: {message}"))
}

fn bark_success_code(code: &Value) -> bool {
    code.as_i64().is_some_and(|value| value == 0 || value == 200)
        || code.as_str().is_some_and(|value| value == "0" || value == "200")
}

fn short_response_body(body: &str) -> String {
    let body = body.trim();
    if body.chars().count() <= 180 {
        body.to_string()
    } else {
        format!("{}...", body.chars().take(180).collect::<String>())
    }
}

fn notify_test_ok(kind: &str) -> Response {
    Json(json!({
        "code": 0,
        "message": format!("{kind} test sent"),
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
    match admin::purge_deleted_hosts(&names) {
        Ok(data) => {
            if let Some(stats_mgr) = G_STATS_MGR.get() {
                stats_mgr.purge_hosts(&purge_set);
            }
            Json(json!({
                "code": 0,
                "message": "deleted hosts purged",
                "data": data,
            }))
            .into_response()
        }
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
    let panel_url = panel_base_url(cfg, req_header);
    let agent_url = agent_base_url(cfg, req_header);
    let uid = query_text(params, "uid").unwrap_or_else(random_server_id);
    let alias = query_text(params, "alias");
    let interval = query_u32(&params, "interval", 1, 1, 86_400).to_string();
    let mut query = Vec::new();
    push_query_pair(&mut query, "gid", &group.gid);
    push_query_pair(&mut query, "pass", &group.password);
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
    format!("'{}'", value.replace('\'', "'\"'\"'"))
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
    let pass = params.get("pass").unwrap_or(&invalid);
    let uid = params.get("uid").unwrap_or(&invalid);
    let gid = params.get("gid").unwrap_or(&invalid);
    let alias = params.get("alias").unwrap_or(&invalid);

    if pass.is_empty() || (uid.is_empty() && gid.is_empty()) || (uid.is_empty() && alias.is_empty()) {
        return (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED.to_string()).into_response();
    }

    // auth
    let mut auth_ok = false;
    if let Some(cfg) = G_CONFIG.get() {
        if gid.is_empty() {
            auth_ok = cfg.auth(uid, pass);
        } else {
            auth_ok = cfg.group_auth(gid, pass);
        }
    }
    if !auth_ok {
        return (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED.to_string()).into_response();
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

    // build client opts
    let mut client_opts = format!(r#"-a "{server_url}" -p "{pass}""#);
    if debug {
        client_opts.push_str(" -d");
    }
    if vnstat {
        client_opts.push_str(" -n");
    }
    if 1 < vnstat_mr && vnstat_mr <= 28 {
        let _ = write!(client_opts, r" --vnstat-mr {vnstat_mr}");
    }
    if disable_ping {
        client_opts.push_str(" --disable-ping");
    }
    if disable_tupd {
        client_opts.push_str(" --disable-tupd");
    }
    if disable_extra {
        client_opts.push_str(" --disable-extra");
    }
    if weight > 0 {
        let _ = write!(client_opts, r" -w {weight}");
    }
    if !gid.is_empty() {
        let _ = write!(client_opts, r#" -g "{gid}""#);
        let _ = write!(client_opts, r#" --alias "{alias}""#);
    }
    if !uid.is_empty() {
        let _ = write!(client_opts, r#" -u "{uid}""#);
    }
    if !notify {
        client_opts.push_str(" --disable-notify");
    }
    if !host_type.is_empty() {
        let _ = write!(client_opts, r#" -t "{host_type}""#);
    }
    if !location.is_empty() {
        let _ = write!(client_opts, r#" --location "{location}""#);
    }
    if !cm.is_empty() && cm.contains(':') {
        let _ = write!(client_opts, r#" --cm "{cm}""#);
    }
    if !ct.is_empty() && ct.contains(':') {
        let _ = write!(client_opts, r#" --ct "{ct}""#);
    }
    if !cu.is_empty() && cu.contains(':') {
        let _ = write!(client_opts, r#" --cu "{cu}""#);
    }

    if !iface.is_empty() {
        let _ = write!(client_opts, r#" --iface "{iface}""#);
    }
    if !exclude_iface.is_empty() {
        let _ = write!(client_opts, r#" --exclude-iface "{exclude_iface}""#);
    }

    if interval > 0 {
        let _ = write!(client_opts, r" --interval {interval}");
    }

    let ip_source = params.get("ip-source").unwrap_or(&invalid);
    if !ip_source.is_empty() {
        let _ = write!(client_opts, r#" --ip-source "{ip_source}""#);
    }

    jinja::render_template(
        KIND,
        "client-init",
        context!(
            pass => pass, uid => uid, gid => gid, alias => alias,
            vnstat => vnstat, weight => weight, cn => cn,
            domain => domain, scheme => scheme,
            server_url => server_url, workspace => workspace,
            client_opts => client_opts,
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

pub async fn get_map(
    // _claims: jwt::Claims
    _auth: auth::AdminAuth,
) -> Response {
    render_jinja_ht_tpl("map")
}

#[allow(clippy::too_many_lines)]
pub async fn get_detail(
    // _claims: jwt::Claims
    _auth: auth::AdminAuth,
) -> Response {
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
pub async fn report(_auth: auth::HostAuth, req_header: HeaderMap, body: Bytes) -> impl IntoResponse {
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

    if let Some(mgr) = G_STATS_MGR.get() {
        if mgr.report(json_data.unwrap()).is_err() {
            return StatusCode::BAD_REQUEST;
        }
    }

    StatusCode::OK
}
