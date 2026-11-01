use std::sync::LazyLock;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::Duration;
use tokio::sync::Semaphore;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::AppError;
use crate::state::{AppState, REFRESH_TTL, SESSION_TTL};
use crate::throttle::ClientIp;

pub const SESSION_COOKIE: &str = "__Host-session";
pub const REFRESH_COOKIE: &str = "__Host-refresh";

static ARGON2: LazyLock<Argon2<'static>> = LazyLock::new(Argon2::default);
// ponytail: global semaphore sized to cores; per-tenant limits if ever multi-tenant
static HASH_SLOTS: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(std::thread::available_parallelism().map_or(4, |n| n.get())));
/// Verified when the email is unknown so the response time does not reveal whether it exists.
/// Forced in `router::init` so the first unknown-email login does not pay for computing it.
pub static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
    ARGON2
        .hash_password(b"dummy")
        .expect("argon2 default params are valid")
        .to_string()
});

#[derive(Deserialize)]
pub struct RegisterUser {
    pub email: String,
    pub password: String,
    pub name: String,
}

#[derive(Deserialize)]
pub struct LoginUser {
    pub email: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct Me {
    pub id: Uuid,
}

fn normalize(password: &str) -> String {
    password.nfkc().collect()
}

fn validate(email: &str, password: &str) -> Result<(), AppError> {
    if email.len() > 254 || !email.contains('@') {
        return Err(AppError::Unprocessable("invalid email"));
    }
    if !(8..=128).contains(&password.len()) {
        return Err(AppError::Unprocessable("password must be 8-128 bytes"));
    }
    Ok(())
}

async fn hash_password(password: String) -> Result<String, AppError> {
    let _slot = HASH_SLOTS.acquire().await?;
    let hash = tokio::task::spawn_blocking(move || {
        ARGON2
            .hash_password(password.as_bytes())
            .map(|h| h.to_string())
    })
    .await??;
    Ok(hash)
}

async fn verify_password(password: String, hash: String) -> Result<bool, AppError> {
    let _slot = HASH_SLOTS.acquire().await?;
    let ok = tokio::task::spawn_blocking(move || {
        let parsed = PasswordHash::new(&hash)?;
        Ok::<_, argon2::password_hash::Error>(
            ARGON2.verify_password(password.as_bytes(), &parsed).is_ok(),
        )
    })
    .await??;
    Ok(ok)
}

/// (cookie value, SHA-256 of it for storage)
fn new_token() -> (String, [u8; 32]) {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let value = URL_SAFE_NO_PAD.encode(bytes);
    let hash = Sha256::digest(value.as_bytes()).into();
    (value, hash)
}

fn cookie(name: &'static str, value: String, ttl: Duration) -> Cookie<'static> {
    Cookie::build((name, value))
        .path("/")
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax)
        .max_age(ttl)
        .build()
}

fn session_cookies(jar: CookieJar, session: String, refresh: String) -> CookieJar {
    jar.add(cookie(SESSION_COOKIE, session, SESSION_TTL))
        .add(cookie(REFRESH_COOKIE, refresh, REFRESH_TTL))
}

#[tracing::instrument(skip_all)]
pub async fn register(
    State(state): State<AppState>,
    Json(user): Json<RegisterUser>,
) -> Result<StatusCode, AppError> {
    let password = normalize(&user.password);
    validate(&user.email, &password)?;
    if user.name.trim().is_empty() {
        return Err(AppError::Unprocessable("name is required"));
    }
    let hash = hash_password(password).await?;
    match state.add_user(&user.email, &hash, &user.name).await {
        Ok(()) => Ok(StatusCode::CREATED),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            Err(AppError::Conflict("email already registered"))
        }
        Err(e) => Err(e.into()),
    }
}

#[tracing::instrument(skip_all)]
pub async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    jar: CookieJar,
    Json(user): Json<LoginUser>,
) -> Result<CookieJar, AppError> {
    let key = user.email.trim().to_lowercase();
    let password = normalize(&user.password);
    // Checked before any database or argon2 work, and keyed only on what the request sent,
    // so a throttled response looks the same whether or not the email exists.
    if let Some(retry) = state
        .email_limiter
        .check(&key)
        .or_else(|| state.ip_limiter.check(&ip))
    {
        return Err(AppError::TooManyRequests(retry.as_secs().max(1)));
    }

    let found = state.get_user_password_hash(&user.email).await?;
    let hash = found
        .as_ref()
        .map_or_else(|| DUMMY_HASH.clone(), |(_, h)| h.clone());
    let ok = verify_password(password, hash).await?;
    let Some((user_id, _)) = found.filter(|_| ok) else {
        state.email_limiter.record(key);
        state.ip_limiter.record(ip);
        return Err(AppError::Unauthorized);
    };

    let (session, session_hash) = new_token();
    let (refresh, refresh_hash) = new_token();
    state
        .add_session(user_id, &session_hash, &refresh_hash)
        .await?;
    Ok(session_cookies(jar, session, refresh))
}

#[tracing::instrument(skip_all)]
pub async fn token(State(state): State<AppState>, jar: CookieJar) -> Result<CookieJar, AppError> {
    let old = jar.get(REFRESH_COOKIE).ok_or(AppError::Unauthorized)?;
    let old_hash: [u8; 32] = Sha256::digest(old.value().as_bytes()).into();
    let (session, session_hash) = new_token();
    let (refresh, refresh_hash) = new_token();
    if !state
        .refresh_session(&old_hash, &session_hash, &refresh_hash)
        .await?
    {
        if state.revoke_replayed_session(&old_hash).await? {
            tracing::warn!("refresh token replayed; session revoked");
        }
        return Err(AppError::Unauthorized);
    }
    Ok(session_cookies(jar, session, refresh))
}

pub async fn me(AuthUser(id): AuthUser) -> Json<Me> {
    Json(Me { id })
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use axum::extract::FromRequestParts;
    use axum::http::Request;
    use sqlx::PgPool;

    use super::*;
    use crate::config::ThrottleConfig;

    const PW: &str = "hunter2hunter2";

    fn state(db: PgPool) -> AppState {
        AppState::build(db, false, &ThrottleConfig::default())
    }

    fn ip(n: u8) -> ClientIp {
        ClientIp(IpAddr::V4(Ipv4Addr::new(10, 0, 0, n)))
    }

    fn reg(email: &str) -> Json<RegisterUser> {
        Json(RegisterUser {
            email: email.into(),
            password: PW.into(),
            name: "A".into(),
        })
    }

    fn creds(email: &str, password: &str) -> Json<LoginUser> {
        Json(LoginUser {
            email: email.into(),
            password: password.into(),
        })
    }

    async fn login_as(
        st: &State<AppState>,
        email: &str,
        password: &str,
        from: u8,
    ) -> Result<CookieJar, AppError> {
        login(
            st.clone(),
            ip(from),
            CookieJar::new(),
            creds(email, password),
        )
        .await
    }

    /// Runs the `AuthUser` extractor against a request carrying the jar's session cookie.
    async fn auth(state: &AppState, jar: &CookieJar) -> Result<Uuid, AppError> {
        let mut request = Request::builder();
        if let Some(c) = jar.get(SESSION_COOKIE) {
            request = request.header("cookie", format!("{}={}", c.name(), c.value()));
        }
        let (mut parts, _) = request.body(()).unwrap().into_parts();
        AuthUser::from_request_parts(&mut parts, state)
            .await
            .map(|AuthUser(id)| id)
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn register_login_refresh(db: PgPool) {
        let state = state(db);
        let st = State(state.clone());

        assert_eq!(
            register(st.clone(), reg("a@b.c")).await.unwrap(),
            StatusCode::CREATED
        );
        assert!(matches!(
            register(st.clone(), reg("a@b.c")).await,
            Err(AppError::Conflict(_))
        ));
        assert!(matches!(
            register(
                st.clone(),
                Json(RegisterUser {
                    password: "short".into(),
                    ..reg("x@b.c").0
                })
            )
            .await,
            Err(AppError::Unprocessable(_))
        ));
        assert!(matches!(
            register(st.clone(), reg("not-an-email")).await,
            Err(AppError::Unprocessable(_))
        ));

        assert!(matches!(
            login_as(&st, "a@b.c", "wrong-password", 1).await,
            Err(AppError::Unauthorized)
        ));
        assert!(matches!(
            login_as(&st, "nobody@b.c", PW, 1).await,
            Err(AppError::Unauthorized)
        ));

        let jar = login_as(&st, "a@b.c", PW, 1).await.unwrap();
        let old_refresh = jar.get(REFRESH_COOKIE).unwrap().value().to_owned();
        assert!(jar.get(SESSION_COOKIE).is_some());

        let jar = token(st.clone(), jar).await.unwrap();
        assert_ne!(jar.get(REFRESH_COOKIE).unwrap().value(), old_refresh);
        assert!(matches!(
            token(st.clone(), CookieJar::new()).await,
            Err(AppError::Unauthorized)
        ));

        sqlx::query!("UPDATE sessions SET expires_at = now() - interval '1 second'")
            .execute(&state.db)
            .await
            .unwrap();
        assert!(matches!(token(st, jar).await, Err(AppError::Unauthorized)));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn auth_extractor(db: PgPool) {
        let state = state(db);
        let st = State(state.clone());
        register(st.clone(), reg("a@b.c")).await.unwrap();

        assert!(matches!(
            auth(&state, &CookieJar::new()).await,
            Err(AppError::Unauthorized)
        ));

        let jar = login_as(&st, "a@b.c", PW, 1).await.unwrap();
        let expected = sqlx::query_scalar!("SELECT id FROM users WHERE email = $1", "a@b.c")
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(auth(&state, &jar).await.unwrap(), expected);

        let rotated = token(st.clone(), jar.clone()).await.unwrap();
        assert!(
            matches!(auth(&state, &jar).await, Err(AppError::Unauthorized)),
            "old session cookie is dead after rotation"
        );
        assert_eq!(auth(&state, &rotated).await.unwrap(), expected);

        sqlx::query!("UPDATE sessions SET session_expires_at = now() - interval '1 second'")
            .execute(&state.db)
            .await
            .unwrap();
        assert!(matches!(
            auth(&state, &rotated).await,
            Err(AppError::Unauthorized)
        ));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn refresh_replay_revokes_session(db: PgPool) {
        let state = state(db);
        let st = State(state.clone());
        register(st.clone(), reg("a@b.c")).await.unwrap();

        let jar = login_as(&st, "a@b.c", PW, 1).await.unwrap();
        let old_refresh = jar.get(REFRESH_COOKIE).unwrap().value().to_owned();
        let rotated = token(st.clone(), jar).await.unwrap();

        let replay = CookieJar::new().add(Cookie::new(REFRESH_COOKIE, old_refresh));
        assert!(matches!(
            token(st.clone(), replay).await,
            Err(AppError::Unauthorized)
        ));
        assert!(
            matches!(
                token(st.clone(), rotated.clone()).await,
                Err(AppError::Unauthorized)
            ),
            "the current refresh token is revoked too"
        );
        assert!(matches!(
            auth(&state, &rotated).await,
            Err(AppError::Unauthorized)
        ));
        let sessions = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM sessions"#)
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(sessions, 0);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn email_throttle_is_blind_to_account_existence(db: PgPool) {
        let state = state(db);
        let st = State(state.clone());
        register(st.clone(), reg("a@b.c")).await.unwrap();
        register(st.clone(), reg("b@b.c")).await.unwrap();

        for email in ["a@b.c", "ghost@b.c"] {
            for _ in 0..5 {
                assert!(matches!(
                    login_as(&st, email, "wrong-password", 1).await,
                    Err(AppError::Unauthorized)
                ));
            }
            assert!(matches!(
                login_as(&st, email, PW, 1).await,
                Err(AppError::TooManyRequests(secs)) if (1..=60).contains(&secs)
            ));
        }
        assert!(
            matches!(
                login_as(&st, "A@B.C ", PW, 1).await,
                Err(AppError::TooManyRequests(_))
            ),
            "key is case- and whitespace-insensitive"
        );
        assert!(
            login_as(&st, "b@b.c", PW, 1).await.is_ok(),
            "other emails from the same IP are unaffected"
        );
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn ip_throttle(db: PgPool) {
        let throttle = ThrottleConfig {
            email_failures_per_minute: 5,
            ip_failures_per_minute: 3,
        };
        let state = AppState::build(db, false, &throttle);
        let st = State(state.clone());

        for n in 0..3 {
            assert!(matches!(
                login_as(&st, &format!("g{n}@b.c"), PW, 2).await,
                Err(AppError::Unauthorized)
            ));
        }
        assert!(matches!(
            login_as(&st, "g9@b.c", PW, 2).await,
            Err(AppError::TooManyRequests(_))
        ));
        assert!(matches!(
            login_as(&st, "g9@b.c", PW, 3).await,
            Err(AppError::Unauthorized)
        ));
    }
}
