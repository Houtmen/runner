use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::runtime::{
    OutputStream, RuntimeError, RuntimeResult, RuntimeSession, SessionRuntime, SessionStatus,
    SpawnSpec,
};

#[derive(Default)]
pub(crate) struct SessionLimit {
    configuration: Mutex<()>,
    state: Mutex<LimitState>,
}

#[derive(Default)]
struct LimitState {
    maximum: Option<usize>,
    active: usize,
}

impl SessionLimit {
    pub(crate) fn info(&self) -> runner_core::protocol::SessionLimitInfo {
        let state = self.state.lock().unwrap();
        runner_core::protocol::SessionLimitInfo {
            maximum: state.maximum,
            active: state.active,
        }
    }

    pub(crate) fn configure_with(
        &self,
        maximum: Option<usize>,
        persist: impl FnOnce() -> crate::error::Result<()>,
    ) -> crate::error::Result<()> {
        if maximum == Some(0) {
            return Err(crate::error::Error::msg("session limit must be positive"));
        }
        // Serialize saves without holding the admission lock during database IO.
        // This avoids a DB/admission lock inversion with concurrent spawns.
        let _configuration = self.configuration.lock().unwrap();
        persist()?;
        self.state.lock().unwrap().maximum = maximum;
        Ok(())
    }

    pub(crate) fn configure(&self, maximum: Option<usize>) -> RuntimeResult<()> {
        if maximum == Some(0) {
            return Err(RuntimeError::Msg("session limit must be positive".into()));
        }
        let _configuration = self.configuration.lock().unwrap();
        self.state.lock().unwrap().maximum = maximum;
        Ok(())
    }

    pub(crate) fn acquire(self: &Arc<Self>) -> RuntimeResult<Permit> {
        let mut state = self.state.lock().unwrap();
        if state.maximum.is_some_and(|maximum| state.active >= maximum) {
            return Err(RuntimeError::Msg(format!(
                "global session limit reached ({}); stop a session and retry",
                state.maximum.unwrap()
            )));
        }
        state.active += 1;
        Ok(Permit(Arc::clone(self)))
    }
}

pub(crate) struct Permit(Arc<SessionLimit>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().active -= 1;
    }
}

pub(crate) struct LimitedRuntime {
    inner: Arc<dyn SessionRuntime>,
    limit: Arc<SessionLimit>,
    permits: Mutex<HashMap<String, Permit>>,
}

impl LimitedRuntime {
    pub(crate) fn new(inner: Arc<dyn SessionRuntime>, limit: Arc<SessionLimit>) -> Self {
        Self {
            inner,
            limit,
            permits: Mutex::default(),
        }
    }

    fn release(&self, session: &RuntimeSession) {
        self.permits.lock().unwrap().remove(&session.session_id);
    }
}

impl SessionRuntime for LimitedRuntime {
    fn spawn(&self, spec: SpawnSpec) -> RuntimeResult<(RuntimeSession, OutputStream)> {
        let permit = self.limit.acquire()?;
        let (session, output) = self.inner.spawn(spec)?;
        self.permits
            .lock()
            .unwrap()
            .insert(session.session_id.clone(), permit);
        Ok((session, output))
    }

    fn stop(&self, session: &RuntimeSession) -> RuntimeResult<()> {
        self.inner.stop(session)?;
        // A failed stop or an unknown transport state must retain its capacity.
        if self
            .inner
            .status(session)?
            .is_none_or(|status| !status.alive)
        {
            self.release(session);
        }
        Ok(())
    }

    fn status(&self, session: &RuntimeSession) -> RuntimeResult<Option<SessionStatus>> {
        let status = self.inner.status(session)?;
        if status.as_ref().is_none_or(|status| !status.alive) {
            self.release(session);
        }
        Ok(status)
    }

    fn send_bytes(&self, session: &RuntimeSession, bytes: &[u8]) -> RuntimeResult<()> {
        self.inner.send_bytes(session, bytes)
    }
    fn send_key(&self, session: &RuntimeSession, key: &str) -> RuntimeResult<()> {
        self.inner.send_key(session, key)
    }
    fn resize(&self, session: &RuntimeSession, cols: u16, rows: u16) -> RuntimeResult<()> {
        self.inner.resize(session, cols, rows)
    }
    fn has_foreground_process(&self, session: &RuntimeSession) -> RuntimeResult<Option<bool>> {
        self.inner.has_foreground_process(session)
    }
    fn admit_hook(
        &self,
        report: runner_core::protocol::HookReport,
    ) -> crate::error::Result<super::hook_queue::Admission> {
        self.inner.admit_hook(report)
    }
    fn drain_workers(&self) {
        self.inner.drain_workers();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_admission_is_atomic_and_drop_returns_capacity() {
        let limit = Arc::new(SessionLimit::default());
        limit.configure(Some(2)).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(9));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let limit = Arc::clone(&limit);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    limit.acquire().ok()
                })
            })
            .collect();
        barrier.wait();
        let mut permits: Vec<_> = threads
            .into_iter()
            .filter_map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(permits.len(), 2);
        assert!(limit.acquire().is_err());
        permits.pop();
        assert!(limit.acquire().is_ok());
        drop(permits);
        assert_eq!(limit.state.lock().unwrap().active, 0);
    }

    #[test]
    fn unlimited_default_and_lowering_limit_preserve_live_sessions() {
        let limit = Arc::new(SessionLimit::default());
        let first = limit.acquire().unwrap();
        let second = limit.acquire().unwrap();
        limit.configure(Some(1)).unwrap();
        assert!(limit.acquire().is_err());
        drop(first);
        assert!(limit.acquire().is_err());
        drop(second);
        assert!(limit.acquire().is_ok());
        assert!(limit.configure(Some(0)).is_err());
    }
}
