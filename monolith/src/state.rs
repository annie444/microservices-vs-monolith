use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::config::{AppConfig, ThrottleConfig};
use crate::routes::articles::{Article, Author, FeedArticle};
use crate::routes::comments::Comment;
use crate::routes::profile::Profile;
use crate::throttle::RateLimiter;

/// Flat shape of one article row; `query_as!` cannot build the nested `creator`.
struct ArticleRow {
    id: Uuid,
    title: String,
    subtitle: Option<String>,
    content: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    creator_id: Uuid,
    creator_name: String,
    creator_pronouns: Option<String>,
    tags: Vec<String>,
    favorites: i64,
    favorited: bool,
}

impl From<ArticleRow> for FeedArticle {
    fn from(r: ArticleRow) -> Self {
        Self {
            id: r.id,
            creator: Author {
                id: r.creator_id,
                name: r.creator_name,
                pronouns: r.creator_pronouns,
            },
            title: r.title,
            subtitle: r.subtitle,
            created_at: r.created_at,
            updated_at: r.updated_at,
            tags: r.tags,
            favorites: r.favorites,
            favorited: r.favorited,
        }
    }
}

impl From<ArticleRow> for Article {
    fn from(r: ArticleRow) -> Self {
        Self {
            id: r.id,
            creator: Author {
                id: r.creator_id,
                name: r.creator_name,
                pronouns: r.creator_pronouns,
            },
            title: r.title,
            subtitle: r.subtitle,
            content: r.content.unwrap_or_default(),
            created_at: r.created_at,
            updated_at: r.updated_at,
            tags: r.tags,
            favorites: r.favorites,
            favorited: r.favorited,
        }
    }
}

struct CommentRow {
    id: Uuid,
    article_id: Uuid,
    content: String,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    author_id: Uuid,
    author_name: String,
    author_pronouns: Option<String>,
}

impl From<CommentRow> for Comment {
    fn from(r: CommentRow) -> Self {
        Self {
            id: r.id,
            article_id: r.article_id,
            author: Author {
                id: r.author_id,
                name: r.author_name,
                pronouns: r.author_pronouns,
            },
            content: r.content,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

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
        sqlx::query_as!(
            Profile,
            r#"
                SELECT p.user_id AS id, p.name, p.pronouns, p.bio, p.followers_count,
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
                RETURNING user_id AS id, name, pronouns, bio, followers_count,
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

    /// Newest-first page of articles with `id < before`. A viewer who follows anyone sees only
    /// followees' articles; anonymous viewers and users following nobody see everything.
    pub async fn feed(
        &self,
        viewer: Option<Uuid>,
        before: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<FeedArticle>> {
        // Followee branch: the newest `limit` ids per followee straight off
        // idx_articles_creator_id (creator_id, id DESC), merged by a top-N sort. Cost is
        // follows x limit index entries, independent of how much history each author has.
        // Global branch: its NOT EXISTS guard is uncorrelated, so Postgres evaluates it once
        // and skips the branch (One-Time Filter) for anyone who follows someone.
        // ponytail: ceiling is a few thousand follows per viewer; past that, fan-out on write
        // (a feed_entries table filled at publish time) turns the read into one range scan.
        let rows = sqlx::query_as!(
            ArticleRow,
            r#"
                WITH page AS (
                    (SELECT a.id FROM follows f
                     JOIN LATERAL (
                         SELECT id FROM articles
                         WHERE creator_id = f.followee_id AND id < $2
                         ORDER BY id DESC LIMIT $3
                     ) a ON true
                     WHERE f.follower_id = $1
                     ORDER BY a.id DESC LIMIT $3)
                    UNION ALL
                    (SELECT id FROM articles
                     WHERE id < $2 AND NOT EXISTS (SELECT 1 FROM follows WHERE follower_id = $1)
                     ORDER BY id DESC LIMIT $3)
                )
                SELECT a.id, a.title, a.subtitle, NULL::text AS content, a.created_at, a.updated_at,
                       p.user_id AS creator_id, p.name AS creator_name, p.pronouns AS creator_pronouns,
                       (SELECT coalesce(array_agg(tag ORDER BY tag), '{}') FROM article_tags WHERE article_id = a.id) AS "tags!: Vec<String>",
                       a.favorites_count AS favorites,
                       EXISTS (SELECT 1 FROM article_favorites WHERE article_id = a.id AND user_id = $1) AS "favorited!"
                FROM page
                JOIN articles a ON a.id = page.id
                JOIN profiles p ON p.user_id = a.creator_id
                ORDER BY a.id DESC
                LIMIT $3
            "#,
            viewer,
            before,
            limit
        )
        .fetch_all(&self.db)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn get_article(
        &self,
        id: Uuid,
        viewer: Option<Uuid>,
    ) -> sqlx::Result<Option<Article>> {
        let row = sqlx::query_as!(
            ArticleRow,
            r#"
                SELECT a.id, a.title, a.subtitle, a.content AS "content?", a.created_at, a.updated_at,
                       p.user_id AS creator_id, p.name AS creator_name, p.pronouns AS creator_pronouns,
                       (SELECT coalesce(array_agg(tag ORDER BY tag), '{}') FROM article_tags WHERE article_id = a.id) AS "tags!: Vec<String>",
                       a.favorites_count AS favorites,
                       EXISTS (SELECT 1 FROM article_favorites WHERE article_id = a.id AND user_id = $1) AS "favorited!"
                FROM articles a
                JOIN profiles p ON p.user_id = a.creator_id
                WHERE a.id = $2
            "#,
            viewer,
            id
        )
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map(Into::into))
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
                SELECT favorites_count + (SELECT count(*) FROM ins) AS "favorites!"
                FROM articles WHERE id = $1
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
                SELECT favorites_count - (SELECT count(*) FROM del) AS "favorites!"
                FROM articles WHERE id = $1
            "#,
            article_id,
            user_id
        )
        .fetch_optional(&self.db)
        .await
    }

    /// Comments on an article, oldest first, with `id > after`.
    pub async fn list_comments(
        &self,
        article_id: Uuid,
        after: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<Comment>> {
        let rows = sqlx::query_as!(
            CommentRow,
            r#"
                SELECT c.id, c.article_id, c.content, c.created_at, c.updated_at,
                       p.user_id AS author_id, p.name AS author_name, p.pronouns AS author_pronouns
                FROM comments c
                JOIN profiles p ON p.user_id = c.user_id
                WHERE c.article_id = $1 AND c.id > $2
                ORDER BY c.id
                LIMIT $3
            "#,
            article_id,
            after,
            limit
        )
        .fetch_all(&self.db)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn article_exists(&self, id: Uuid) -> sqlx::Result<bool> {
        sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM articles WHERE id = $1) AS "exists!""#,
            id
        )
        .fetch_one(&self.db)
        .await
    }

    /// An unknown `article_id` surfaces as a foreign-key violation.
    pub async fn create_comment(
        &self,
        article_id: Uuid,
        user_id: Uuid,
        content: &str,
    ) -> sqlx::Result<Comment> {
        let row = sqlx::query_as!(
            CommentRow,
            r#"
                WITH c AS (
                    INSERT INTO comments (article_id, user_id, content) VALUES ($1, $2, $3)
                    RETURNING *
                )
                SELECT c.id AS "id!", c.article_id AS "article_id!", c.content AS "content!",
                       c.created_at AS "created_at!", c.updated_at AS "updated_at!",
                       p.user_id AS author_id, p.name AS author_name, p.pronouns AS author_pronouns
                FROM c
                JOIN profiles p ON p.user_id = c.user_id
            "#,
            article_id,
            user_id,
            content
        )
        .fetch_one(&self.db)
        .await?;
        Ok(row.into())
    }

    /// Only the author's own comment on that article matches; `None` otherwise.
    pub async fn update_comment(
        &self,
        article_id: Uuid,
        comment_id: Uuid,
        user_id: Uuid,
        content: &str,
    ) -> sqlx::Result<Option<Comment>> {
        let row = sqlx::query_as!(
            CommentRow,
            r#"
                WITH c AS (
                    UPDATE comments SET content = $4
                    WHERE id = $2 AND article_id = $1 AND user_id = $3
                    RETURNING *
                )
                SELECT c.id AS "id!", c.article_id AS "article_id!", c.content AS "content!",
                       c.created_at AS "created_at!", c.updated_at AS "updated_at!",
                       p.user_id AS author_id, p.name AS author_name, p.pronouns AS author_pronouns
                FROM c
                JOIN profiles p ON p.user_id = c.user_id
            "#,
            article_id,
            comment_id,
            user_id,
            content
        )
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map(Into::into))
    }

    /// The comment's author or the article's creator may delete. `true` if a row went.
    pub async fn delete_comment(
        &self,
        article_id: Uuid,
        comment_id: Uuid,
        user_id: Uuid,
    ) -> sqlx::Result<bool> {
        let result = sqlx::query!(
            r#"
                DELETE FROM comments c USING articles a
                WHERE c.id = $2 AND c.article_id = $1 AND a.id = c.article_id
                  AND (c.user_id = $3 OR a.creator_id = $3)
            "#,
            article_id,
            comment_id,
            user_id
        )
        .execute(&self.db)
        .await?;
        Ok(result.rows_affected() > 0)
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
                SELECT followers_count + (SELECT count(*) FROM ins) AS "followers_count!"
                FROM profiles WHERE user_id = $2
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
                SELECT followers_count - (SELECT count(*) FROM del) AS "followers_count!"
                FROM profiles WHERE user_id = $2
            "#,
            follower_id,
            followee_id
        )
        .fetch_optional(&self.db)
        .await
    }
}
