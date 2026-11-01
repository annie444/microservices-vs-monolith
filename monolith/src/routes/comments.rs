use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::AppError;
use crate::routes::articles::{Author, DEFAULT_SIZE, FeedPagination, FeedQuery, MAX_SIZE};
use crate::state::AppState;

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
pub struct Comments {
    pub comments: Vec<Comment>,
    pub pagination: FeedPagination,
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

/// Oldest first; `cursor` is the last comment id of the previous page.
#[tracing::instrument(skip_all)]
pub async fn list(
    State(state): State<AppState>,
    Path(article_id): Path<Uuid>,
    Query(query): Query<FeedQuery>,
) -> Result<Json<Comments>, AppError> {
    let size = query.size.unwrap_or(DEFAULT_SIZE).clamp(1, MAX_SIZE);
    let after = query.cursor.unwrap_or(Uuid::nil());
    let mut comments = state
        .list_comments(article_id, after, i64::from(size) + 1)
        .await?;
    // An empty page and an unknown article look the same; only then pay for the check.
    if comments.is_empty() && !state.article_exists(article_id).await? {
        return Err(ARTICLE_NOT_FOUND);
    }
    let has_more = comments.len() > usize::from(size);
    comments.truncate(usize::from(size));
    let next_cursor = comments.last().map_or(Uuid::nil(), |c| c.id);
    Ok(Json(Comments {
        comments,
        pagination: FeedPagination {
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
    match state.create_comment(article_id, me, &input.content).await {
        Ok(comment) => Ok((StatusCode::CREATED, Json(comment))),
        Err(sqlx::Error::Database(e)) if e.is_foreign_key_violation() => Err(ARTICLE_NOT_FOUND),
        Err(e) => Err(e.into()),
    }
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
    state
        .update_comment(article_id, comment_id, me, &input.content)
        .await?
        .map(Json)
        .ok_or(NOT_FOUND)
}

/// The comment's author or the article's creator may delete it; anything else is 404.
#[tracing::instrument(skip_all)]
pub async fn delete(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path((article_id, comment_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, AppError> {
    if state.delete_comment(article_id, comment_id, me).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(NOT_FOUND)
    }
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;
    use crate::config::ThrottleConfig;

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

    async fn article(state: &AppState, creator: Uuid) -> Uuid {
        state
            .create_article(creator, "t", None, "body", &[])
            .await
            .unwrap()
    }

    fn body(content: &str) -> Json<CommentInput> {
        Json(CommentInput {
            content: content.into(),
        })
    }

    fn q(cursor: Option<Uuid>, size: Option<u16>) -> Query<FeedQuery> {
        Query(FeedQuery { cursor, size })
    }

    fn contents(c: &Comments) -> Vec<&str> {
        c.comments.iter().map(|c| c.content.as_str()).collect()
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn comment_flow(db: PgPool) {
        let state = state(db);
        let st = State(state.clone());
        let a = user(&state, "a@b.c", "Ada").await;
        let b = user(&state, "b@b.c", "Bob").await;
        let c = user(&state, "c@b.c", "Cy").await;
        let art = article(&state, a).await;
        let empty = article(&state, a).await;

        let mut ids = Vec::new();
        for (who, text) in [(b, "1"), (b, "2"), (c, "3")] {
            let (status, Json(cm)) = create(st.clone(), AuthUser(who), Path(art), body(text))
                .await
                .unwrap();
            assert_eq!(status, StatusCode::CREATED);
            assert_eq!(
                (cm.article_id, cm.author.id, cm.content.as_str()),
                (art, who, text)
            );
            ids.push(cm.id);
        }
        let Json(first) = create(st.clone(), AuthUser(b), Path(art), body(" padded "))
            .await
            .map(|(_, j)| j)
            .unwrap();
        assert_eq!(
            (first.author.name.as_str(), first.content.as_str()),
            ("Bob", "padded")
        );
        ids.push(first.id);

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
        assert!(page.comments.is_empty(), "existing article, no comments");
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

    #[sqlx::test(migrations = "../migrations")]
    async fn comment_permissions(db: PgPool) {
        let state = state(db);
        let st = State(state.clone());
        let a = user(&state, "a@b.c", "Ada").await;
        let b = user(&state, "b@b.c", "Bob").await;
        let c = user(&state, "c@b.c", "Cy").await;
        let art = article(&state, a).await;
        let other = article(&state, a).await;
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
            "article owner may delete"
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
}
