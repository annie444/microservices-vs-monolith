use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use common::articles::ArticleRef;
use common::error::AppError;
use serde::Deserialize;
use uuid::Uuid;

use crate::state::AppState;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Follow,
    Unfollow,
}

/// One row of profile-api's follow outbox.
#[derive(Debug, Deserialize)]
pub struct FollowEvent {
    pub id: i64,
    pub follower_id: Uuid,
    pub followee_id: Uuid,
    pub kind: Kind,
}

/// Consumer for profile-api's outbox: applies a batch in order and answers 204. Replays are
/// harmless, which is what lets the producer deliver at-least-once.
#[tracing::instrument(skip_all, fields(events = events.len()))]
pub async fn follow_events(
    State(state): State<AppState>,
    Json(events): Json<Vec<FollowEvent>>,
) -> Result<StatusCode, AppError> {
    state.apply_follow_events(&events).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Internal: existence and ownership for comment-api.
#[tracing::instrument(skip_all)]
pub async fn article_ref(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ArticleRef>, AppError> {
    state
        .article_ref(id)
        .await?
        .map(Json)
        .ok_or(AppError::NotFound("article not found"))
}

#[cfg(test)]
mod tests {
    use common::profiles::ProfileClient;
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test(migrations = "./migrations")]
    async fn article_ref_lookup(db: PgPool) {
        let state = AppState::build(
            db.clone(),
            db,
            false,
            ProfileClient::new("http://127.0.0.1:9"),
        );
        let st = State(state.clone());
        let creator = Uuid::from_u128(7);
        let id = sqlx::query_scalar!(
            "INSERT INTO articles (creator_id, title, content) VALUES ($1, 't', 'c') RETURNING id",
            creator
        )
        .fetch_one(&state.db)
        .await
        .unwrap();

        let Json(found) = article_ref(st.clone(), Path(id)).await.unwrap();
        assert_eq!((found.id, found.creator_id), (id, creator));
        assert!(matches!(
            article_ref(st, Path(Uuid::nil())).await,
            Err(AppError::NotFound(_))
        ));
    }
}
