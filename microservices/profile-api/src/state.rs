use std::sync::Arc;
use std::time::Duration;

use axum::extract::FromRef;
use common::auth::AuthDb;
use common::profiles::Author;
use common::throttle::BehindProxy;
use sqlx::PgPool;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::config::AppConfig;
use crate::routes::profile::Profile;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    /// The user database as `session_verifier`; only `verify_session()` works on it.
    pub auth_db: PgPool,
    pub behind_proxy: bool,
    pub http: reqwest::Client,
    pub article_api_url: String,
    /// Woken by follow/unfollow so the outbox dispatcher delivers without waiting for its tick.
    pub outbox: Arc<Notify>,
}

impl FromRef<AppState> for AuthDb {
    fn from_ref(state: &AppState) -> Self {
        Self(state.auth_db.clone())
    }
}

impl FromRef<AppState> for BehindProxy {
    fn from_ref(state: &AppState) -> Self {
        Self(state.behind_proxy)
    }
}

impl AppState {
    pub async fn new(config: &AppConfig) -> sqlx::Result<Self> {
        let db = common::config::pool(&config.db).await?;
        let auth_db = common::config::pool(&config.auth_db).await?;
        Ok(Self::build(
            db,
            auth_db,
            config.server.behind_proxy,
            config.article_api.url.clone(),
        ))
    }

    pub fn build(db: PgPool, auth_db: PgPool, behind_proxy: bool, article_api_url: String) -> Self {
        Self {
            db,
            auth_db,
            behind_proxy,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
            article_api_url: article_api_url.trim_end_matches('/').to_owned(),
            outbox: Arc::new(Notify::new()),
        }
    }

    pub async fn migrate(&self) -> Result<(), sqlx::migrate::MigrateError> {
        sqlx::migrate!("./migrations").run(&self.db).await
    }

    /// Batch lookup for article-api and comment-api (`GET /internal/profiles?ids=`).
    pub async fn authors(&self, ids: &[Uuid]) -> sqlx::Result<Vec<Author>> {
        sqlx::query_as!(
            Author,
            "SELECT user_id AS id, name, pronouns FROM profiles WHERE user_id = ANY($1)",
            ids
        )
        .fetch_all(&self.db)
        .await
    }

    /// Called by user-api during registration. `true` if the profile was created now.
    pub async fn create_profile(&self, user_id: Uuid, name: &str) -> sqlx::Result<bool> {
        let result = sqlx::query!(
            "INSERT INTO profiles (user_id, name) VALUES ($1, $2) ON CONFLICT (user_id) DO NOTHING",
            user_id,
            name
        )
        .execute(&self.db)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// A profile as seen by `viewer` (`None` = anonymous, so `following` is false).
    pub async fn get_profile(
        &self,
        user_id: Uuid,
        viewer: Option<Uuid>,
    ) -> sqlx::Result<Option<Profile>> {
        sqlx::query_as!(
            Profile,
            r#"
                SELECT p.user_id AS id, p.name, p.pronouns, p.bio,
                       coalesce(s.followers_count, 0) AS "followers_count!",
                       EXISTS (SELECT 1 FROM follows WHERE follower_id = $2 AND followee_id = p.user_id) AS "following!"
                FROM profiles p
                LEFT JOIN profile_stats s ON s.user_id = p.user_id
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
                          coalesce((SELECT followers_count FROM profile_stats WHERE user_id = $1), 0) AS "followers_count!",
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

    /// Idempotent; returns the followee's follower count after the insert. An unknown
    /// `followee_id` surfaces as a foreign-key violation.
    pub async fn follow(&self, follower_id: Uuid, followee_id: Uuid) -> sqlx::Result<i64> {
        // followers_count is bumped by a trigger, but this statement's snapshot cannot see
        // that, so the inserted row (0 or 1) is added back from RETURNING.
        sqlx::query_scalar!(
            r#"
                WITH ins AS (
                    INSERT INTO follows (follower_id, followee_id) VALUES ($1, $2)
                    ON CONFLICT DO NOTHING RETURNING 1
                )
                SELECT coalesce(s.followers_count, 0) + (SELECT count(*) FROM ins) AS "followers_count!"
                FROM profiles p
                LEFT JOIN profile_stats s ON s.user_id = p.user_id
                WHERE p.user_id = $2
            "#,
            follower_id,
            followee_id
        )
        .fetch_one(&self.db)
        .await
    }

    /// Idempotent; returns the followee's follower count after the delete, `None` if the
    /// profile does not exist.
    pub async fn unfollow(
        &self,
        follower_id: Uuid,
        followee_id: Uuid,
    ) -> sqlx::Result<Option<i64>> {
        sqlx::query_scalar!(
            r#"
                WITH del AS (
                    DELETE FROM follows WHERE follower_id = $1 AND followee_id = $2 RETURNING 1
                )
                SELECT coalesce(s.followers_count, 0) - (SELECT count(*) FROM del) AS "followers_count!"
                FROM profiles p
                LEFT JOIN profile_stats s ON s.user_id = p.user_id
                WHERE p.user_id = $2
            "#,
            follower_id,
            followee_id
        )
        .fetch_optional(&self.db)
        .await
    }
}
