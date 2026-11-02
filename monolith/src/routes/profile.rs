use axum::extract::{Json, Path, State};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct Profile {
    pub id: Uuid,
    pub name: String,
    pub pronouns: Option<String>,
    pub bio: Option<String>,
    pub followers_count: i64,
    /// Whether the signed-in viewer follows this profile; false for anonymous viewers and yourself.
    pub following: bool,
}

/// Full replacement: omitted `pronouns` / `bio` become null.
#[derive(Deserialize)]
pub struct UpdateProfile {
    pub name: String,
    pub pronouns: Option<String>,
    pub bio: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FollowersCount {
    pub followers_count: i64,
}

const NOT_FOUND: AppError = AppError::NotFound("profile not found");

fn validate(update: &mut UpdateProfile) -> Result<(), AppError> {
    update.name = update.name.trim().to_owned();
    if update.name.is_empty() || update.name.chars().count() > 100 {
        return Err(AppError::Unprocessable("name must be 1-100 characters"));
    }
    if update
        .pronouns
        .as_ref()
        .is_some_and(|p| p.chars().count() > 50)
    {
        return Err(AppError::Unprocessable(
            "pronouns must be at most 50 characters",
        ));
    }
    if update
        .bio
        .as_ref()
        .is_some_and(|b| b.chars().count() > 2000)
    {
        return Err(AppError::Unprocessable(
            "bio must be at most 2000 characters",
        ));
    }
    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn get_profile(
    State(state): State<AppState>,
    viewer: Option<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<Profile>, AppError> {
    let viewer = viewer.map(|AuthUser(id)| id);
    state
        .get_profile(id, viewer)
        .await?
        .map(Json)
        .ok_or(NOT_FOUND)
}

#[tracing::instrument(skip_all)]
pub async fn me(
    State(state): State<AppState>,
    AuthUser(id): AuthUser,
) -> Result<Json<Profile>, AppError> {
    state
        .get_profile(id, Some(id))
        .await?
        .map(Json)
        .ok_or(NOT_FOUND)
}

#[tracing::instrument(skip_all)]
pub async fn update_me(
    State(state): State<AppState>,
    AuthUser(id): AuthUser,
    Json(mut update): Json<UpdateProfile>,
) -> Result<Json<Profile>, AppError> {
    validate(&mut update)?;
    state
        .update_profile(
            id,
            &update.name,
            update.pronouns.as_deref(),
            update.bio.as_deref(),
        )
        .await?
        .map(Json)
        .ok_or(NOT_FOUND)
}

/// Idempotent; returns the followee's updated follower count.
#[tracing::instrument(skip_all)]
pub async fn follow(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<FollowersCount>, AppError> {
    if id == me {
        return Err(AppError::Unprocessable("cannot follow yourself"));
    }
    match state.follow(me, id).await {
        Ok(followers_count) => Ok(Json(FollowersCount { followers_count })),
        Err(sqlx::Error::Database(e)) if e.is_foreign_key_violation() => Err(NOT_FOUND),
        Err(e) => Err(e.into()),
    }
}

/// Idempotent; returns the followee's updated follower count.
#[tracing::instrument(skip_all)]
pub async fn unfollow(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<FollowersCount>, AppError> {
    state
        .unfollow(me, id)
        .await?
        .map(|followers_count| Json(FollowersCount { followers_count }))
        .ok_or(NOT_FOUND)
}

#[cfg(test)]
mod tests {
    use axum::extract::FromRequestParts;
    use axum::http::Request;
    use sha2::{Digest, Sha256};
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

    fn update(name: &str, pronouns: Option<&str>, bio: Option<&str>) -> Json<UpdateProfile> {
        Json(UpdateProfile {
            name: name.into(),
            pronouns: pronouns.map(Into::into),
            bio: bio.map(Into::into),
        })
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn profile_flow(db: PgPool) {
        let state = state(db);
        let st = State(state.clone());
        let a = user(&state, "a@b.c", "Ada").await;
        let b = user(&state, "b@b.c", "Bob").await;

        let p = get_profile(st.clone(), None, Path(b)).await.unwrap().0;
        assert_eq!(
            (p.id, p.name.as_str(), p.followers_count, p.following),
            (b, "Bob", 0, false)
        );
        assert!(matches!(
            get_profile(st.clone(), None, Path(Uuid::nil())).await,
            Err(AppError::NotFound(_))
        ));

        assert_eq!(me(st.clone(), AuthUser(a)).await.unwrap().0.id, a);

        let p = update_me(
            st.clone(),
            AuthUser(a),
            update("Ada L", Some("she/her"), Some("hi")),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(
            (p.name.as_str(), p.pronouns.as_deref(), p.bio.as_deref()),
            ("Ada L", Some("she/her"), Some("hi"))
        );
        let p = update_me(st.clone(), AuthUser(a), update("  Ada  ", None, None))
            .await
            .unwrap()
            .0;
        assert_eq!((p.name.as_str(), p.pronouns, p.bio), ("Ada", None, None));
        assert!(matches!(
            update_me(st.clone(), AuthUser(a), update("   ", None, None)).await,
            Err(AppError::Unprocessable(_))
        ));
        assert!(matches!(
            update_me(
                st.clone(),
                AuthUser(a),
                update("x", None, Some(&"b".repeat(2001)))
            )
            .await,
            Err(AppError::Unprocessable(_))
        ));

        let count = |r: Result<Json<FollowersCount>, AppError>| r.unwrap().0.followers_count;
        assert_eq!(count(follow(st.clone(), AuthUser(a), Path(b)).await), 1);
        assert_eq!(
            count(follow(st.clone(), AuthUser(a), Path(b)).await),
            1,
            "idempotent"
        );
        assert_eq!(count(follow(st.clone(), AuthUser(b), Path(a)).await), 1);
        assert!(matches!(
            follow(st.clone(), AuthUser(a), Path(a)).await,
            Err(AppError::Unprocessable(_))
        ));
        assert!(matches!(
            follow(st.clone(), AuthUser(a), Path(Uuid::nil())).await,
            Err(AppError::NotFound(_))
        ));

        assert_eq!(count(unfollow(st.clone(), AuthUser(b), Path(a)).await), 0);
        assert_eq!(
            count(unfollow(st.clone(), AuthUser(b), Path(a)).await),
            0,
            "idempotent"
        );
        assert_eq!(
            count(unfollow(st.clone(), AuthUser(b), Path(b)).await),
            1,
            "a still follows b"
        );
        assert!(matches!(
            unfollow(st.clone(), AuthUser(a), Path(Uuid::nil())).await,
            Err(AppError::NotFound(_))
        ));

        // The counter is kept by a trigger, so it also tracks cascade deletes.
        let c = user(&state, "c@b.c", "Cy").await;
        assert_eq!(count(follow(st.clone(), AuthUser(c), Path(b)).await), 2);
        sqlx::query!("DELETE FROM users WHERE id = $1", c)
            .execute(&state.db)
            .await
            .unwrap();
        let p = get_profile(st.clone(), None, Path(b)).await.unwrap().0;
        assert_eq!(
            p.followers_count, 1,
            "cascade delete decremented the counter"
        );

        assert!(
            get_profile(st.clone(), Some(AuthUser(a)), Path(b))
                .await
                .unwrap()
                .0
                .following
        );
        assert!(
            !get_profile(st.clone(), None, Path(b))
                .await
                .unwrap()
                .0
                .following
        );
        assert!(
            !get_profile(st, Some(AuthUser(b)), Path(b))
                .await
                .unwrap()
                .0
                .following
        );
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn optional_auth(db: PgPool) {
        let state = state(db);

        async fn viewer(state: &AppState, cookie: Option<&str>) -> Result<Option<Uuid>, AppError> {
            let mut request = Request::builder();
            if let Some(c) = cookie {
                request = request.header("cookie", c);
            }
            let (mut parts, _) = request.body(()).unwrap().into_parts();
            <Option<AuthUser>>::from_request_parts(&mut parts, state)
                .await
                .map(|v| v.map(|AuthUser(id)| id))
        }

        assert_eq!(viewer(&state, None).await.unwrap(), None);
        assert_eq!(
            viewer(&state, Some("__Host-session=garbage"))
                .await
                .unwrap(),
            None
        );

        let id = user(&state, "a@b.c", "Ada").await;
        let session_hash: [u8; 32] = Sha256::digest(b"tok").into();
        let refresh_hash: [u8; 32] = Sha256::digest(b"ref").into();
        state
            .add_session(id, &session_hash, &refresh_hash)
            .await
            .unwrap();
        assert_eq!(
            viewer(&state, Some("__Host-session=tok")).await.unwrap(),
            Some(id)
        );
    }
}
