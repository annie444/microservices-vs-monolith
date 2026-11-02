//! Plumbing shared by the microservices: errors, config loading, logging, login throttling,
//! session cookies and the `AuthUser` extractor that verifies sessions in the user database.

pub mod auth;
pub mod config;
pub mod cron;
pub mod error;
pub mod logging;
pub mod throttle;
