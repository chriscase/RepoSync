//! An explicit recovery-only startup: existing storage, no ordinary bootstrap,
//! migration, credential seeding, checkpoint restoration or scheduler.
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use reposync_core::{
    config::AppConfig, db::Database, git::GitClient, identity::IdentityMapper, svn::SvnClient,
    sync_engine::SyncEngine,
};

pub(crate) async fn serve(mut config: AppConfig, config_path: PathBuf) -> Result<()> {
    let directory = config
        .daemon
        .data_dir
        .canonicalize()
        .context("recovery requires an existing data directory")?;
    let lock = Arc::new(reposync_core::data_dir::acquire(&directory)?);
    let path = directory.join("reposync.db");
    let db = Database::open_existing_for_pair_refresh_recovery(&path)?;
    let web_db = Database::open_existing_for_pair_refresh_recovery(&path)?;
    config.resolve_recovery_secrets_from_db(&db)?;
    let git = GitClient::open_existing_for_pair_refresh_recovery(directory.join("git-repo"))
        .context("recovery requires the existing engine Git repository")?;
    let svn = SvnClient::new(
        &config.svn.url,
        &config.svn.username,
        config.svn.password.as_deref().unwrap_or(""),
    );
    let identity = Arc::new(IdentityMapper::new(&config.identity)?);
    let engine = Arc::new(SyncEngine::new(config.clone(), db, svn, git, identity));
    let listen = config.web.listen.clone();
    let server = reposync_web::WebServer::for_pair_refresh_recovery(
        config,
        web_db,
        engine,
        config_path,
        lock,
    )?;
    server.start(&listen).await
}
