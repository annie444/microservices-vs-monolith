use std::io::IsTerminal;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

pub fn init_tracing() -> WorkerGuard {
    let (writer, guard) = tracing_appender::non_blocking(std::io::stdout());
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let fmt_layer = fmt::layer()
        .compact()
        .with_ansi(std::io::stdout().is_terminal())
        .with_writer(writer);
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .init();
    guard
}
