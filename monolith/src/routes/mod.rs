pub mod articles;
pub mod comments;
pub mod profile;
pub mod user;

pub async fn health() -> &'static str {
    "ok"
}
