use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::config::{AppConfig, ThrottleConfig};
use crate::routes::profile::Profile;
use crate::throttle::RateLimiter;

pub const SESSION_TTL: time::Duration = time::Duration::minutes(15);
pub const REFRESH_TTL: time::Duration = time::Duration::days(7);
const THROTTLE_WINDOW: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub behind_proxy: bool,
    pub email_limiter: Arc<RateLimiter<String>>,
    pub ip_limiter: Arc<RateLimiter<IpAddr>>,
}

impl AppState {
    pub async fn new(config: &AppConfig) -> sqlx::Result<Self> {
        let db = PgPoolOptions::new()
            .max_connections(config.db.max_connections)
            .min_connections(config.db.max_connections)
            .acquire_timeout(Duration::from_secs(3))
            .connect(&config.db.url)
            .await?;
        Ok(Self::build(
            db,
            config.server.behind_proxy,
            &config.throttle,
        ))
    }

    pub fn build(db: PgPool, behind_proxy: bool, throttle: &ThrottleConfig) -> Self {
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
        }
    }

    pub async fn migrate(&self) -> Result<(), sqlx::migrate::MigrateError> {
        sqlx::migrate!("./../migrations").run(&self.db).await
    }

    pub async fn add_user(&self, email: &str, password_hash: &str, name: &str) -> sqlx::Result<()> {
        sqlx::query!(
            r#"
                WITH u AS (
                    INSERT INTO users (email, password_hash)
                    VALUES ($1, $2)
                    RETURNING id
                )
                INSERT INTO profiles (user_id, name)
                SELECT id, $3 FROM u
            "#,
            email,
            password_hash,
            name
        )
        .execute(&self.db)
        .await?;
        Ok(())
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

    /// The user behind a live session token.
    pub async fn get_session_user(&self, token_hash: &[u8]) -> sqlx::Result<Option<Uuid>> {
        sqlx::query_scalar!(
            "SELECT user_id FROM sessions WHERE token_hash = $1 AND session_expires_at > now()",
            token_hash
        )
        .fetch_optional(&self.db)
        .await
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

    pub async fn delete_expired_sessions(&self) -> sqlx::Result<u64> {
        let result = sqlx::query!("DELETE FROM sessions WHERE expires_at < now()")
            .execute(&self.db)
            .await?;
        Ok(result.rows_affected())
    }

    /// A profile as seen by `viewer` (`None` = anonymous, so `following` is false).
    pub async fn get_profile(
        &self,
        user_id: Uuid,
        viewer: Option<Uuid>,
    ) -> sqlx::Result<Option<Profile>> {
        // ponytail: count(*) per read via idx_follows_followee_id; denormalize
        // profiles.followers_count if celebrity profiles show up in the benchmark
        sqlx::query_as!(
            Profile,
            r#"
                SELECT p.user_id AS id, p.name, p.pronouns, p.bio,
                       (SELECT count(*) FROM follows WHERE followee_id = p.user_id) AS "followers_count!",
                       EXISTS (SELECT 1 FROM follows WHERE follower_id = $2 AND followee_id = p.user_id) AS "following!"
                FROM profiles p
                WHERE p.user_id = $1
            "#,
            user_id,
            viewer
        )
        .fetch_optional(&self.db)
        .await
    }

    /// Full replacement; returns the updated profile as its owner sees it.
    pub async fn update_profile(
        &self,
        user_id: Uuid,
        name: &str,
        pronouns: Option<&str>,
        bio: Option<&str>,
    ) -> sqlx::Result<Option<Profile>> {
        sqlx::query_as!(
            Profile,
            r#"
                UPDATE profiles
                SET name = $2, pronouns = $3, bio = $4
                WHERE user_id = $1
                RETURNING user_id AS id, name, pronouns, bio,
                          (SELECT count(*) FROM follows WHERE followee_id = $1) AS "followers_count!",
                          false AS "following!"
            "#,
            user_id,
            name,
            pronouns,
            bio
        )
        .fetch_optional(&self.db)
        .await
    }

    /// Idempotent. An unknown `followee_id` surfaces as a foreign-key violation.
    pub async fn follow(&self, follower_id: Uuid, followee_id: Uuid) -> sqlx::Result<()> {
        sqlx::query!(
            "INSERT INTO follows (follower_id, followee_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            follower_id,
            followee_id
        )
        .execute(&self.db)
        .await?;
        Ok(())
    }
}
