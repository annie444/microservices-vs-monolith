use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::FromRef;
use common::auth::{AuthDb, REFRESH_TTL, SESSION_TTL};
use common::config::ThrottleConfig;
use common::error::AppError;
use common::throttle::{BehindProxy, RateLimiter};
use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::config::AppConfig;

const THROTTLE_WINDOW: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub behind_proxy: bool,
    pub email_limiter: Arc<RateLimiter<String>>,
    pub ip_limiter: Arc<RateLimiter<IpAddr>>,
    pub http: reqwest::Client,
    pub profile_api_url: String,
}

/// user-api owns `sessions`, so its own pool is the auth pool.
impl FromRef<AppState> for AuthDb {
    fn from_ref(state: &AppState) -> Self {
        Self(state.db.clone())
    }
}

impl FromRef<AppState> for BehindProxy {
    fn from_ref(state: &AppState) -> Self {
        Self(state.behind_proxy)
    }
}

#[derive(Serialize)]
struct NewProfile<'a> {
    id: Uuid,
    name: &'a str,
}

impl AppState {
    pub async fn new(config: &AppConfig) -> sqlx::Result<Self> {
        let db = common::config::pool(&config.db).await?;
        Ok(Self::build(
            db,
            config.server.behind_proxy,
            &config.throttle,
            config.profile_api.url.clone(),
        ))
    }

    pub fn build(
        db: PgPool,
        behind_proxy: bool,
        throttle: &ThrottleConfig,
        profile_api_url: String,
    ) -> Self {
        Self {
            db,
            behind_proxy,
            email_limiter: Arc::new(RateLimiter::new(
                throttle.email_failures_per_minute,
                THROTTLE_WINDOW,
            )),
            ip_limiter: Arc::new(RateLimiter::new(
                throttle.ip_failures_per_minute,
                THROTTLE_WINDOW,
            )),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
            profile_api_url: profile_api_url.trim_end_matches('/').to_owned(),
        }
    }

    pub async fn migrate(&self) -> Result<(), sqlx::migrate::MigrateError> {
        sqlx::migrate!("./migrations").run(&self.db).await
    }

    pub async fn add_user(&self, email: &str, password_hash: &str) -> sqlx::Result<Uuid> {
        sqlx::query_scalar!(
            "INSERT INTO users (email, password_hash) VALUES ($1, $2) RETURNING id",
            email,
            password_hash
        )
        .fetch_one(&self.db)
        .await
    }

    /// Compensation for a failed profile creation.
    pub async fn delete_user(&self, id: Uuid) -> sqlx::Result<()> {
        sqlx::query!("DELETE FROM users WHERE id = $1", id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    /// `POST {profile_api}/internal/profiles`. Any failure is logged and surfaces as 502.
    pub async fn create_profile(&self, id: Uuid, name: &str) -> Result<(), AppError> {
        let result = self
            .http
            .post(format!("{}/internal/profiles", self.profile_api_url))
            .json(&NewProfile { id, name })
            .send()
            .await
            .and_then(|r| r.error_for_status());
        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::error!(error = %e, "profile-api call failed");
                Err(AppError::Upstream)
            }
        }
    }

    pub async fn get_user_password_hash(
        &self,
        email: &str,
    ) -> sqlx::Result<Option<(Uuid, String)>> {
        let record = sqlx::query!(
            "SELECT id, password_hash FROM users WHERE email = $1",
            email
        )
        .fetch_optional(&self.db)
        .await?;
        Ok(record.map(|r| (r.id, r.password_hash)))
    }

    pub async fn add_session(
        &self,
        user_id: Uuid,
        token_hash: &[u8],
        refresh_token_hash: &[u8],
    ) -> sqlx::Result<()> {
        let now = OffsetDateTime::now_utc();
        sqlx::query!(
            r#"
                INSERT INTO sessions (user_id, token_hash, refresh_token_hash, session_expires_at, expires_at)
                VALUES ($1, $2, $3, $4, $5)
            "#,
            user_id,
            token_hash,
            refresh_token_hash,
            now + SESSION_TTL,
            now + REFRESH_TTL
        )
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// Rotates both tokens, remembering the superseded refresh hash for replay detection.
    /// `false` when the refresh token is unknown or the session has expired.
    pub async fn refresh_session(
        &self,
        old_refresh_token_hash: &[u8],
        token_hash: &[u8],
        refresh_token_hash: &[u8],
    ) -> sqlx::Result<bool> {
        let result = sqlx::query!(
            r#"
                UPDATE sessions
                SET token_hash = $1,
                    prev_refresh_token_hash = refresh_token_hash,
                    refresh_token_hash = $2,
                    session_expires_at = $3,
                    last_seen_at = now()
                WHERE refresh_token_hash = $4 AND expires_at > now()
            "#,
            token_hash,
            refresh_token_hash,
            OffsetDateTime::now_utc() + SESSION_TTL,
            old_refresh_token_hash
        )
        .execute(&self.db)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Deletes the session whose *previous* refresh token was presented. `true` means a replay.
    pub async fn revoke_replayed_session(&self, refresh_token_hash: &[u8]) -> sqlx::Result<bool> {
        let result = sqlx::query!(
            "DELETE FROM sessions WHERE prev_refresh_token_hash = $1",
            refresh_token_hash
        )
        .execute(&self.db)
        .await?;
        Ok(result.rows_affected() > 0)
    }
}
