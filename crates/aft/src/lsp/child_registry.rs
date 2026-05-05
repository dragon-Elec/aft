//! Process-wide registry of LSP child PIDs spawned by `LspClient::spawn`.
//!
//! Mirrors the `BgTaskRegistry` pattern: `Arc`-cloneable handle that the
//! signal handler thread can use to SIGKILL all child language servers
//! before the aft process exits. Without this registry, LSP children get
//! orphaned to PID 1 when aft is SIGTERM'd by its parent (e.g., during
//! plugin bridge.shutdown() or e2e test cleanup), accumulating across runs.
//!
//! The registry intentionally does NOT do graceful shutdown — that takes
//! up to 5 seconds per server (shutdown request + exit notification +
//! poll). Signal handlers must finish quickly. Graceful shutdown still
//! happens on the natural stdin-closed exit path via `LspManager::shutdown_all`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct LspChildRegistry {
    inner: Arc<Mutex<HashSet<u32>>>,
}

impl LspChildRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Track a newly-spawned LSP child PID.
    pub fn track(&self, pid: u32) {
        if let Ok(mut set) = self.inner.lock() {
            set.insert(pid);
        }
    }

    /// Forget a PID (called when the client is dropped or shut down gracefully).
    pub fn untrack(&self, pid: u32) {
        if let Ok(mut set) = self.inner.lock() {
            set.remove(&pid);
        }
    }

    /// Snapshot of currently-tracked PIDs.
    pub fn pids(&self) -> Vec<u32> {
        self.inner
            .lock()
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Force-kill every tracked child synchronously. Used by the signal
    /// handler to prevent orphaned LSP processes when aft is SIGTERM'd.
    /// Returns the number of PIDs that were sent SIGKILL.
    #[cfg(unix)]
    pub fn kill_all(&self) -> usize {
        use std::os::raw::c_int;
        let pids = self.pids();
        let mut killed = 0;
        for pid in pids {
            // SIGKILL = 9. We use the raw libc call rather than crossbeam
            // because we're inside a signal-handler context where allocator
            // and channel use is risky.
            // SAFETY: kill(2) is async-signal-safe.
            unsafe {
                let pid_t = pid as libc::pid_t;
                let rc = libc::kill(pid_t, 9 as c_int);
                if rc == 0 {
                    killed += 1;
                }
            }
        }
        killed
    }

    /// Windows fallback: best-effort kill via `taskkill`. Not technically
    /// async-signal-safe but Windows doesn't deliver signals the same way.
    #[cfg(not(unix))]
    pub fn kill_all(&self) -> usize {
        let pids = self.pids();
        let mut killed = 0;
        for pid in pids {
            if std::process::Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .status()
                .is_ok()
            {
                killed += 1;
            }
        }
        killed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_untrack_pids_round_trip() {
        let reg = LspChildRegistry::new();
        reg.track(100);
        reg.track(200);
        let mut pids = reg.pids();
        pids.sort();
        assert_eq!(pids, vec![100, 200]);
        reg.untrack(100);
        assert_eq!(reg.pids(), vec![200]);
    }

    #[test]
    fn clones_share_state() {
        let a = LspChildRegistry::new();
        let b = a.clone();
        a.track(42);
        assert_eq!(b.pids(), vec![42]);
        b.untrack(42);
        assert!(a.pids().is_empty());
    }

    #[test]
    fn untracking_unknown_pid_is_safe() {
        let reg = LspChildRegistry::new();
        reg.untrack(999); // no-op, no panic
        assert!(reg.pids().is_empty());
    }

    #[test]
    fn kill_all_with_no_pids_returns_zero() {
        let reg = LspChildRegistry::new();
        assert_eq!(reg.kill_all(), 0);
    }
}
