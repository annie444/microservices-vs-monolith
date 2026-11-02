use std::net::IpAddr;
use std::time::Duration;

use anyhow::Context;
use config::{Config, Environment, File};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// `config.toml` in the working directory, overridden by `<PREFIX>_<SECTION>__<KEY>` env vars
/// (e.g. `USER_API_SERVER__PORT`). Services do not read `DATABASE_URL`: that is the compile-time
/// URL for the sqlx macros and the monolith's database.
pub fn load<T: DeserializeOwned>(env_prefix: &str) -> anyhow::Result<T> {
    Config::builder()
        .add_source(File::with_name("config").required(false))
        .add_source(File::with_name("/app/config").required(false))
        .add_source(
            Environment::with_prefix(env_prefix)
                .prefix_separator("_")
                .separator("__"),
        )
        .build()
        .context("Unable to read config")?
        .try_deserialize()
        .context("Invalid config")
}

/// A fixed-size pool (`min == max`) so the benchmark never pays for connection warm-up.
pub async fn pool(db: &DbConfig) -> sqlx::Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(db.max_connections)
        .min_connections(db.max_connections)
        .acquire_timeout(Duration::from_secs(3))
        .connect(&db.url)
        .await
}

#[derive(Deserialize)]
pub struct ServerConfig {
    pub listen: IpAddr,
    pub port: u16,
    /// Trust the rightmost `X-Forwarded-For` entry as the client IP. Only enable behind a
    /// proxy that always appends it; otherwise clients can forge it.
    #[serde(default)]
    pub behind_proxy: bool,
}

#[derive(Deserialize)]
pub struct DbConfig {
    pub url: String,
    #[serde(default = "default_pool_size")]
    pub max_connections: u32,
}

fn default_pool_size() -> u32 {
    2 * std::thread::available_parallelism().map_or(4, |n| n.get() as u32)
}

/// Failed-login limits per fixed 60 s window. `0` disables a limiter.
#[derive(Deserialize)]
pub struct ThrottleConfig {
    #[serde(default = "default_email_failures")]
    pub email_failures_per_minute: u32,
    #[serde(default = "default_ip_failures")]
    pub ip_failures_per_minute: u32,
}

impl Default for ThrottleConfig {
    fn default() -> Self {
        Self {
            email_failures_per_minute: default_email_failures(),
            ip_failures_per_minute: default_ip_failures(),
        }
    }
}

fn default_email_failures() -> u32 {
    5
}

fn default_ip_failures() -> u32 {
    20
}
