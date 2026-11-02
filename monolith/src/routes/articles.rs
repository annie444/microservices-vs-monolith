use std::collections::BTreeSet;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::AppError;
use crate::state::AppState;

pub(crate) const DEFAULT_SIZE: u16 = 20;
pub(crate) const MAX_SIZE: u16 = 100;
const NOT_FOUND: AppError = AppError::NotFound("article not found");

#[derive(Debug, Serialize)]
pub struct Author {
    pub id: Uuid,
    pub name: String,
    pub pronouns: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FeedArticle {
    pub id: Uuid,
    pub creator: Author,
    pub title: String,
    pub subtitle: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub tags: Vec<String>,
    pub favorites: i64,
    pub favorited: bool,
}

#[derive(Debug, Serialize)]
pub struct Article {
    pub id: Uuid,
    pub creator: Author,
    pub title: String,
    pub subtitle: Option<String>,
    pub content: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub tags: Vec<String>,
    pub favorites: i64,
    pub favorited: bool,
}

#[derive(Debug, Serialize)]
pub struct FeedPagination {
    /// Id of the last article on this page (nil UUID when the page is empty). Paginate on `has_more`.
    pub next_cursor: Uuid,
    pub has_more: bool,
    pub size: u16,
}

#[derive(Debug, Serialize)]
pub struct Feed {
    pub articles: Vec<FeedArticle>,
    pub pagination: FeedPagination,
}

#[derive(Debug, Deserialize)]
pub struct FeedQuery {
    pub cursor: Option<Uuid>,
    pub size: Option<u16>,
}

/// Body of create and update. Update is a full replacement: omitted `subtitle` becomes null
/// and `tags` replaces the whole set.
#[derive(Debug, Deserialize)]
pub struct ArticleInput {
    pub title: String,
    pub subtitle: Option<String>,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Favorites {
    pub favorites: i64,
}

fn validate(input: &mut ArticleInput) -> Result<(), AppError> {
    input.title = input.title.trim().to_owned();
    if input.title.is_empty() || input.title.chars().count() > 200 {
        return Err(AppError::Unprocessable("title must be 1-200 characters"));
    }
    input.subtitle = input
        .subtitle
        .take()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    if input
        .subtitle
        .as_ref()
        .is_some_and(|s| s.chars().count() > 300)
    {
        return Err(AppError::Unprocessable(
            "subtitle must be at most 300 characters",
        ));
    }
    if input.content.trim().is_empty() || input.content.chars().count() > 50_000 {
        return Err(AppError::Unprocessable(
            "content must be 1-50000 characters",
        ));
    }
    // Sorted + deduplicated so the stored set matches get_article's ORDER BY tag.
    let tags: BTreeSet<String> = input.tags.iter().map(|t| t.trim().to_lowercase()).collect();
    if tags.len() > 10 {
        return Err(AppError::Unprocessable("at most 10 tags"));
    }
    if tags.iter().any(|t| t.is_empty() || t.chars().count() > 30) {
        return Err(AppError::Unprocessable("tags must be 1-30 characters"));
    }
    input.tags = tags.into_iter().collect();
    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn feed(
    State(state): State<AppState>,
    Query(query): Query<FeedQuery>,
    viewer: Option<AuthUser>,
) -> Result<Json<Feed>, AppError> {
    let viewer = viewer.map(|AuthUser(id)| id);
    let size = query.size.unwrap_or(DEFAULT_SIZE).clamp(1, MAX_SIZE);
    // Ids are uuidv7, so "newest first" is `id DESC` and the cursor is just the last id.
    let before = query.cursor.unwrap_or(Uuid::max());
    let mut articles = state.feed(viewer, before, i64::from(size) + 1).await?;
    let has_more = articles.len() > usize::from(size);
    articles.truncate(usize::from(size));
    let next_cursor = articles.last().map_or(Uuid::nil(), |a| a.id);
    Ok(Json(Feed {
        articles,
        pagination: FeedPagination {
            next_cursor,
            has_more,
            size,
        },
    }))
}

#[tracing::instrument(skip_all)]
pub async fn get_article(
    State(state): State<AppState>,
    viewer: Option<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<Article>, AppError> {
    let viewer = viewer.map(|AuthUser(id)| id);
    state
        .get_article(id, viewer)
        .await?
        .map(Json)
        .ok_or(NOT_FOUND)
}

#[tracing::instrument(skip_all)]
pub async fn create(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Json(mut input): Json<ArticleInput>,
) -> Result<(StatusCode, Json<Article>), AppError> {
    validate(&mut input)?;
    let id = state
        .create_article(
            me,
            &input.title,
            input.subtitle.as_deref(),
            &input.content,
            &input.tags,
        )
        .await?;
    let article = state.get_article(id, Some(me)).await?.ok_or(NOT_FOUND)?;
    Ok((StatusCode::CREATED, Json(article)))
}

/// Full replacement. An article that is unknown or not the caller's is 404 either way.
#[tracing::instrument(skip_all)]
pub async fn update(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(id): Path<Uuid>,
    Json(mut input): Json<ArticleInput>,
) -> Result<Json<Article>, AppError> {
    validate(&mut input)?;
    let updated = state
        .update_article(
            id,
            me,
            &input.title,
            input.subtitle.as_deref(),
            &input.content,
            &input.tags,
        )
        .await?;
    if !updated {
        return Err(NOT_FOUND);
    }
    state
        .get_article(id, Some(me))
        .await?
        .map(Json)
        .ok_or(NOT_FOUND)
}

/// Idempotent; returns the article's updated favorite count.
#[tracing::instrument(skip_all)]
pub async fn favorite(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Favorites>, AppError> {
    match state.favorite(id, me).await {
        Ok(favorites) => Ok(Json(Favorites { favorites })),
        Err(sqlx::Error::Database(e)) if e.is_foreign_key_violation() => Err(NOT_FOUND),
        Err(e) => Err(e.into()),
    }
}

/// Idempotent; returns the article's updated favorite count.
#[tracing::instrument(skip_all)]
pub async fn unfavorite(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Favorites>, AppError> {
    state
        .unfavorite(id, me)
        .await?
        .map(|favorites| Json(Favorites { favorites }))
        .ok_or(NOT_FOUND)
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;
    use crate::config::ThrottleConfig;

    fn input(
        title: &str,
        subtitle: Option<&str>,
        content: &str,
        tags: &[&str],
    ) -> Json<ArticleInput> {
        Json(ArticleInput {
            title: title.into(),
            subtitle: subtitle.map(Into::into),
            content: content.into(),
            tags: tags.iter().map(|t| (*t).into()).collect(),
        })
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn create_update(db: PgPool) {
        let state = state(db);
        let st = State(state.clone());
        let a = user(&state, "a@b.c", "Ada").await;
        let b = user(&state, "b@b.c", "Bob").await;

        let (status, Json(art)) = create(
            st.clone(),
            AuthUser(a),
            input("Hi", Some("  "), "body", &[" Rust ", "axum", "rust"]),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            (
                art.creator.name.as_str(),
                art.subtitle.as_deref(),
                art.favorites,
                art.favorited
            ),
            ("Ada", None, 0, false)
        );
        assert_eq!(art.tags, ["axum", "rust"]);
        let id = art.id;
        assert_eq!(
            ids(&feed(st.clone(), q(None, None), None).await.unwrap().0),
            [id]
        );

        let Json(art) = update(
            st.clone(),
            AuthUser(a),
            Path(id),
            input("Hi2", Some("sub"), "body2", &["tokio"]),
        )
        .await
        .unwrap();
        assert_eq!(
            (
                art.title.as_str(),
                art.subtitle.as_deref(),
                art.content.as_str()
            ),
            ("Hi2", Some("sub"), "body2")
        );
        assert_eq!(art.tags, ["tokio"], "tags replaced, not merged");
        assert!(art.updated_at >= art.created_at);
        let Json(art) = update(
            st.clone(),
            AuthUser(a),
            Path(id),
            input("Hi3", None, "b", &[]),
        )
        .await
        .unwrap();
        assert!(art.tags.is_empty());

        assert!(matches!(
            update(
                st.clone(),
                AuthUser(b),
                Path(id),
                input("x", None, "y", &[])
            )
            .await,
            Err(AppError::NotFound(_)),
        ));
        assert!(matches!(
            update(
                st.clone(),
                AuthUser(a),
                Path(Uuid::nil()),
                input("x", None, "y", &[])
            )
            .await,
            Err(AppError::NotFound(_)),
        ));

        let eleven: Vec<String> = (0..11).map(|i| format!("t{i}")).collect();
        let eleven: Vec<&str> = eleven.iter().map(String::as_str).collect();
        let long = "x".repeat(31);
        for bad in [
            input("  ", None, "y", &[]),
            input("x", None, "  ", &[]),
            input("x", Some(&"s".repeat(301)), "y", &[]),
            input("x", None, "y", &eleven),
            input("x", None, "y", &[long.as_str()]),
            input("x", None, "y", &["  "]),
        ] {
            assert!(matches!(
                create(st.clone(), AuthUser(a), bad).await,
                Err(AppError::Unprocessable(_))
            ));
        }
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn favorite_flow(db: PgPool) {
        let state = state(db);
        let st = State(state.clone());
        let a = user(&state, "a@b.c", "Ada").await;
        let b = user(&state, "b@b.c", "Bob").await;
        let id = article(&state, a, "t").await;
        let count = |r: Result<Json<Favorites>, AppError>| r.unwrap().0.favorites;
        // xmin changes whenever a row version is written; the article row must stay put.
        let xmin = |db: &PgPool| {
            let db = db.clone();
            async move {
                sqlx::query_scalar!(
                    r#"SELECT xmin::text AS "xmin!" FROM articles WHERE id = $1"#,
                    id
                )
                .fetch_one(&db)
                .await
                .unwrap()
            }
        };
        let before = xmin(&state.db).await;

        assert_eq!(count(favorite(st.clone(), AuthUser(a), Path(id)).await), 1);
        assert_eq!(
            count(favorite(st.clone(), AuthUser(a), Path(id)).await),
            1,
            "idempotent"
        );
        assert_eq!(count(favorite(st.clone(), AuthUser(b), Path(id)).await), 2);
        let art = get_article(st.clone(), Some(AuthUser(a)), Path(id))
            .await
            .unwrap()
            .0;
        assert_eq!((art.favorites, art.favorited), (2, true));
        assert_eq!(art.updated_at, art.created_at, "favoriting is not an edit");
        assert_eq!(
            xmin(&state.db).await,
            before,
            "favoriting must not rewrite the article row"
        );

        // The counter is kept by a trigger, so it also tracks cascade deletes.
        sqlx::query!("DELETE FROM users WHERE id = $1", b)
            .execute(&state.db)
            .await
            .unwrap();
        let art = get_article(st.clone(), None, Path(id)).await.unwrap().0;
        assert_eq!(art.favorites, 1, "cascade delete decremented the counter");
        assert_eq!(count(favorite(st.clone(), AuthUser(a), Path(id)).await), 1);

        assert_eq!(
            count(unfavorite(st.clone(), AuthUser(a), Path(id)).await),
            0
        );
        assert_eq!(
            count(unfavorite(st.clone(), AuthUser(a), Path(id)).await),
            0,
            "idempotent"
        );
        assert!(matches!(
            favorite(st.clone(), AuthUser(a), Path(Uuid::nil())).await,
            Err(AppError::NotFound(_))
        ));
        assert!(matches!(
            unfavorite(st, AuthUser(a), Path(Uuid::nil())).await,
            Err(AppError::NotFound(_))
        ));
    }

    fn state(db: PgPool) -> AppState {
        AppState::build(db, false, &ThrottleConfig::default())
    }

    async fn user(state: &AppState, email: &str, name: &str) -> Uuid {
        state.add_user(email, "unused-hash", name).await.unwrap();
        sqlx::query_scalar!("SELECT id FROM users WHERE email = $1", email)
            .fetch_one(&state.db)
            .await
            .unwrap()
    }

    async fn article(state: &AppState, creator: Uuid, title: &str) -> Uuid {
        sqlx::query_scalar!(
            "INSERT INTO articles (creator_id, title, content) VALUES ($1, $2, 'body') RETURNING id",
            creator,
            title
        )
        .fetch_one(&state.db)
        .await
        .unwrap()
    }

    fn q(cursor: Option<Uuid>, size: Option<u16>) -> Query<FeedQuery> {
        Query(FeedQuery { cursor, size })
    }

    fn ids(feed: &Feed) -> Vec<Uuid> {
        feed.articles.iter().map(|a| a.id).collect()
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn feed_pagination(db: PgPool) {
        let state = state(db);
        let st = State(state.clone());
        let a = user(&state, "a@b.c", "Ada").await;
        let mut posted = Vec::new();
        for i in 0..5 {
            posted.push(article(&state, a, &format!("t{i}")).await);
        }
        posted.reverse(); // newest first

        let page = feed(st.clone(), q(None, Some(2)), None).await.unwrap().0;
        assert_eq!(ids(&page), posted[..2]);
        assert_eq!(
            (
                page.pagination.has_more,
                page.pagination.next_cursor,
                page.pagination.size
            ),
            (true, posted[1], 2)
        );
        assert_eq!(page.articles[0].creator.name, "Ada");

        let page = feed(
            st.clone(),
            q(Some(page.pagination.next_cursor), Some(2)),
            None,
        )
        .await
        .unwrap()
        .0;
        assert_eq!(ids(&page), posted[2..4]);
        assert!(page.pagination.has_more);

        let page = feed(
            st.clone(),
            q(Some(page.pagination.next_cursor), Some(2)),
            None,
        )
        .await
        .unwrap()
        .0;
        assert_eq!(ids(&page), posted[4..]);
        assert!(!page.pagination.has_more);

        let page = feed(
            st.clone(),
            q(Some(page.pagination.next_cursor), Some(2)),
            None,
        )
        .await
        .unwrap()
        .0;
        assert!(page.articles.is_empty());
        assert_eq!(
            (page.pagination.has_more, page.pagination.next_cursor),
            (false, Uuid::nil())
        );

        assert_eq!(
            feed(st.clone(), q(None, Some(0)), None)
                .await
                .unwrap()
                .0
                .pagination
                .size,
            1
        );
        let page = feed(st.clone(), q(None, Some(500)), None).await.unwrap().0;
        assert_eq!((page.pagination.size, page.articles.len()), (MAX_SIZE, 5));
        assert_eq!(
            feed(st, q(None, None), None)
                .await
                .unwrap()
                .0
                .pagination
                .size,
            DEFAULT_SIZE
        );
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn feed_following(db: PgPool) {
        let state = state(db);
        let st = State(state.clone());
        let a = user(&state, "a@b.c", "Ada").await;
        let b = user(&state, "b@b.c", "Bob").await;
        let c = user(&state, "c@b.c", "Cy").await;
        let a1 = article(&state, a, "a1").await;
        let b1 = article(&state, b, "b1").await;
        let a2 = article(&state, a, "a2").await;
        state.follow(c, a).await.unwrap();

        let as_viewer = |v: Option<Uuid>| {
            let st = st.clone();
            async move { ids(&feed(st, q(None, None), v.map(AuthUser)).await.unwrap().0) }
        };
        assert_eq!(as_viewer(Some(c)).await, [a2, a1], "only followees");
        assert_eq!(
            as_viewer(Some(b)).await,
            [a2, b1, a1],
            "follows nobody: global"
        );
        assert_eq!(
            as_viewer(Some(a)).await,
            [a2, b1, a1],
            "own posts, follows nobody: global"
        );
        assert_eq!(as_viewer(None).await, [a2, b1, a1], "anonymous: global");
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn article_detail(db: PgPool) {
        let state = state(db);
        let st = State(state.clone());
        let a = user(&state, "a@b.c", "Ada").await;
        let b = user(&state, "b@b.c", "Bob").await;
        let id = article(&state, a, "hello").await;
        sqlx::query!(
            r#"
                WITH t AS (INSERT INTO tags (tag) VALUES ('rust'), ('axum') RETURNING tag)
                INSERT INTO article_tags (article_id, tag) SELECT $1, tag FROM t
            "#,
            id
        )
        .execute(&state.db)
        .await
        .unwrap();
        sqlx::query!(
            "INSERT INTO article_favorites (article_id, user_id) VALUES ($1, $2)",
            id,
            b
        )
        .execute(&state.db)
        .await
        .unwrap();

        let art = get_article(st.clone(), Some(AuthUser(b)), Path(id))
            .await
            .unwrap()
            .0;
        assert_eq!(
            (
                art.id,
                art.title.as_str(),
                art.content.as_str(),
                art.creator.name.as_str(),
                art.favorites,
                art.favorited
            ),
            (id, "hello", "body", "Ada", 1, true)
        );
        assert_eq!(art.tags, ["axum", "rust"]);

        let art = get_article(st.clone(), None, Path(id)).await.unwrap().0;
        assert!(!art.favorited);
        let page = feed(st.clone(), q(None, None), Some(AuthUser(b)))
            .await
            .unwrap()
            .0;
        assert_eq!(
            (page.articles[0].favorites, page.articles[0].favorited),
            (1, true)
        );

        assert!(matches!(
            get_article(st, None, Path(Uuid::nil())).await,
            Err(AppError::NotFound(_))
        ));
    }
}
