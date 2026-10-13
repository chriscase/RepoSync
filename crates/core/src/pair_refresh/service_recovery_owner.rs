//! Recovery epoch lifetime belongs to the server, not an individual request.
use super::*;

pub(super) struct RecoveryOwner {
    engine: Arc<SyncEngine>,
    _lock: Arc<crate::data_dir::LockGuard>,
    lease: Mutex<Option<(Uuid, crate::writer_fence::RecoveryWriterFence)>>,
}

impl RecoveryOwner {
    pub(super) fn new(
        engine: Arc<SyncEngine>,
        lock: Arc<crate::data_dir::LockGuard>,
    ) -> Result<Arc<Self>> {
        lock.require_current(&engine.config().daemon.data_dir)?;
        Ok(Arc::new(Self {
            engine,
            _lock: lock,
            lease: Mutex::new(None),
        }))
    }

    pub(super) fn require_or_claim(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        id: Uuid,
        busy: &crate::busy::PairBusyGuard,
    ) -> Result<()> {
        anyhow::ensure!(
            std::ptr::eq(self.engine.as_ref(), engine),
            "recovery owner engine changed"
        );
        self._lock
            .require_current(&engine.config().daemon.data_dir)?;
        let mut lease = self.lease.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((claimed, fence)) = lease.as_ref() {
            anyhow::ensure!(
                *claimed == id,
                "recovery epoch belongs to another held request"
            );
            fence.require_current(engine.db())?;
        } else {
            let fence = super::super::recovery::claim_owned_refresh_recovery_writer(
                engine,
                actor,
                id,
                busy,
                &self._lock,
            )?;
            *lease = Some((id, fence.into_recovery_lifetime()));
        }
        Ok(())
    }
}

impl Drop for RecoveryOwner {
    fn drop(&mut self) {
        if let Some((_, fence)) = self
            .lease
            .get_mut()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            // Every worker also owns this Arc, so no worker can still publish.
            // Held/corrupt history preserves the lease and process identity for
            // native owner-death recovery. Terminal cleanup uses this connection.
            let _ = fence.release_if_quiet(self.engine.db());
        }
    }
}
