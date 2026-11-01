use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use tokio::{net::TcpListener, signal};
use tracing::{info, warn};

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

    let listener = TcpListener::bind((config.server.listen, config.server.port))
        .await
        .context("Unable to bind listener")?;
    info!("Listening on {}", listener.local_addr()?);

    // ponytail: single-process sweeper; move to pg_cron if more than one instance runs
    let sweeper = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(300));
        loop {
            tick.tick().await;
            sweeper.email_limiter.prune();
            sweeper.ip_limiter.prune();
            if let Err(e) = sweeper.delete_expired_sessions().await {
                warn!(error = %e, "session sweep failed");
            }
        }
    });

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
