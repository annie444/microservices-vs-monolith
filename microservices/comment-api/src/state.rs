use axum::extract::FromRef;
use common::articles::ArticleClient;
use common::auth::AuthDb;
use common::profiles::ProfileClient;
use common::throttle::BehindProxy;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::config::AppConfig;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    /// The user database as `session_verifier`; only `verify_session()` works on it.
    pub auth_db: PgPool,
    pub behind_proxy: bool,
    pub profiles: ProfileClient,
    pub articles: ArticleClient,
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

/// One comment as stored here; the author is resolved from profile-api by the handler.
pub struct CommentRow {
    pub id: Uuid,
    pub article_id: Uuid,
    pub user_id: Uuid,
    pub content: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
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
            ArticleClient::new(&config.article_api.url),
        ))
    }

    pub fn build(
        db: PgPool,
        auth_db: PgPool,
        behind_proxy: bool,
        profiles: ProfileClient,
        articles: ArticleClient,
    ) -> Self {
        Self {
            db,
            auth_db,
            behind_proxy,
            profiles,
            articles,
        }
    }

    pub async fn migrate(&self) -> Result<(), sqlx::migrate::MigrateError> {
        sqlx::migrate!("./migrations").run(&self.db).await
    }

    /// Comments on an article, oldest first, with `id > after`.
    pub async fn list_comments(
        &self,
        article_id: Uuid,
        after: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<CommentRow>> {
        sqlx::query_as!(
            CommentRow,
            r#"
                SELECT id, article_id, user_id, content, created_at, updated_at
                FROM comments
                WHERE article_id = $1 AND id > $2
                ORDER BY id
                LIMIT $3
            "#,
            article_id,
            after,
            limit
        )
        .fetch_all(&self.db)
        .await
    }

    pub async fn create_comment(
        &self,
        article_id: Uuid,
        user_id: Uuid,
        content: &str,
    ) -> sqlx::Result<CommentRow> {
        sqlx::query_as!(
            CommentRow,
            r#"
                INSERT INTO comments (article_id, user_id, content) VALUES ($1, $2, $3)
                RETURNING id, article_id, user_id, content, created_at, updated_at
            "#,
            article_id,
            user_id,
            content
        )
        .fetch_one(&self.db)
        .await
    }

    /// Only the author's own comment on that article matches; `None` otherwise.
    pub async fn update_comment(
        &self,
        article_id: Uuid,
        comment_id: Uuid,
        user_id: Uuid,
        content: &str,
    ) -> sqlx::Result<Option<CommentRow>> {
        sqlx::query_as!(
            CommentRow,
            r#"
                UPDATE comments SET content = $4
                WHERE id = $2 AND article_id = $1 AND user_id = $3
                RETURNING id, article_id, user_id, content, created_at, updated_at
            "#,
            article_id,
            comment_id,
            user_id,
            content
        )
        .fetch_optional(&self.db)
        .await
    }

    /// The author deleting their own comment. `true` if a row went.
    pub async fn delete_own_comment(
        &self,
        article_id: Uuid,
        comment_id: Uuid,
        user_id: Uuid,
    ) -> sqlx::Result<bool> {
        let result = sqlx::query!(
            "DELETE FROM comments WHERE id = $2 AND article_id = $1 AND user_id = $3",
            article_id,
            comment_id,
            user_id
        )
        .execute(&self.db)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Moderation by the article's creator (verified against article-api by the handler).
    pub async fn delete_any_comment(
        &self,
        article_id: Uuid,
        comment_id: Uuid,
    ) -> sqlx::Result<bool> {
        let result = sqlx::query!(
            "DELETE FROM comments WHERE id = $2 AND article_id = $1",
            article_id,
            comment_id
        )
        .execute(&self.db)
        .await?;
        Ok(result.rows_affected() > 0)
    }
}
