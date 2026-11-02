use common::config::{DbConfig, ServerConfig, ThrottleConfig};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub db: DbConfig,
    #[serde(default)]
    pub throttle: ThrottleConfig,
    pub profile_api: ProfileApiConfig,
}

/// Where registration creates the profile (`POST {url}/internal/profiles`).
#[derive(Deserialize)]
pub struct ProfileApiConfig {
    pub url: String,
}

impl AppConfig {
    /// `config.toml` in the working directory, overridden by `USER_API_<SECTION>__<KEY>`.
    pub fn load() -> anyhow::Result<Self> {
        common::config::load("USER_API")
    }
}
