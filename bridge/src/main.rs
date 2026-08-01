//! ThatsMatter bridge process entrypoint.

use std::sync::Arc;

use clap::Parser;
use thatsmatter_bridge::AppState;
use thatsmatter_bridge::catalog::CatalogStore;
use thatsmatter_bridge::config::{BackendKind, Config};
use thatsmatter_bridge::ipc;
use thatsmatter_bridge::matter::{DevMatterBackend, MatterBackend, RsMatterBackend};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
    .init();

  let cfg = Config::parse();
  cfg.ensure_loopback()?;
  std::fs::create_dir_all(&cfg.data_dir)?;

  let catalog = CatalogStore::load_or_new(&cfg.data_dir, &cfg.bridge_name)?;

  let backend: Arc<dyn MatterBackend> = match cfg.matter_backend {
    BackendKind::Dev => {
      let b = Arc::new(DevMatterBackend::new(&cfg.data_dir)?);
      b.start().await?;
      let exports = catalog.list();
      b.set_exports(&exports).await?;
      b
    }
    BackendKind::RsMatter => {
      let b = Arc::new(RsMatterBackend::new(&cfg.data_dir)?);
      b.start().await?;
      let exports = catalog.list();
      b.set_exports(&exports).await?;
      b
    }
  };

  let state = Arc::new(AppState::new(catalog, backend));

  tracing::info!(
    listen = %cfg.listen,
    data_dir = %cfg.data_dir.display(),
    backend = ?cfg.matter_backend,
    "thatsmatter-bridge starting"
  );

  ipc::serve(cfg.listen, state).await
}
