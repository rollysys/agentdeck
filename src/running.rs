//! Per-profile "agent is currently running inside this deck" counter.
//!
//! Each WebSocket-backed pty session counts as one in-flight launch.
//! `handle` in `ws_handler.rs` acquires a `RunningGuard` right after a
//! successful spawn; the guard decrements on drop, covering every exit
//! path (clean close, ws error, pty SIGHUP, panic). A simple
//! `std::sync::Mutex` is enough — increments happen on spawn, decrements
//! on teardown, reads come from an HTTP poll every ~10s. Lock hold is
//! microseconds.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
pub struct RunningCounter {
    inner: Mutex<HashMap<String, usize>>,
}

impl RunningCounter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Increment and return an RAII guard that decrements on drop.
    pub fn track(self: &Arc<Self>, profile: impl Into<String>) -> RunningGuard {
        let profile = profile.into();
        {
            let mut m = self.inner.lock().expect("running counter poisoned");
            *m.entry(profile.clone()).or_insert(0) += 1;
        }
        RunningGuard {
            counter: Arc::clone(self),
            profile,
        }
    }

    pub fn snapshot(&self) -> HashMap<String, usize> {
        self.inner
            .lock()
            .expect("running counter poisoned")
            .clone()
    }
}

pub struct RunningGuard {
    counter: Arc<RunningCounter>,
    profile: String,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        let mut m = self.counter.inner.lock().expect("running counter poisoned");
        if let Some(n) = m.get_mut(&self.profile) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.profile);
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ProfileStatus {
    pub running: usize,
    pub last_active_ts: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_inc_drop_dec() {
        let c = RunningCounter::new();
        assert_eq!(c.snapshot().len(), 0);
        let g1 = c.track("a");
        let g2 = c.track("a");
        let g3 = c.track("b");
        let snap = c.snapshot();
        assert_eq!(snap.get("a"), Some(&2));
        assert_eq!(snap.get("b"), Some(&1));
        drop(g1);
        drop(g3);
        let snap = c.snapshot();
        assert_eq!(snap.get("a"), Some(&1));
        assert!(snap.get("b").is_none()); // removed when zero
        drop(g2);
        assert_eq!(c.snapshot().len(), 0);
    }
}
