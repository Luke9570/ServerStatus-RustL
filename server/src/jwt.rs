use axum::{
    extract::FromRequestParts,
    http::{header::HeaderMap, request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json, RequestPartsExt,
};
use axum_extra::{
    headers::{authorization::Bearer, Authorization},
    TypedHeader,
};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::G_CONFIG;

const ADMIN_SCOPE: &str = "admin";
const TOKEN_TTL_SECONDS: u64 = 3 * 24 * 3600;
const LOGIN_MAX_FAILURES: u32 = 5;
const LOGIN_LOCK_SECONDS: usize = 5 * 60;

static LOGIN_ATTEMPTS: Lazy<Mutex<HashMap<String, LoginAttempt>>> = Lazy::new(Default::default);

pub static KEYS: Lazy<Keys> = Lazy::new(|| {
    let cfg = G_CONFIG.get().unwrap();
    Keys::new(cfg.jwt_secret.as_ref().unwrap().as_bytes())
});

pub async fn authorize(headers: HeaderMap, Json(payload): Json<AuthPayload>) -> Result<Json<AuthBody>, AuthError> {
    let username = payload.username.trim();
    if username.is_empty() || payload.password.is_empty() {
        return Err(AuthError::MissingCredentials);
    }
    let login_key = login_attempt_key(&headers, username);
    let now = unix_ts();
    if login_attempt_limited(&mut LOGIN_ATTEMPTS.lock().unwrap(), &login_key, now) {
        return Err(AuthError::TooManyAttempts);
    }

    let mut auth_ok = false;
    if let Some(cfg) = G_CONFIG.get() {
        auth_ok = cfg.admin_auth(username, &payload.password);
    }
    if !auth_ok {
        record_login_failure(&mut LOGIN_ATTEMPTS.lock().unwrap(), &login_key, now);
        return Err(AuthError::WrongCredentials);
    }
    clear_login_attempts(&mut LOGIN_ATTEMPTS.lock().unwrap(), &login_key);

    let claims = Claims {
        sub: username.to_owned(),
        scope: ADMIN_SCOPE.to_owned(),
        iat: now,
        exp: now.saturating_add(usize::try_from(TOKEN_TTL_SECONDS).unwrap_or(usize::MAX)),
        pwdv: crate::admin::admin_session_version(),
    };
    let token = encode(&Header::default(), &claims, &KEYS.encoding).map_err(|_| AuthError::TokenCreation)?;

    Ok(Json(AuthBody::new(token)))
}

pub struct Keys {
    pub encoding: EncodingKey,
    pub decoding: DecodingKey,
}

impl Keys {
    pub fn new(secret: &[u8]) -> Self {
        Self {
            encoding: EncodingKey::from_secret(secret),
            decoding: DecodingKey::from_secret(secret),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub scope: String,
    pub iat: usize,
    pub exp: usize,
    #[serde(default)]
    pub pwdv: u64,
}

#[derive(Debug, Serialize)]
pub struct AuthBody {
    pub access_token: String,
    pub token_type: String,
}

#[derive(Debug, Deserialize)]
pub struct AuthPayload {
    pub username: String,
    pub password: String,
}

#[derive(Debug)]
pub enum AuthError {
    WrongCredentials,
    MissingCredentials,
    TokenCreation,
    InvalidToken,
    TooManyAttempts,
}

impl Display for Claims {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "sub: {}\nscope: {}\niat: {}\nexp: {}\npwdv: {}",
            self.sub, self.scope, self.iat, self.exp, self.pwdv
        )
    }
}

impl AuthBody {
    pub fn new(access_token: String) -> Self {
        Self {
            access_token,
            token_type: "Bearer".to_string(),
        }
    }
}

impl<S> FromRequestParts<S> for Claims
where
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let TypedHeader(Authorization(bearer)) = parts
            .extract::<TypedHeader<Authorization<Bearer>>>()
            .await
            .map_err(|_| AuthError::InvalidToken)?;
        let token_data = decode::<Claims>(bearer.token(), &KEYS.decoding, &Validation::default())
            .map_err(|_| AuthError::InvalidToken)?;
        let claims = token_data.claims;
        if claims.scope != ADMIN_SCOPE {
            return Err(AuthError::InvalidToken);
        }
        let cfg = G_CONFIG.get().ok_or(AuthError::InvalidToken)?;
        if crate::admin::effective_admin_user(cfg.admin_user.as_deref()).as_deref() != Some(claims.sub.as_str()) {
            return Err(AuthError::InvalidToken);
        }
        if claims.pwdv != crate::admin::admin_session_version() {
            return Err(AuthError::InvalidToken);
        }

        Ok(claims)
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, error_message) = match self {
            AuthError::WrongCredentials => (StatusCode::UNAUTHORIZED, "Wrong credentials"),
            AuthError::MissingCredentials => (StatusCode::BAD_REQUEST, "Missing credentials"),
            AuthError::TokenCreation => (StatusCode::INTERNAL_SERVER_ERROR, "Token creation error"),
            AuthError::InvalidToken => (StatusCode::UNAUTHORIZED, "Invalid token"),
            AuthError::TooManyAttempts => (StatusCode::TOO_MANY_REQUESTS, "Too many login attempts"),
        };
        let body = Json(json!({
            "error": error_message,
        }));
        (status, body).into_response()
    }
}

fn unix_ts() -> usize {
    usize::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()).unwrap_or(usize::MAX)
}

#[derive(Debug, Clone, Default)]
struct LoginAttempt {
    failures: u32,
    locked_until: usize,
}

fn login_attempt_key(headers: &HeaderMap, username: &str) -> String {
    let ip = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("unknown");
    format!("{}:{}", ip, username.trim().to_ascii_lowercase())
}

fn login_attempt_limited(attempts: &mut HashMap<String, LoginAttempt>, key: &str, now: usize) -> bool {
    let Some(attempt) = attempts.get(key) else {
        return false;
    };
    if attempt.locked_until > now {
        return true;
    }
    if attempt.locked_until > 0 {
        attempts.remove(key);
    }
    false
}

fn record_login_failure(attempts: &mut HashMap<String, LoginAttempt>, key: &str, now: usize) {
    let attempt = attempts.entry(key.to_string()).or_default();
    attempt.failures = attempt.failures.saturating_add(1);
    if attempt.failures >= LOGIN_MAX_FAILURES {
        attempt.locked_until = now.saturating_add(LOGIN_LOCK_SECONDS);
    }
}

fn clear_login_attempts(attempts: &mut HashMap<String, LoginAttempt>, key: &str) {
    attempts.remove(key);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_attempts_lock_after_repeated_failures() {
        let mut attempts = HashMap::new();
        let key = "127.0.0.1:admin";
        for _ in 0..LOGIN_MAX_FAILURES {
            assert!(!login_attempt_limited(&mut attempts, key, 100));
            record_login_failure(&mut attempts, key, 100);
        }

        assert!(login_attempt_limited(&mut attempts, key, 101));
        assert!(!login_attempt_limited(&mut attempts, key, 100 + LOGIN_LOCK_SECONDS + 1));
    }

    #[test]
    fn login_attempts_clear_after_success() {
        let mut attempts = HashMap::new();
        let key = "127.0.0.1:admin";
        record_login_failure(&mut attempts, key, 100);
        clear_login_attempts(&mut attempts, key);

        assert!(!attempts.contains_key(key));
    }
}
