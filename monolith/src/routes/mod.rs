pub mod articles;
pub mod profile;
pub mod user;

pub async fn health() -> &'static str {
    "ok"
}
