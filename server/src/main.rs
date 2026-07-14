#![deny(warnings)]
// #![allow(unused)]
#[macro_use]
extern crate log;
extern crate pretty_env_logger;
#[macro_use]
extern crate prettytable;

use clap::Parser;
use once_cell::sync::OnceCell;
use std::process;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::runtime::Handle;
use tokio::signal;

use axum::{
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Router,
};

mod admin;
mod assets;
mod auth;
mod config;
mod expiry;
mod grpc;
mod http;
mod jinja;
mod jwt;
mod notifier;
mod payload;
mod runtime_state;
mod stats;

static G_CONFIG: OnceCell<crate::config::Config> = OnceCell::new();
static G_STATS_MGR: OnceCell<crate::stats::StatsMgr> = OnceCell::new();

#[derive(Parser, Debug)]
#[command(author, version = env!("APP_VERSION"), about, long_about = None)]
struct Args {
    #[arg(short, long, default_value = "config.toml")]
    config: String,
    #[arg(short = 't', long, help = "config test, default:false")]
    config_test: bool,
    #[arg(long = "notify-test", help = "notify test, default:false")]
    notify_test: bool,
}

fn create_app_router() -> Router {
    Router::new()
        .route("/report", post(http::report))
        .route("/json/stats.json", get(http::get_stats_json)) // 兼容就旧主题
        // .route("/config.pub.json", get(http::get_site_config_json)) // TODO
        .route("/api/admin/authorize", post(jwt::authorize))
        .route(
            "/api/admin/settings",
            get(http::admin_settings).post(http::save_admin_settings),
        )
        .route("/api/admin/notify-test/{kind}", post(http::test_admin_notification))
        .route("/api/admin/password", post(http::change_admin_password))
        .route("/api/admin/deleted-hosts", delete(http::clear_deleted_hosts))
        .route("/api/admin/deleted-hosts/{name}", delete(http::purge_deleted_host))
        .route("/api/admin/access-command", post(http::admin_default_access_command))
        .route("/api/admin/access-command/{gid}", post(http::admin_access_command))
        .route("/api/admin/access-secret/{gid}", get(http::admin_access_secret))
        .route("/api/admin/{path}", get(http::admin_api)) // stats.json || config.json
        .route("/admin", get(admin_index_handler))
        .route("/detail", get(http::get_detail))
        .route("/map", get(http::get_map))
        .route("/i", get(http::init_client))
        .route("/", get(assets::index_handler))
        .fallback(fallback)
}

async fn admin_index_handler() -> Response {
    if admin::request_matches_admin_path(admin::DEFAULT_ADMIN_PATH) {
        assets::admin_index_handler().await.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

async fn fallback(uri: Uri) -> Response {
    if admin::request_matches_admin_path(uri.path()) {
        assets::admin_index_handler().await.into_response()
    } else {
        assets::static_handler(&uri).into_response()
    }
}

/// Waits for Ctrl-C or SIGTERM to initiate graceful shutdown.
#[allow(clippy::missing_panics_doc)]
pub async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    println!("signal received, starting graceful shutdown");
}

fn build_notifiers(cfg: &'static config::Config) -> Vec<Box<dyn notifier::Notifier + Send>> {
    vec![
        Box::new(notifier::ReadinessGate::new(
            notifier::tgbot::TGBot::new(&cfg.tgbot),
            || admin::effective_tgbot_config(&cfg.tgbot).is_ready(),
        )),
        Box::new(notifier::ReadinessGate::new(
            notifier::bark::Bark::new(&cfg.bark),
            || admin::effective_bark_config(&cfg.bark).is_ready(),
        )),
        Box::new(notifier::ReadinessGate::new(
            notifier::wechat::WeChat::new(&cfg.wechat),
            || admin::effective_wechat_config(&cfg.wechat).is_ready(),
        )),
        Box::new(notifier::ReadinessGate::new(
            notifier::email::Email::new(&cfg.email),
            || admin::effective_email_config(&cfg.email).is_ready(),
        )),
        Box::new(notifier::ReadinessGate::new(
            notifier::webhook::Webhook::new(&cfg.webhook),
            || admin::effective_webhook_override(&cfg.webhook).is_ready(),
        )),
        Box::new(notifier::ReadinessGate::new(
            notifier::log::Log::new(&cfg.log),
            || admin::effective_log_config(&cfg.log).is_ready(),
        )),
    ]
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    pretty_env_logger::init();
    let args = Args::parse();

    eprintln!("✨ {} {}", env!("CARGO_BIN_NAME"), env!("APP_VERSION"));
    admin::init().unwrap();

    // config test
    if args.config_test {
        config::test_from_file(&args.config).unwrap();
        eprintln!("✨ the conf file {} syntax is ok", &args.config);
        eprintln!("✨ the conf file {} test is successful", &args.config);
        process::exit(0);
    }

    // config load
    if let Some(cfg) = {
        eprintln!("✨ run in normal mode, load conf from local file `{}", &args.config);
        config::from_file(&args.config)
    } {
        debug!("config loaded");
        G_CONFIG.set(cfg).unwrap();
    } else {
        error!("can't parse config");
        process::exit(1);
    }
    // init tpl
    http::init_jinja_tpl().unwrap();

    // init notifier
    *notifier::NOTIFIER_HANDLE.lock().unwrap() = Some(Handle::current());
    let cfg = G_CONFIG.get().unwrap();
    let notifies = Arc::new(Mutex::new(build_notifiers(cfg)));
    // init notifier end

    // notify test
    if args.notify_test {
        for notifier in &*notifies.lock().unwrap() {
            eprintln!("send test message to {}", notifier.kind());
            if notifier.notify_test().is_err() {
                error!(
                    "notification test failed: kind={}, error=notification failed",
                    notifier.kind()
                );
            }
        }
        thread::sleep(Duration::from_millis(7000)); // TODO: wait
        eprintln!("Please check for notifications");
        process::exit(0);
    }

    // init mgr
    let mut mgr = crate::stats::StatsMgr::new();
    mgr.init(G_CONFIG.get().unwrap(), notifies)?;
    if G_STATS_MGR.set(mgr).is_err() {
        error!("can't set G_STATS_MGR");
        process::exit(1);
    }

    // serv grpc
    tokio::spawn(async move { grpc::serv_grpc(cfg).await });

    let http_addr = cfg.http_addr.clone();
    eprintln!("🚀 listening on http://{http_addr}");

    let listener = TcpListener::bind(&http_addr).await.unwrap();
    axum::serve(listener, create_app_router())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_all_six_with_disabled_malformed_legacy_configs() {
        let mut config: config::Config = toml::from_str("").unwrap();
        config.wechat.enabled = false;
        config.wechat.online_tpl = "{{ invalid".into();
        config.webhook.enabled = false;
        config.webhook.receiver = vec![notifier::webhook::Receiver {
            enabled: true,
            script: "let = invalid".into(),
            ..Default::default()
        }];
        let config = Box::leak(Box::new(config));

        let notifiers = build_notifiers(config);
        let kinds = notifiers
            .iter()
            .map(|notifier| notifier.kind())
            .collect::<Vec<_>>();

        assert_eq!(kinds, ["tgbot", "bark", "wechat", "email", "webhook", "log"]);
    }
}
