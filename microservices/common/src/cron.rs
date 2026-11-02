use anyhow::{Context, ensure};
use sqlx::PgPool;

/// Fails unless pg_cron is loaded and the `sweep-sessions` job is scheduled in this database.
/// The user-api migration installs that job only where pg_cron is configured, so without this
/// check a database missing it would silently keep expired sessions forever.
pub async fn check_session_sweeper(db: &PgPool) -> anyhow::Result<()> {
    // Unchecked query: test databases have no `cron` schema, so this must not be a
    // compile-time macro.
    let (job_active, launcher_running): (Option<bool>, bool) = sqlx::query_as(
        "SELECT (SELECT active FROM cron.job WHERE jobname = 'sweep-sessions'),
                EXISTS (SELECT 1 FROM pg_stat_activity WHERE backend_type = 'pg_cron launcher')",
    )
    .fetch_one(db)
    .await
    .context("pg_cron is not installed in this database (cron.job is missing)")?;
    ensure!(
        launcher_running,
        "pg_cron launcher is not running: add pg_cron to shared_preload_libraries"
    );
    ensure!(
        job_active == Some(true),
        "sweep-sessions job is missing or inactive: set cron.database_name to this database and re-run the migrations"
    );
    Ok(())
}
