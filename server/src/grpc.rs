// #![allow(unused)]
use anyhow::Result;
use std::str::FromStr;
use tonic::{
    transport::{Certificate, Identity, Server, ServerTlsConfig},
    Request, Response, Status,
};

use stat_common::server_status;
use stat_common::server_status::server_status_server::{ServerStatus, ServerStatusServer};
use stat_common::server_status::StatRequest;

use crate::auth;
use crate::config::Config;
use crate::G_CONFIG;
use crate::G_STATS_MGR;

#[derive(Default)]
pub struct ServerStatusSrv {}

#[tonic::async_trait]
impl ServerStatus for ServerStatusSrv {
    async fn report(&self, request: Request<StatRequest>) -> Result<Response<server_status::Response>, Status> {
        let (username, password, group_auth) = report_auth_parts(&request)?;
        let mut stat = request.into_inner();
        let Some(cfg) = G_CONFIG.get() else {
            return Err(Status::unauthenticated("invalid user/group && pass"));
        };
        let existing_gid = G_STATS_MGR.get().and_then(|mgr| mgr.active_host_gid(&stat.name));
        let Some(decision) = auth::verify_report_auth(
            cfg,
            &username,
            &password,
            group_auth,
            &stat.name,
            &stat.gid,
            existing_gid.as_deref(),
        ) else {
            return Err(Status::unauthenticated("invalid user/group && pass"));
        };
        if let Some(gid) = decision.override_gid {
            stat.gid = gid;
        }

        if let Some(mgr) = G_STATS_MGR.get() {
            match serde_json::to_value(&stat) {
                Ok(v) => {
                    let _ = mgr.report(v);
                }
                Err(err) => {
                    error!("serde_json::to_value err => {err:?}");
                }
            }
        }

        Ok(Response::new(server_status::Response {
            code: 0,
            message: "ok".to_string(),
        }))
    }
}

fn report_auth_parts(req: &Request<StatRequest>) -> Result<(String, String, bool), Status> {
    let group_auth = req
        .metadata()
        .get("ssr-auth")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "group");
    let token = req
        .metadata()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| Status::unauthenticated("invalid user/group && pass"))?;
    let mut parts = token.splitn(2, "@_@");
    let username = parts.next().unwrap_or_default().to_string();
    let password = parts.next().unwrap_or_default().to_string();
    if username.is_empty() || password.is_empty() {
        return Err(Status::unauthenticated("invalid user/group && pass"));
    }
    Ok((username, password, group_auth))
}

pub async fn serv_grpc(cfg: &Config) -> anyhow::Result<()> {
    let sock_addr = cfg.grpc_addr.parse().unwrap();
    let sss = ServerStatusSrv::default();
    let svc = ServerStatusServer::new(sss);

    if cfg.grpc_tls > 0 {
        let mut proto = " + TLS";
        let tls_dir = std::path::PathBuf::from_str(&cfg.tls_dir)?;
        let cert = std::fs::read_to_string(tls_dir.join("server.pem"))?;
        let key = std::fs::read_to_string(tls_dir.join("server.key"))?;
        let identity = Identity::from_pem(cert, key);

        let mut tls = ServerTlsConfig::new().identity(identity);
        if cfg.grpc_tls > 1 {
            let ca = Certificate::from_pem(std::fs::read_to_string(tls_dir.join("ca.pem"))?);
            tls = tls.client_ca_root(ca);
            proto = " + mTLS";
        }

        eprintln!("🚀 listening on grpc://{sock_addr}{proto}");
        Server::builder()
            .tls_config(tls)?
            .add_service(svc)
            .serve(sock_addr)
            .await
            .map_err(anyhow::Error::new)
    } else {
        eprintln!("🚀 listening on grpc://{sock_addr}");
        Server::builder()
            .accept_http1(true)
            .add_service(svc)
            .serve(sock_addr)
            .await
            .map_err(anyhow::Error::new)
    }
}
