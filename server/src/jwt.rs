use axum::{
    extract::FromRequestParts,
    http::{request::Parts, StatusCode},
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
use std::fmt::Display;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::G_CONFIG;

const ADMIN_SCOPE: &str = "admin";
const TOKEN_TTL_SECONDS: u64 = 7 * 24 * 3600;

pub static KEYS: Lazy<Keys> = Lazy::new(|| {
    let cfg = G_CONFIG.get().unwrap();
    Keys::new(cfg.jwt_secret.as_ref().unwrap().as_bytes())
});

pub async fn authorize(Json(payload): Json<AuthPayload>) -> Result<Json<AuthBody>, AuthError> {
    let username = payload.username.trim();
    if username.is_empty() || payload.password.is_empty() {
        return Err(AuthError::MissingCredentials);
    }

    let mut auth_ok = false;
    if let Some(cfg) = G_CONFIG.get() {
        auth_ok = cfg.admin_auth(username, &payload.password);
    }
    if !auth_ok {
        return Err(AuthError::WrongCredentials);
    }

    let now = unix_ts();
    let claims = Claims {
        sub: username.to_owned(),
        scope: ADMIN_SCOPE.to_owned(),
        iat: now,
        exp: now.saturating_add(usize::try_from(TOKEN_TTL_SECONDS).unwrap_or(usize::MAX)),
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
}

impl Display for Claims {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sub: {}\nscope: {}\niat: {}\nexp: {}", self.sub, self.scope, self.iat, self.exp)
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
        if cfg.admin_user.as_deref() != Some(claims.sub.as_str()) {
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
