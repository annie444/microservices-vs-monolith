use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use common::auth::AuthUser;
use common::error::AppError;
use common::profiles::Author;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::state::{AppState, CommentRow};

const DEFAULT_SIZE: u16 = 20;
const MAX_SIZE: u16 = 100;
const NOT_FOUND: AppError = AppError::NotFound("comment not found");
const ARTICLE_NOT_FOUND: AppError = AppError::NotFound("article not found");

#[derive(Debug, Serialize)]
pub struct Comment {
    pub id: Uuid,
    pub article_id: Uuid,
    pub author: Author,
    pub content: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct Pagination {
    /// Id of the last comment on this page (nil UUID when the page is empty). Paginate on `has_more`.
    pub next_cursor: Uuid,
    pub has_more: bool,
    pub size: u16,
}

#[derive(Debug, Serialize)]
pub struct Comments {
    pub comments: Vec<Comment>,
    pub pagination: Pagination,
}

#[derive(Debug, Deserialize)]
pub struct PageQuery {
    pub cursor: Option<Uuid>,
    pub size: Option<u16>,
}

#[derive(Debug, Deserialize)]
pub struct CommentInput {
    pub content: String,
}

fn validate(input: &mut CommentInput) -> Result<(), AppError> {
    input.content = input.content.trim().to_owned();
    if input.content.is_empty() || input.content.chars().count() > 2000 {
        return Err(AppError::Unprocessable("content must be 1-2000 characters"));
    }
    Ok(())
}

fn author(authors: &HashMap<Uuid, Author>, id: Uuid) -> Author {
    authors.get(&id).cloned().unwrap_or_else(|| {
        tracing::warn!(user_id = %id, "profile-api has no profile for this author");
        Author {
            id,
            name: String::new(),
            pronouns: None,
        }
    })
}

fn into_comment(r: CommentRow, author: Author) -> Comment {
    Comment {
        id: r.id,
        article_id: r.article_id,
        author,
        content: r.content,
        created_at: r.created_at,
        updated_at: r.updated_at,
    }
}

/// One comment plus its author from profile-api.
async fn with_author(state: &AppState, row: CommentRow) -> Result<Comment, AppError> {
    let authors = state.profiles.authors(&[row.user_id]).await?;
    let a = author(&authors, row.user_id);
    Ok(into_comment(row, a))
}

/// Oldest first; `cursor` is the last comment id of the previous page.
#[tracing::instrument(skip_all)]
pub async fn list(
    State(state): State<AppState>,
    Path(article_id): Path<Uuid>,
    Query(query): Query<PageQuery>,
) -> Result<Json<Comments>, AppError> {
    let size = query.size.unwrap_or(DEFAULT_SIZE).clamp(1, MAX_SIZE);
    let after = query.cursor.unwrap_or(Uuid::nil());
    let mut rows = state
        .list_comments(article_id, after, i64::from(size) + 1)
        .await?;
    // An empty page and an unknown article look the same; only then pay for the hop.
    if rows.is_empty() && state.articles.get(article_id).await?.is_none() {
        return Err(ARTICLE_NOT_FOUND);
    }
    let has_more = rows.len() > usize::from(size);
    rows.truncate(usize::from(size));
    let next_cursor = rows.last().map_or(Uuid::nil(), |c| c.id);
    let ids: Vec<Uuid> = rows.iter().map(|r| r.user_id).collect();
    let authors = state.profiles.authors(&ids).await?;
    let comments = rows
        .into_iter()
        .map(|r| {
            let a = author(&authors, r.user_id);
            into_comment(r, a)
        })
        .collect();
    Ok(Json(Comments {
        comments,
        pagination: Pagination {
            next_cursor,
            has_more,
            size,
        },
    }))
}

#[tracing::instrument(skip_all)]
pub async fn create(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(article_id): Path<Uuid>,
    Json(mut input): Json<CommentInput>,
) -> Result<(StatusCode, Json<Comment>), AppError> {
    validate(&mut input)?;
    state
        .articles
        .get(article_id)
        .await?
        .ok_or(ARTICLE_NOT_FOUND)?;
    let row = state.create_comment(article_id, me, &input.content).await?;
    Ok((StatusCode::CREATED, Json(with_author(&state, row).await?)))
}

/// Only the comment's author may edit it; anything else is 404.
#[tracing::instrument(skip_all)]
pub async fn update(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path((article_id, comment_id)): Path<(Uuid, Uuid)>,
    Json(mut input): Json<CommentInput>,
) -> Result<Json<Comment>, AppError> {
    validate(&mut input)?;
    let row = state
        .update_comment(article_id, comment_id, me, &input.content)
        .await?
        .ok_or(NOT_FOUND)?;
    Ok(Json(with_author(&state, row).await?))
}

/// The comment's author or the article's creator may delete it; anything else is 404. Only the
/// creator path needs article-api.
#[tracing::instrument(skip_all)]
pub async fn delete(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path((article_id, comment_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, AppError> {
    if state.delete_own_comment(article_id, comment_id, me).await? {
        return Ok(StatusCode::NO_CONTENT);
    }
    let owns_article = state
        .articles
        .get(article_id)
        .await?
        .is_some_and(|a| a.creator_id == me);
    if owns_article && state.delete_any_comment(article_id, comment_id).await? {
        return Ok(StatusCode::NO_CONTENT);
    }
    Err(NOT_FOUND)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::Router;
    use axum::routing::get;
    use common::articles::{ArticleClient, ArticleRef};
    use common::profiles::ProfileClient;
    use sqlx::PgPool;

    use super::*;

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
        serve(app).await
    }

    /// A stand-in article-api knowing exactly `known` (article id → creator id).
    async fn article_stub(known: HashMap<Uuid, Uuid>) -> String {
        let app = Router::new().route(
            "/internal/articles/{id}",
            get(move |Path(id): Path<Uuid>| {
                let known = known.clone();
                async move {
                    match known.get(&id) {
                        Some(&creator_id) => Ok(Json(ArticleRef { id, creator_id })),
                        None => Err(StatusCode::NOT_FOUND),
                    }
                }
            }),
        );
        serve(app).await
    }

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    async fn state(db: PgPool, known: &[(Uuid, Uuid)]) -> AppState {
        AppState::build(
            db.clone(),
            db,
            false,
            ProfileClient::new(&profile_stub().await),
            ArticleClient::new(&article_stub(known.iter().copied().collect()).await),
        )
    }

    /// There is no users or articles table here; any uuid is a user or article.
    async fn id(db: &PgPool) -> Uuid {
        sqlx::query_scalar!(r#"SELECT uuidv7() AS "id!""#)
            .fetch_one(db)
            .await
            .unwrap()
    }

    fn name(id: Uuid) -> String {
        format!("name-{id}")
    }

    fn body(content: &str) -> Json<CommentInput> {
        Json(CommentInput {
            content: content.into(),
        })
    }

    fn q(cursor: Option<Uuid>, size: Option<u16>) -> Query<PageQuery> {
        Query(PageQuery { cursor, size })
    }

    fn contents(c: &Comments) -> Vec<&str> {
        c.comments.iter().map(|c| c.content.as_str()).collect()
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn comment_flow(db: PgPool) {
        let (a, b, c) = (id(&db).await, id(&db).await, id(&db).await);
        let (art, empty) = (id(&db).await, id(&db).await);
        let state = state(db, &[(art, a), (empty, a)]).await;
        let st = State(state.clone());

        let mut ids = Vec::new();
        for (who, text) in [(b, "1"), (b, "2"), (c, "3")] {
            let (status, Json(cm)) = create(st.clone(), AuthUser(who), Path(art), body(text))
                .await
                .unwrap();
            assert_eq!(status, StatusCode::CREATED);
            assert_eq!(
                (
                    cm.article_id,
                    cm.author.id,
                    cm.author.name,
                    cm.content.as_str()
                ),
                (art, who, name(who), text)
            );
            ids.push(cm.id);
        }
        let Json(padded) = create(st.clone(), AuthUser(b), Path(art), body(" padded "))
            .await
            .map(|(_, j)| j)
            .unwrap();
        assert_eq!(padded.content, "padded");

        let page = list(st.clone(), Path(art), q(None, Some(2)))
            .await
            .unwrap()
            .0;
        assert_eq!(contents(&page), ["1", "2"]);
        assert_eq!(
            (
                page.pagination.has_more,
                page.pagination.next_cursor,
                page.pagination.size
            ),
            (true, ids[1], 2)
        );
        assert_eq!(page.comments[0].author.name, name(b));
        let page = list(
            st.clone(),
            Path(art),
            q(Some(page.pagination.next_cursor), Some(2)),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(contents(&page), ["3", "padded"]);
        assert!(!page.pagination.has_more);
        let page = list(
            st.clone(),
            Path(art),
            q(Some(page.pagination.next_cursor), Some(2)),
        )
        .await
        .unwrap()
        .0;
        assert!(page.comments.is_empty());
        assert_eq!(page.pagination.next_cursor, Uuid::nil());

        let page = list(st.clone(), Path(empty), q(None, None))
            .await
            .unwrap()
            .0;
        assert!(page.comments.is_empty(), "known article, no comments");
        assert!(matches!(
            list(st.clone(), Path(Uuid::nil()), q(None, None)).await,
            Err(AppError::NotFound(_))
        ));

        assert!(matches!(
            create(st.clone(), AuthUser(b), Path(Uuid::nil()), body("x")).await,
            Err(AppError::NotFound(_))
        ));
        assert!(matches!(
            create(st.clone(), AuthUser(b), Path(art), body("  ")).await,
            Err(AppError::Unprocessable(_))
        ));
        assert!(matches!(
            create(st, AuthUser(b), Path(art), body(&"x".repeat(2001))).await,
            Err(AppError::Unprocessable(_))
        ));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn comment_permissions(db: PgPool) {
        let (a, b, c) = (id(&db).await, id(&db).await, id(&db).await);
        let (art, other) = (id(&db).await, id(&db).await);
        let state = state(db, &[(art, a), (other, a)]).await;
        let st = State(state.clone());
        let bs = create(st.clone(), AuthUser(b), Path(art), body("b"))
            .await
            .unwrap()
            .1
            .0;
        let cs = create(st.clone(), AuthUser(c), Path(art), body("c"))
            .await
            .unwrap()
            .1
            .0;

        let Json(edited) = update(st.clone(), AuthUser(b), Path((art, bs.id)), body("b2"))
            .await
            .unwrap();
        assert_eq!(edited.content, "b2");
        assert!(edited.updated_at >= edited.created_at);
        for (who, path) in [(c, (art, bs.id)), (a, (art, bs.id)), (b, (other, bs.id))] {
            assert!(
                matches!(
                    update(st.clone(), AuthUser(who), Path(path), body("x")).await,
                    Err(AppError::NotFound(_))
                ),
                "only the author may edit, on the right article"
            );
        }

        assert!(matches!(
            delete(st.clone(), AuthUser(c), Path((art, bs.id))).await,
            Err(AppError::NotFound(_))
        ));
        assert_eq!(
            delete(st.clone(), AuthUser(a), Path((art, bs.id)))
                .await
                .unwrap(),
            StatusCode::NO_CONTENT,
            "article owner may delete (checked against article-api)"
        );
        assert_eq!(
            delete(st.clone(), AuthUser(c), Path((art, cs.id)))
                .await
                .unwrap(),
            StatusCode::NO_CONTENT,
            "author may delete"
        );
        assert!(matches!(
            delete(st, AuthUser(c), Path((art, cs.id))).await,
            Err(AppError::NotFound(_))
        ));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn article_api_down(db: PgPool) {
        let (a, b, art) = (id(&db).await, id(&db).await, id(&db).await);
        let healthy = state(db.clone(), &[(art, a)]).await;
        let seeded = create(State(healthy.clone()), AuthUser(b), Path(art), body("x"))
            .await
            .unwrap()
            .1
            .0;

        let state = AppState::build(
            db.clone(),
            db,
            false,
            ProfileClient::new(&profile_stub().await),
            ArticleClient::new("http://127.0.0.1:9"),
        );
        let st = State(state.clone());
        assert!(matches!(
            create(st.clone(), AuthUser(b), Path(art), body("y")).await,
            Err(AppError::Upstream)
        ));
        assert!(
            list(st.clone(), Path(art), q(None, None)).await.is_ok(),
            "a non-empty page needs no article lookup"
        );
        assert!(matches!(
            list(st.clone(), Path(Uuid::nil()), q(None, None)).await,
            Err(AppError::Upstream)
        ));
        assert!(
            matches!(
                delete(st.clone(), AuthUser(a), Path((art, seeded.id))).await,
                Err(AppError::Upstream)
            ),
            "owner moderation needs article-api"
        );
        assert_eq!(
            delete(st, AuthUser(b), Path((art, seeded.id)))
                .await
                .unwrap(),
            StatusCode::NO_CONTENT,
            "the author needs no hop"
        );
    }
}
