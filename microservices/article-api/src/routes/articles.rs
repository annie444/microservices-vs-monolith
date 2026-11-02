use std::collections::{BTreeSet, HashMap};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use common::auth::AuthUser;
use common::error::AppError;
use common::profiles::Author;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::state::{AppState, ArticleRow};

pub(crate) const DEFAULT_SIZE: u16 = 20;
pub(crate) const MAX_SIZE: u16 = 100;
const NOT_FOUND: AppError = AppError::NotFound("article not found");

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

/// Authors for every creator on the page, one profile-api call. A missing profile (should not
/// happen) becomes a nameless author rather than failing the whole page.
async fn authors(state: &AppState, rows: &[ArticleRow]) -> Result<HashMap<Uuid, Author>, AppError> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.creator_id).collect();
    state.profiles.authors(&ids).await
}

fn author(authors: &HashMap<Uuid, Author>, id: Uuid) -> Author {
    authors.get(&id).cloned().unwrap_or_else(|| {
        tracing::warn!(creator_id = %id, "profile-api has no profile for this author");
        Author {
            id,
            name: String::new(),
            pronouns: None,
        }
    })
}

fn into_feed(r: ArticleRow, creator: Author) -> FeedArticle {
    FeedArticle {
        id: r.id,
        creator,
        title: r.title,
        subtitle: r.subtitle,
        created_at: r.created_at,
        updated_at: r.updated_at,
        tags: r.tags,
        favorites: r.favorites,
        favorited: r.favorited,
    }
}

fn into_article(r: ArticleRow, creator: Author) -> Article {
    Article {
        id: r.id,
        creator,
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

/// `get_article` + author lookup, shared by the read and the two write handlers.
async fn load_article(
    state: &AppState,
    id: Uuid,
    viewer: Option<Uuid>,
) -> Result<Article, AppError> {
    let row = state.get_article(id, viewer).await?.ok_or(NOT_FOUND)?;
    let authors = state.profiles.authors(&[row.creator_id]).await?;
    let creator = author(&authors, row.creator_id);
    Ok(into_article(row, creator))
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
    let mut rows = state.feed(viewer, before, i64::from(size) + 1).await?;
    let has_more = rows.len() > usize::from(size);
    rows.truncate(usize::from(size));
    let next_cursor = rows.last().map_or(Uuid::nil(), |a| a.id);
    let authors = authors(&state, &rows).await?;
    let articles = rows
        .into_iter()
        .map(|r| {
            let creator = author(&authors, r.creator_id);
            into_feed(r, creator)
        })
        .collect();
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
    load_article(&state, id, viewer).await.map(Json)
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
    let article = load_article(&state, id, Some(me)).await?;
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
    load_article(&state, id, Some(me)).await.map(Json)
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
    use axum::Router;
    use axum::routing::get;
    use common::profiles::ProfileClient;
    use sqlx::PgPool;

    use super::*;
    use crate::routes::internal::{FollowEvent, Kind};

    #[derive(Deserialize)]
    struct IdsQuery {
        ids: String,
    }

    /// A stand-in profile-api: every requested id gets the name `name-<id>`.
    async fn profile_stub() -> String {
        let app = Router::new().route(
            "/internal/profiles",
            get(|Query(q): Query<IdsQuery>| async move {
                let authors: Vec<Author> = q
                    .ids
                    .split(',')
                    .map(|s| s.parse::<Uuid>().unwrap())
                    .map(|id| Author {
                        id,
                        name: format!("name-{id}"),
                        pronouns: None,
                    })
                    .collect();
                Json(authors)
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    async fn state(db: PgPool) -> AppState {
        AppState::build(
            db.clone(),
            db,
            false,
            ProfileClient::new(&profile_stub().await),
        )
    }

    fn name(id: Uuid) -> String {
        format!("name-{id}")
    }

    /// There is no users table here; any uuid is a user.
    async fn user(db: &PgPool) -> Uuid {
        sqlx::query_scalar!(r#"SELECT uuidv7() AS "id!""#)
            .fetch_one(db)
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

    fn event(id: i64, follower_id: Uuid, followee_id: Uuid, kind: Kind) -> FollowEvent {
        FollowEvent {
            id,
            follower_id,
            followee_id,
            kind,
        }
    }

    async fn follow(state: &AppState, follower: Uuid, followee: Uuid) {
        state
            .apply_follow_events(&[event(1, follower, followee, Kind::Follow)])
            .await
            .unwrap();
    }

    async fn unfollow(state: &AppState, follower: Uuid, followee: Uuid) {
        state
            .apply_follow_events(&[event(1, follower, followee, Kind::Unfollow)])
            .await
            .unwrap();
    }

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

    fn q(cursor: Option<Uuid>, size: Option<u16>) -> Query<FeedQuery> {
        Query(FeedQuery { cursor, size })
    }

    fn ids(feed: &Feed) -> Vec<Uuid> {
        feed.articles.iter().map(|a| a.id).collect()
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn create_update(db: PgPool) {
        let state = state(db).await;
        let st = State(state.clone());
        let a = user(&state.db).await;
        let b = user(&state.db).await;

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
                art.creator.id,
                art.creator.name.as_str(),
                art.subtitle.as_deref(),
                art.favorites,
                art.favorited
            ),
            (a, name(a).as_str(), None, 0, false)
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

    #[sqlx::test(migrations = "./migrations")]
    async fn favorite_flow(db: PgPool) {
        let state = state(db).await;
        let st = State(state.clone());
        let a = user(&state.db).await;
        let b = user(&state.db).await;
        let id = article(&state, a, "t").await;
        let count = |r: Result<Json<Favorites>, AppError>| r.unwrap().0.favorites;
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

        // The counter is kept by a trigger, so it also tracks rows removed behind its back.
        sqlx::query!("DELETE FROM article_favorites WHERE user_id = $1", b)
            .execute(&state.db)
            .await
            .unwrap();
        let art = get_article(st.clone(), None, Path(id)).await.unwrap().0;
        assert_eq!(art.favorites, 1);

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

    #[sqlx::test(migrations = "./migrations")]
    async fn feed_pagination(db: PgPool) {
        let state = state(db).await;
        let st = State(state.clone());
        let a = user(&state.db).await;
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
        assert_eq!(page.articles[0].creator.name, name(a));

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

    #[sqlx::test(migrations = "./migrations")]
    async fn feed_following(db: PgPool) {
        let state = state(db).await;
        let st = State(state.clone());
        let a = user(&state.db).await;
        let b = user(&state.db).await;
        let c = user(&state.db).await;
        let a1 = article(&state, a, "a1").await;
        let b1 = article(&state, b, "b1").await;
        let a2 = article(&state, a, "a2").await;
        follow(&state, c, a).await;

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
        assert_eq!(as_viewer(None).await, [a2, b1, a1], "anonymous: global");

        // Fan-out on publish, backfill on follow, cleanup on unfollow, all from the replica.
        let a3 = article(&state, a, "a3").await;
        assert_eq!(
            as_viewer(Some(c)).await,
            [a3, a2, a1],
            "published after follow"
        );
        follow(&state, c, b).await;
        assert_eq!(as_viewer(Some(c)).await, [a3, a2, b1, a1], "backfilled b");
        unfollow(&state, c, a).await;
        assert_eq!(as_viewer(Some(c)).await, [b1], "unfollow removed a's");
        unfollow(&state, c, b).await;
        assert_eq!(
            as_viewer(Some(c)).await,
            [a3, a2, b1, a1],
            "follows nobody again: global"
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn feed_backfill_limit(db: PgPool) {
        let state = state(db).await;
        let st = State(state.clone());
        let a = user(&state.db).await;
        let c = user(&state.db).await;
        sqlx::query!(
            "INSERT INTO articles (creator_id, title, content) SELECT $1, 't' || g, 'c' FROM generate_series(1, 120) g",
            a
        )
        .execute(&state.db)
        .await
        .unwrap();
        follow(&state, c, a).await;

        let page = feed(st.clone(), q(None, Some(100)), Some(AuthUser(c)))
            .await
            .unwrap()
            .0;
        assert_eq!(page.articles.len(), 100);
        let rest = feed(
            st.clone(),
            q(Some(page.pagination.next_cursor), Some(100)),
            Some(AuthUser(c)),
        )
        .await
        .unwrap()
        .0;
        assert!(
            rest.articles.is_empty(),
            "only the latest 100 were backfilled"
        );

        let newest = article(&state, a, "fresh").await;
        let page = feed(st, q(None, Some(1)), Some(AuthUser(c)))
            .await
            .unwrap()
            .0;
        assert_eq!(ids(&page), [newest], "fan-out on publish still applies");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn article_detail(db: PgPool) {
        let state = state(db).await;
        let st = State(state.clone());
        let a = user(&state.db).await;
        let b = user(&state.db).await;
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
            (id, "hello", "body", name(a).as_str(), 1, true)
        );
        assert_eq!(art.tags, ["axum", "rust"]);

        let art = get_article(st.clone(), None, Path(id)).await.unwrap().0;
        assert!(!art.favorited);
        assert!(matches!(
            get_article(st, None, Path(Uuid::nil())).await,
            Err(AppError::NotFound(_))
        ));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn follow_events_are_idempotent_and_ordered(db: PgPool) {
        let state = state(db).await;
        let a = user(&state.db).await;
        let b = user(&state.db).await;
        let c = user(&state.db).await;
        let b1 = article(&state, b, "b1").await;
        let c1 = article(&state, c, "c1").await;
        let pool = state.db.clone();
        let follows = || {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM follows"#)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };
        let feed_of = |u: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar!(
                    "SELECT article_id FROM feed_entries WHERE user_id = $1 ORDER BY article_id",
                    u
                )
                .fetch_all(&pool)
                .await
                .unwrap()
            }
        };

        let batch = [event(1, a, b, Kind::Follow)];
        state.apply_follow_events(&batch).await.unwrap();
        state.apply_follow_events(&batch).await.unwrap();
        assert_eq!(follows().await, 1, "redelivery is a no-op");
        assert_eq!(feed_of(a).await, [b1]);

        state
            .apply_follow_events(&[event(2, a, c, Kind::Follow), event(3, a, c, Kind::Unfollow)])
            .await
            .unwrap();
        assert_eq!(
            follows().await,
            1,
            "follow then unfollow in one batch nets out"
        );
        assert_eq!(
            feed_of(a).await,
            [b1],
            "c's article never stuck in the feed"
        );
        let _ = c1;

        state
            .apply_follow_events(&[event(4, a, b, Kind::Unfollow)])
            .await
            .unwrap();
        assert_eq!(follows().await, 0);
        assert!(feed_of(a).await.is_empty());
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn profile_api_down_is_502(db: PgPool) {
        let state = AppState::build(
            db.clone(),
            db,
            false,
            ProfileClient::new("http://127.0.0.1:9"),
        );
        let st = State(state.clone());
        assert!(
            feed(st.clone(), q(None, None), None).await.is_ok(),
            "an empty page needs no author lookup"
        );
        let a = user(&state.db).await;
        article(&state, a, "t").await;
        assert!(matches!(
            feed(st, q(None, None), None).await,
            Err(AppError::Upstream)
        ));
    }
}
