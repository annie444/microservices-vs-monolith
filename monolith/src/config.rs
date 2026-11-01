use std::net::IpAddr;

use anyhow::Context;
use config::Config;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub db: DbConfig,
    #[serde(default)]
    pub throttle: ThrottleConfig,
}

impl AppConfig {
    /// `config.toml` in the working directory, overridden by `MONOLITH_<SECTION>__<KEY>`
    /// env vars; `DATABASE_URL` (already required by the sqlx macros) wins for `db.url`.
    pub fn load() -> anyhow::Result<Self> {
        Config::builder()
            .add_source(config::File::with_name("config").required(false))
            .add_source(
                config::Environment::with_prefix("MONOLITH")
                    .prefix_separator("_")
                    .separator("__"),
            )
            .set_override_option("db.url", std::env::var("DATABASE_URL").ok())?
            .build()
            .context("Unable to read config")?
            .try_deserialize()
            .context("Invalid config")
    }
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
