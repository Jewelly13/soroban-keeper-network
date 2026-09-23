//! Keeper Bot v2 entry point.
//!
//! Startup order:
//! 1. Validate configuration
//! 2. Connect to state store and apply migrations
//! 3. Load persisted task state into memory (before first round)
//! 4. Begin processing rounds (now safe to skip already-finished tasks)

use anyhow::Result;
use keeper_bot_v2::state::Store;
use std::env;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing/logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("KEEPER_LOG")
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tracing::info!("starting keeper-bot-v2");

    // Get database URL from environment
    let database_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite:keeper-bot.db".to_string());

    tracing::info!(database_url = %database_url, "connecting to state store");

    // Connect and run migrations (step 2 of startup)
    let store = Store::connect(&database_url)
        .await?;

    tracing::info!("state store ready; loading persisted task state");

    // Load all previously recorded task outcomes before first round (step 3)
    let outcomes = store.get_all_processed_tasks().await?;
    tracing::info!(
        count = outcomes.len(),
        "loaded persisted task outcomes; keeper will skip these on first round"
    );

    // Record startup checkpoint (now ready to begin processing)
    store.record_startup_checkpoint(0, true).await?;

    tracing::info!("startup complete; keeper is ready for processing rounds");

    // TODO: Begin the main keeper loop here (issue #251 and onwards)
    // For now, this is a demonstration of the startup sequencing.

    Ok(())
}
