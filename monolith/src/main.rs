use std::net::SocketAddr;

use anyhow::Context;
use tokio::{net::TcpListener, signal};
use tracing::info;

mod auth;
mod config;
mod error;
mod logging;
mod router;
mod routes;
mod state;
mod throttle;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _guard = logging::init_tracing();
    let config = config::AppConfig::load()?;

    let state = state::AppState::new(&config)
        .await
        .context("Unable to connect to database")?;
    state.migrate().await.context("Unable to run migrations")?;
    state
        .check_session_sweeper()
        .await
        .context("Expired sessions would never be swept; refusing to start")?;

    let listener = TcpListener::bind((config.server.listen, config.server.port))
        .await
        .context("Unable to bind listener")?;
    info!("Listening on {}", listener.local_addr()?);

    // Expired sessions are swept by pg_cron (migrations/0001_user.up.sql); the login limiters
    // prune themselves as they grow.
    let app = router::init(state).into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("Server error")
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
