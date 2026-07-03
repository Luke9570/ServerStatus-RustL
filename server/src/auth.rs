use axum::{
    extract::FromRequestParts,
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Response},
    RequestPartsExt,
};
use axum_extra::{
    headers::{authorization::Basic, Authorization},
    TypedHeader,
};
use serde::{Deserialize, Serialize};

use crate::config::Config;

const DEFAULT_GROUP_ID: &str = "default";

#[derive(Debug, Serialize, Deserialize)]
pub struct BasicAuth {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportAuthDecision {
    pub override_gid: Option<String>,
}

pub fn verify_report_auth(
    cfg: &Config,
    username: &str,
    password: &str,
    group_auth: bool,
    payload_name: &str,
    payload_gid: &str,
    existing_gid: Option<&str>,
) -> Option<ReportAuthDecision> {
    let username = username.trim();
    let password = password.trim();
    let payload_name = payload_name.trim();
    let payload_gid = payload_gid.trim();
    if username.is_empty() || password.is_empty() || payload_name.is_empty() {
        return None;
    }

    if group_auth {
        if !cfg.group_auth(username, password) {
            return None;
        }
        if !payload_gid.is_empty() && payload_gid != username {
            return None;
        }
        return Some(ReportAuthDecision {
            override_gid: Some(username.to_string()),
        });
    }

    if username != payload_name {
        return None;
    }
    if cfg.auth(username, password) {
        return Some(ReportAuthDecision { override_gid: None });
    }
    if !payload_gid.is_empty() || cfg.hosts_map.contains_key(payload_name) {
        return None;
    }
    if existing_gid
        .map(str::trim)
        .filter(|gid| !gid.is_empty() && *gid != DEFAULT_GROUP_ID)
        .is_some()
    {
        return None;
    }
    if !cfg.group_auth(DEFAULT_GROUP_ID, password) {
        return None;
    }

    Some(ReportAuthDecision {
        override_gid: Some(DEFAULT_GROUP_ID.to_string()),
    })
}

impl<S> FromRequestParts<S> for BasicAuth
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // Extract the token from the authorization header
        let TypedHeader(Authorization(basic_auth)) = parts
            .extract::<TypedHeader<Authorization<Basic>>>()
            .await
            .map_err(|_| StatusCode::UNAUTHORIZED.into_response())?;

        Ok(BasicAuth {
            username: basic_auth.username().into(),
            password: basic_auth.password().into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::config;

    fn cfg() -> config::Config {
        config::from_str(
            r#"
            [[hosts]]
            name = "static-1"
            password = "static-pass"

            [[hosts_group]]
            gid = "default"
            password = "default-pass"

            [[hosts_group]]
            gid = "g2"
            password = "g2-pass"
            "#,
        )
        .expect("config should parse")
    }

    #[test]
    fn report_auth_accepts_default_key_for_new_ungrouped_host() {
        let cfg = cfg();

        let decision = super::verify_report_auth(
            &cfg,
            "srv-new",
            "default-pass",
            false,
            "srv-new",
            "",
            None,
        )
        .expect("default fallback should authenticate");

        assert_eq!(decision.override_gid.as_deref(), Some("default"));
    }

    #[test]
    fn report_auth_rejects_default_key_for_existing_static_or_other_group_host() {
        let cfg = cfg();

        assert!(super::verify_report_auth(&cfg, "static-1", "default-pass", false, "static-1", "", None).is_none());
        assert!(
            super::verify_report_auth(&cfg, "srv-old", "default-pass", false, "srv-old", "", Some("g2")).is_none()
        );
    }

    #[test]
    fn report_auth_binds_headers_to_payload_identity() {
        let cfg = cfg();

        assert!(super::verify_report_auth(&cfg, "static-1", "static-pass", false, "other", "", None).is_none());
        assert!(super::verify_report_auth(&cfg, "default", "default-pass", true, "srv", "g2", None).is_none());

        let group = super::verify_report_auth(&cfg, "default", "default-pass", true, "srv", "", None)
            .expect("empty payload gid should be filled from the authenticated group");
        assert_eq!(group.override_gid.as_deref(), Some("default"));
    }

    #[test]
    fn report_auth_accepts_normal_single_and_group_reports() {
        let cfg = cfg();

        assert!(super::verify_report_auth(&cfg, "static-1", "static-pass", false, "static-1", "", None).is_some());
        assert!(super::verify_report_auth(&cfg, "g2", "g2-pass", true, "srv", "g2", None).is_some());
    }
}
