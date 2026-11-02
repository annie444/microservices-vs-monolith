use axum::extract::{FromRef, FromRequestParts, OptionalFromRequestParts};
use axum::http::request::Parts;
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::Duration;
use uuid::Uuid;

use crate::error::AppError;

pub const SESSION_COOKIE: &str = "__Host-session";
pub const REFRESH_COOKIE: &str = "__Host-refresh";
pub const SESSION_TTL: Duration = Duration::minutes(15);
pub const REFRESH_TTL: Duration = Duration::days(7);

/// A pool into the user database that can run `verify_session(bytea)`. user-api uses its own
/// pool; every other service connects as the `session_verifier` role, which can do nothing else.
#[derive(Clone)]
pub struct AuthDb(pub PgPool);

/// The signed-in caller. Declaring it as a handler argument costs one `verify_session` call in
/// the user database; missing, unknown, or expired session cookies are rejected with 401.
/// Declare `Option<AuthUser>` on a public route to treat those cases as anonymous instead.
pub struct AuthUser(pub Uuid);

impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
    AuthDb: FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, AppError> {
        let Ok(jar) = CookieJar::from_request_parts(parts, state).await;
        let cookie = jar.get(SESSION_COOKIE).ok_or(AppError::Unauthorized)?;
        let hash: [u8; 32] = Sha256::digest(cookie.value().as_bytes()).into();
        let AuthDb(db) = AuthDb::from_ref(state);
        verify_session(&db, &hash)
            .await?
            .map(Self)
            .ok_or(AppError::Unauthorized)
    }
}

impl<S> OptionalFromRequestParts<S> for AuthUser
where
    S: Send + Sync,
    AuthDb: FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Option<Self>, AppError> {
        match <Self as FromRequestParts<S>>::from_request_parts(parts, state).await {
            Ok(user) => Ok(Some(user)),
            Err(AppError::Unauthorized) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// The user behind a live session token, via the `SECURITY DEFINER` function the user-api
/// migration creates. Unchecked query: only the user database has it.
pub async fn verify_session(db: &PgPool, token_hash: &[u8]) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar("SELECT verify_session($1)")
        .bind(token_hash)
        .fetch_one(db)
        .await
}

pub fn cookie(name: &'static str, value: String, ttl: Duration) -> Cookie<'static> {
    Cookie::build((name, value))
        .path("/")
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax)
        .max_age(ttl)
        .build()
}

pub fn session_cookies(jar: CookieJar, session: String, refresh: String) -> CookieJar {
    jar.add(cookie(SESSION_COOKIE, session, SESSION_TTL))
        .add(cookie(REFRESH_COOKIE, refresh, REFRESH_TTL))
}
