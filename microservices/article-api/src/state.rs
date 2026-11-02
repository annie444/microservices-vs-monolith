use axum::extract::FromRef;
use common::articles::ArticleRef;
use common::auth::AuthDb;
use common::profiles::ProfileClient;
use common::throttle::BehindProxy;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::config::AppConfig;
use crate::routes::internal::{FollowEvent, Kind};

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    /// The user database as `session_verifier`; only `verify_session()` works on it.
    pub auth_db: PgPool,
    pub behind_proxy: bool,
    pub profiles: ProfileClient,
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

/// One article as stored here; the author is resolved from profile-api by the handler.
pub struct ArticleRow {
    pub id: Uuid,
    pub creator_id: Uuid,
    pub title: String,
    pub subtitle: Option<String>,
    pub content: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub tags: Vec<String>,
    pub favorites: i64,
    pub favorited: bool,
}

impl AppState {
    pub async fn new(config: &AppConfig) -> sqlx::Result<Self> {
        let db = common::config::pool(&config.db).await?;
        let auth_db = common::config::pool(&config.auth_db).await?;
        Ok(Self::build(
            db,
            auth_db,
            config.server.behind_proxy,
            ProfileClient::new(&config.profile_api.url),
        ))
    }

    pub fn build(db: PgPool, auth_db: PgPool, behind_proxy: bool, profiles: ProfileClient) -> Self {
        Self {
            db,
            auth_db,
            behind_proxy,
            profiles,
        }
    }

    pub async fn migrate(&self) -> Result<(), sqlx::migrate::MigrateError> {
        sqlx::migrate!("./migrations").run(&self.db).await
    }

    /// Newest-first page of articles with `id < before`. A viewer who follows anyone sees only
    /// followees' articles; anonymous viewers and users following nobody see everything.
    pub async fn feed(
        &self,
        viewer: Option<Uuid>,
        before: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<ArticleRow>> {
        // Followee branch: one range scan on feed_entries_pkey (filled by triggers from the
        // replicated follow graph). Global branch: its NOT EXISTS guard is uncorrelated, so
        // Postgres evaluates it once and skips the branch (One-Time Filter) for anyone who
        // follows someone.
        sqlx::query_as!(
            ArticleRow,
            r#"
                WITH page AS (
                    (SELECT article_id AS id FROM feed_entries
                     WHERE user_id = $1 AND article_id < $2
                     ORDER BY article_id DESC LIMIT $3)
                    UNION ALL
                    (SELECT id FROM articles
                     WHERE id < $2 AND NOT EXISTS (SELECT 1 FROM follows WHERE follower_id = $1)
                     ORDER BY id DESC LIMIT $3)
                )
                SELECT a.id, a.creator_id, a.title, a.subtitle, NULL::text AS content, a.created_at, a.updated_at,
                       (SELECT coalesce(array_agg(tag ORDER BY tag), '{}') FROM article_tags WHERE article_id = a.id) AS "tags!: Vec<String>",
                       coalesce(s.favorites_count, 0) AS "favorites!",
                       EXISTS (SELECT 1 FROM article_favorites WHERE article_id = a.id AND user_id = $1) AS "favorited!"
                FROM page
                JOIN articles a ON a.id = page.id
                LEFT JOIN article_stats s ON s.article_id = a.id
                ORDER BY a.id DESC
                LIMIT $3
            "#,
            viewer,
            before,
            limit
        )
        .fetch_all(&self.db)
        .await
    }

    pub async fn get_article(
        &self,
        id: Uuid,
        viewer: Option<Uuid>,
    ) -> sqlx::Result<Option<ArticleRow>> {
        sqlx::query_as!(
            ArticleRow,
            r#"
                SELECT a.id, a.creator_id, a.title, a.subtitle, a.content AS "content?", a.created_at, a.updated_at,
                       (SELECT coalesce(array_agg(tag ORDER BY tag), '{}') FROM article_tags WHERE article_id = a.id) AS "tags!: Vec<String>",
                       coalesce(s.favorites_count, 0) AS "favorites!",
                       EXISTS (SELECT 1 FROM article_favorites WHERE article_id = a.id AND user_id = $1) AS "favorited!"
                FROM articles a
                LEFT JOIN article_stats s ON s.article_id = a.id
                WHERE a.id = $2
            "#,
            viewer,
            id
        )
        .fetch_optional(&self.db)
        .await
    }

    /// Inserts the article and its tags in one statement; returns the new id.
    pub async fn create_article(
        &self,
        creator_id: Uuid,
        title: &str,
        subtitle: Option<&str>,
        content: &str,
        tags: &[String],
    ) -> sqlx::Result<Uuid> {
        // Sibling CTEs: the article_tags -> tags foreign key is checked after the whole
        // statement, so it sees the tags inserted alongside.
        sqlx::query_scalar!(
            r#"
                WITH a AS (
                    INSERT INTO articles (creator_id, title, subtitle, content)
                    VALUES ($1, $2, $3, $4) RETURNING id
                ), t AS (
                    INSERT INTO tags (tag) SELECT unnest($5::text[]) ON CONFLICT DO NOTHING
                ), at AS (
                    INSERT INTO article_tags (article_id, tag) SELECT a.id, unnest($5::text[]) FROM a
                )
                SELECT id AS "id!" FROM a
            "#,
            creator_id,
            title,
            subtitle,
            content,
            tags
        )
        .fetch_one(&self.db)
        .await
    }

    /// Full replacement of an article the caller owns, tags included. `false` when no
    /// article matches both `id` and `creator_id`.
    pub async fn update_article(
        &self,
        id: Uuid,
        creator_id: Uuid,
        title: &str,
        subtitle: Option<&str>,
        content: &str,
        tags: &[String],
    ) -> sqlx::Result<bool> {
        // Only tags leaving the set are deleted and the insert ignores ones that stay:
        // deleting and re-inserting the same key in sibling CTEs is a duplicate-key error.
        let updated = sqlx::query_scalar!(
            r#"
                WITH a AS (
                    UPDATE articles SET title = $3, subtitle = $4, content = $5
                    WHERE id = $1 AND creator_id = $2 RETURNING id
                ), d AS (
                    DELETE FROM article_tags
                    WHERE article_id = (SELECT id FROM a) AND tag <> ALL($6::text[])
                ), t AS (
                    INSERT INTO tags (tag) SELECT unnest($6::text[]) ON CONFLICT DO NOTHING
                ), at AS (
                    INSERT INTO article_tags (article_id, tag) SELECT a.id, unnest($6::text[]) FROM a
                    ON CONFLICT DO NOTHING
                )
                SELECT id AS "id!" FROM a
            "#,
            id,
            creator_id,
            title,
            subtitle,
            content,
            tags
        )
        .fetch_optional(&self.db)
        .await?;
        Ok(updated.is_some())
    }

    /// Idempotent; returns the article's favorite count after the insert. An unknown
    /// `article_id` surfaces as a foreign-key violation.
    pub async fn favorite(&self, article_id: Uuid, user_id: Uuid) -> sqlx::Result<i64> {
        // favorites_count is bumped by a trigger, but this statement's snapshot cannot see
        // that, so the inserted row (0 or 1) is added back from RETURNING.
        sqlx::query_scalar!(
            r#"
                WITH ins AS (
                    INSERT INTO article_favorites (article_id, user_id) VALUES ($1, $2)
                    ON CONFLICT DO NOTHING RETURNING 1
                )
                SELECT coalesce(s.favorites_count, 0) + (SELECT count(*) FROM ins) AS "favorites!"
                FROM articles a
                LEFT JOIN article_stats s ON s.article_id = a.id
                WHERE a.id = $1
            "#,
            article_id,
            user_id
        )
        .fetch_one(&self.db)
        .await
    }

    /// Idempotent; returns the article's favorite count after the delete, `None` if the
    /// article does not exist.
    pub async fn unfavorite(&self, article_id: Uuid, user_id: Uuid) -> sqlx::Result<Option<i64>> {
        sqlx::query_scalar!(
            r#"
                WITH del AS (
                    DELETE FROM article_favorites WHERE article_id = $1 AND user_id = $2 RETURNING 1
                )
                SELECT coalesce(s.favorites_count, 0) - (SELECT count(*) FROM del) AS "favorites!"
                FROM articles a
                LEFT JOIN article_stats s ON s.article_id = a.id
                WHERE a.id = $1
            "#,
            article_id,
            user_id
        )
        .fetch_optional(&self.db)
        .await
    }

    /// Existence and ownership for comment-api (`GET /internal/articles/{id}`).
    pub async fn article_ref(&self, id: Uuid) -> sqlx::Result<Option<ArticleRef>> {
        sqlx::query_as!(
            ArticleRef,
            "SELECT id, creator_id FROM articles WHERE id = $1",
            id
        )
        .fetch_optional(&self.db)
        .await
    }

    /// Applies profile-api's follow events to the local replica, in order, in one transaction.
    /// Idempotent: replays of either kind are no-ops, so at-least-once delivery is safe.
    pub async fn apply_follow_events(&self, events: &[FollowEvent]) -> sqlx::Result<()> {
        let mut tx = self.db.begin().await?;
        for e in events {
            match e.kind {
                Kind::Follow => {
                    sqlx::query!(
                        "INSERT INTO follows (follower_id, followee_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                        e.follower_id,
                        e.followee_id
                    )
                    .execute(&mut *tx)
                    .await?;
                }
                Kind::Unfollow => {
                    sqlx::query!(
                        "DELETE FROM follows WHERE follower_id = $1 AND followee_id = $2",
                        e.follower_id,
                        e.followee_id
                    )
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }
        tx.commit().await
    }
}
