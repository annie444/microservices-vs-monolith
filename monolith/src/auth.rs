use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_extra::extract::CookieJar;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::AppError;
use crate::routes::user::SESSION_COOKIE;
use crate::state::AppState;

/// The signed-in caller. Declaring it as a handler argument costs one primary-key lookup
/// on `sessions`; missing, unknown, or expired session cookies are rejected with 401.
pub struct AuthUser(pub Uuid);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let Ok(jar) = CookieJar::from_request_parts(parts, state).await;
        let cookie = jar.get(SESSION_COOKIE).ok_or(AppError::Unauthorized)?;
        let hash: [u8; 32] = Sha256::digest(cookie.value().as_bytes()).into();
        state
            .get_session_user(&hash)
            .await?
            .map(Self)
            .ok_or(AppError::Unauthorized)
    }
}
