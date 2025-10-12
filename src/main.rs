mod app;
mod config;
mod protocol;
mod server;
mod state;
mod tui;
mod ui;

use clap::Parser;
use color_eyre::{Result, eyre::eyre};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    init_tracing()?;

    let config = config::Config::parse();
    let app = app::RaygunApp::bootstrap(config).await?;
    app.run().await
}

fn init_tracing() -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("raygun=info,raygun::app=debug"))?;

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .try_init()
        .map_err(|err| eyre!(err))?;

    Ok(())
}
