use axum::extract::{Json, Path, Query, State};
use axum::http::StatusCode;
use common::auth::AuthUser;
use common::error::AppError;
use common::profiles::Author;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

/// Body of `POST /internal/profiles`, sent by user-api during registration.
#[derive(Deserialize)]
pub struct NewProfile {
    pub id: Uuid,
    pub name: String,
}

const NOT_FOUND: AppError = AppError::NotFound("profile not found");

fn validate_name(name: &str) -> Result<(), AppError> {
    if name.is_empty() || name.chars().count() > 100 {
        return Err(AppError::Unprocessable("name must be 1-100 characters"));
    }
    Ok(())
}

fn validate(update: &mut UpdateProfile) -> Result<(), AppError> {
    update.name = update.name.trim().to_owned();
    validate_name(&update.name)?;
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
        Ok(followers_count) => {
            state.outbox.notify_one();
            Ok(Json(FollowersCount { followers_count }))
        }
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
    let count = state.unfollow(me, id).await?.ok_or(NOT_FOUND)?;
    state.outbox.notify_one();
    Ok(Json(FollowersCount {
        followers_count: count,
    }))
}

/// `?ids=<uuid>,<uuid>,...`, at most 100.
#[derive(Deserialize)]
pub struct IdsQuery {
    pub ids: String,
}

/// Internal: author names for article-api / comment-api. Unknown ids are simply omitted.
#[tracing::instrument(skip_all)]
pub async fn authors_internal(
    State(state): State<AppState>,
    Query(query): Query<IdsQuery>,
) -> Result<Json<Vec<Author>>, AppError> {
    let ids = query
        .ids
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.trim().parse::<Uuid>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| AppError::Unprocessable("invalid id"))?;
    if ids.len() > 100 {
        return Err(AppError::Unprocessable("at most 100 ids"));
    }
    Ok(Json(state.authors(&ids).await?))
}

/// Internal: creates the profile for a freshly registered user. `201` when created, `200` when
/// it already existed, so user-api can retry safely.
#[tracing::instrument(skip_all)]
pub async fn create_internal(
    State(state): State<AppState>,
    Json(profile): Json<NewProfile>,
) -> Result<StatusCode, AppError> {
    let name = profile.name.trim();
    validate_name(name)?;
    Ok(if state.create_profile(profile.id, name).await? {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    })
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    fn state(db: PgPool) -> AppState {
        AppState::build(db.clone(), db, false, "http://127.0.0.1:9".into())
    }

    async fn user(state: &AppState, name: &str) -> Uuid {
        sqlx::query_scalar!(
            "INSERT INTO profiles (user_id, name) VALUES (uuidv7(), $1) RETURNING user_id",
            name
        )
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

    async fn events(db: &PgPool) -> Vec<(String, Uuid, Uuid)> {
        sqlx::query_as("SELECT kind, follower_id, followee_id FROM follow_events ORDER BY id")
            .fetch_all(db)
            .await
            .unwrap()
    }

    async fn xmin(db: &PgPool, id: Uuid) -> String {
        sqlx::query_scalar!(
            r#"SELECT xmin::text AS "xmin!" FROM profiles WHERE user_id = $1"#,
            id
        )
        .fetch_one(db)
        .await
        .unwrap()
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn profile_flow(db: PgPool) {
        let state = state(db);
        let st = State(state.clone());
        let a = user(&state, "Ada").await;
        let b = user(&state, "Bob").await;

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
        let before = xmin(&state.db, b).await;
        assert_eq!(count(follow(st.clone(), AuthUser(a), Path(b)).await), 1);
        assert_eq!(
            count(follow(st.clone(), AuthUser(a), Path(b)).await),
            1,
            "idempotent"
        );
        assert_eq!(count(follow(st.clone(), AuthUser(b), Path(a)).await), 1);
        assert_eq!(
            xmin(&state.db, b).await,
            before,
            "following must not rewrite the profile row"
        );
        assert!(matches!(
            follow(st.clone(), AuthUser(a), Path(a)).await,
            Err(AppError::Unprocessable(_))
        ));
        assert!(matches!(
            follow(st.clone(), AuthUser(a), Path(Uuid::nil())).await,
            Err(AppError::NotFound(_))
        ));
        assert!(
            get_profile(st.clone(), Some(AuthUser(a)), Path(b))
                .await
                .unwrap()
                .0
                .following
        );

        assert_eq!(count(unfollow(st.clone(), AuthUser(b), Path(a)).await), 0);
        assert_eq!(
            count(unfollow(st.clone(), AuthUser(b), Path(a)).await),
            0,
            "idempotent"
        );
        assert!(matches!(
            unfollow(st.clone(), AuthUser(a), Path(Uuid::nil())).await,
            Err(AppError::NotFound(_))
        ));

        // Cascade: deleting a follower decrements the count and emits an unfollow event.
        let c = user(&state, "Cy").await;
        assert_eq!(count(follow(st.clone(), AuthUser(c), Path(b)).await), 2);
        sqlx::query!("DELETE FROM profiles WHERE user_id = $1", c)
            .execute(&state.db)
            .await
            .unwrap();
        let p = get_profile(st.clone(), None, Path(b)).await.unwrap().0;
        assert_eq!(p.followers_count, 1);

        let got = events(&state.db).await;
        let kinds: Vec<(&str, Uuid, Uuid)> =
            got.iter().map(|(k, f, t)| (k.as_str(), *f, *t)).collect();
        assert_eq!(
            kinds,
            [
                ("follow", a, b),
                ("follow", b, a),
                ("unfollow", b, a),
                ("follow", c, b),
                ("unfollow", c, b),
            ],
            "one outbox event per change, in order, cascades included"
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn authors_internal_lookup(db: PgPool) {
        let state = state(db);
        let st = State(state.clone());
        let a = user(&state, "Ada").await;
        let b = user(&state, "Bob").await;
        let ids = |s: String| Query(IdsQuery { ids: s });

        let mut got = authors_internal(st.clone(), ids(format!("{a},{b},{}", Uuid::nil())))
            .await
            .unwrap()
            .0;
        got.sort_by(|x, y| x.name.cmp(&y.name));
        let names: Vec<(Uuid, &str)> = got.iter().map(|x| (x.id, x.name.as_str())).collect();
        assert_eq!(names, [(a, "Ada"), (b, "Bob")], "unknown id omitted");

        assert!(
            authors_internal(st.clone(), ids(String::new()))
                .await
                .unwrap()
                .0
                .is_empty()
        );
        assert!(matches!(
            authors_internal(st.clone(), ids("not-a-uuid".into())).await,
            Err(AppError::Unprocessable(_))
        ));
        let many = vec![Uuid::nil().to_string(); 101].join(",");
        assert!(matches!(
            authors_internal(st, ids(many)).await,
            Err(AppError::Unprocessable(_))
        ));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn internal_create_is_idempotent(db: PgPool) {
        let state = state(db);
        let st = State(state.clone());
        let id = Uuid::from_u128(7);
        let body = |name: &str| {
            Json(NewProfile {
                id,
                name: name.into(),
            })
        };
        assert_eq!(
            create_internal(st.clone(), body("  Ada ")).await.unwrap(),
            StatusCode::CREATED
        );
        assert_eq!(
            create_internal(st.clone(), body("Someone Else"))
                .await
                .unwrap(),
            StatusCode::OK,
            "second call is a no-op"
        );
        let p = get_profile(st.clone(), None, Path(id)).await.unwrap().0;
        assert_eq!(p.name, "Ada", "trimmed; not overwritten by the retry");
        assert!(matches!(
            create_internal(st, body("  ")).await,
            Err(AppError::Unprocessable(_))
        ));
    }
}
