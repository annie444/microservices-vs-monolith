//! Delivers `follow_events` to article-api: at-least-once, in id order, from exactly one
//! dispatcher cluster-wide (a transaction-scoped advisory lock). Rows are deleted only after
//! article-api answered 2xx, so a crash in between redelivers and the consumer must be idempotent.

use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::state::AppState;

const BATCH: i64 = 100;
const LOCK_KEY: i64 = 0x7072_6f66; // "prof"
const IDLE: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Event {
    pub id: i64,
    pub follower_id: Uuid,
    pub followee_id: Uuid,
    pub kind: String,
}

pub fn spawn(state: AppState) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = IDLE;
        loop {
            match deliver_once(&state).await {
                Ok(n) if n as i64 >= BATCH => backoff = IDLE,
                Ok(_) => {
                    backoff = IDLE;
                    tokio::select! {
                        _ = state.outbox.notified() => {}
                        _ = tokio::time::sleep(IDLE) => {}
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, retry_in = ?backoff, "follow outbox delivery failed");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    })
}

/// One batch. `Ok(0)` when there is nothing to send or another dispatcher holds the lock.
// Unchecked queries: the compile-time database (the monolith's) has no follow_events table.
pub async fn deliver_once(state: &AppState) -> anyhow::Result<usize> {
    let mut tx = state.db.begin().await?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(LOCK_KEY)
        .fetch_one(&mut *tx)
        .await?;
    if !locked {
        return Ok(0);
    }
    let events: Vec<Event> = sqlx::query_as(
        "SELECT id, follower_id, followee_id, kind FROM follow_events ORDER BY id LIMIT $1",
    )
    .bind(BATCH)
    .fetch_all(&mut *tx)
    .await?;
    if events.is_empty() {
        return Ok(0);
    }
    state
        .http
        .post(format!("{}/internal/follow-events", state.article_api_url))
        .json(&events)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .context("article-api did not accept follow events")?;
    let ids: Vec<i64> = events.iter().map(|e| e.id).collect();
    sqlx::query("DELETE FROM follow_events WHERE id = ANY($1)")
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(events.len())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::{Json, Router};
    use sqlx::PgPool;

    use super::*;

    /// A stand-in article-api that records every event it receives.
    async fn article_stub(status: StatusCode, seen: Arc<Mutex<Vec<Event>>>) -> String {
        let app = Router::new().route(
            "/internal/follow-events",
            post(move |Json(events): Json<Vec<Event>>| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().extend(events);
                    status
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    async fn profile(db: &PgPool, name: &str) -> Uuid {
        sqlx::query_scalar!(
            "INSERT INTO profiles (user_id, name) VALUES (uuidv7(), $1) RETURNING user_id",
            name
        )
        .fetch_one(db)
        .await
        .unwrap()
    }

    async fn pending(db: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM follow_events")
            .fetch_one(db)
            .await
            .unwrap()
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn delivers_in_order_then_deletes(db: PgPool) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let url = article_stub(StatusCode::NO_CONTENT, seen.clone()).await;
        let state = AppState::build(db.clone(), db.clone(), false, url);
        let a = profile(&db, "a").await;
        let b = profile(&db, "b").await;
        state.follow(a, b).await.unwrap();
        state.follow(b, a).await.unwrap();
        state.unfollow(b, a).await.unwrap();
        assert_eq!(pending(&db).await, 3);

        assert_eq!(deliver_once(&state).await.unwrap(), 3);
        {
            let got = seen.lock().unwrap();
            let summary: Vec<(&str, Uuid, Uuid)> = got
                .iter()
                .map(|e| (e.kind.as_str(), e.follower_id, e.followee_id))
                .collect();
            assert_eq!(
                summary,
                [("follow", a, b), ("follow", b, a), ("unfollow", b, a)]
            );
            assert!(got.windows(2).all(|w| w[0].id < w[1].id), "id order");
        }
        assert_eq!(pending(&db).await, 0);
        assert_eq!(deliver_once(&state).await.unwrap(), 0, "nothing left");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn keeps_events_when_delivery_fails_or_lock_is_held(db: PgPool) {
        let rejected = Arc::new(Mutex::new(Vec::new()));
        let failing = article_stub(StatusCode::INTERNAL_SERVER_ERROR, rejected.clone()).await;
        let state = AppState::build(db.clone(), db.clone(), false, failing);
        let a = profile(&db, "a").await;
        let b = profile(&db, "b").await;
        state.follow(a, b).await.unwrap();

        assert!(deliver_once(&state).await.is_err());
        assert_eq!(pending(&db).await, 1, "rows survive a failed delivery");

        let seen = Arc::new(Mutex::new(Vec::new()));
        let ok = article_stub(StatusCode::NO_CONTENT, seen.clone()).await;
        let state = AppState::build(db.clone(), db.clone(), false, ok);
        let mut holder = db.acquire().await.unwrap();
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(LOCK_KEY)
            .execute(&mut *holder)
            .await
            .unwrap();
        assert_eq!(
            deliver_once(&state).await.unwrap(),
            0,
            "another dispatcher holds the lock"
        );
        assert_eq!(pending(&db).await, 1);
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(LOCK_KEY)
            .execute(&mut *holder)
            .await
            .unwrap();
        drop(holder);

        assert_eq!(deliver_once(&state).await.unwrap(), 1);
        assert_eq!(pending(&db).await, 0);
        let redelivered = seen.lock().unwrap().len();
        let first_attempt = rejected.lock().unwrap().len();
        assert_eq!(
            (first_attempt, redelivered),
            (1, 1),
            "the event rejected by the first stub was redelivered to the second"
        );
    }
}
