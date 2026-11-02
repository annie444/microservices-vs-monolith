pub mod articles;
pub mod internal;

pub async fn health() -> &'static str {
    "ok"
}
