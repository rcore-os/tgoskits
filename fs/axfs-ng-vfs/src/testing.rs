//! Fixed host-test signals for the real single-name unlink publication boundary.

use std::{
    sync::{Condvar, Mutex},
    time::Duration,
};

#[derive(Default)]
struct UnlinkProgress {
    published: bool,
    resume: bool,
    reader_blocked: Option<bool>,
}

/// Fixture-local rendezvous; never calls an arbitrary callback under a gate.
#[derive(Default)]
pub struct UnlinkPublicationProbe {
    progress: Mutex<UnlinkProgress>,
    changed: Condvar,
}

impl UnlinkPublicationProbe {
    pub(crate) fn pause_after_publication(&self) {
        let mut progress = self.progress.lock().unwrap();
        progress.published = true;
        self.changed.notify_all();
        let (_progress, timeout) = self
            .changed
            .wait_timeout_while(progress, Duration::from_secs(10), |p| !p.resume)
            .unwrap();
        assert!(!timeout.timed_out(), "unlink publication was not resumed");
    }

    pub fn wait_for_publication(&self) {
        let (progress, _) = self
            .changed
            .wait_timeout_while(
                self.progress.lock().unwrap(),
                Duration::from_secs(10),
                |p| !p.published,
            )
            .unwrap();
        assert!(progress.published, "unlink did not publish its candidate");
    }

    /// Reports the actual native lookup's unsuccessful gate try-lock.
    pub fn reader_entering_backend(&self, blocked: bool) {
        self.progress.lock().unwrap().reader_blocked = Some(blocked);
        self.changed.notify_all();
    }

    /// A fast-path return must wake the observer too, rather than deadlock it.
    pub fn reader_completed(&self) {
        self.progress
            .lock()
            .unwrap()
            .reader_blocked
            .get_or_insert(false);
        self.changed.notify_all();
    }

    pub fn wait_for_reader(&self) -> bool {
        let (progress, _) = self
            .changed
            .wait_timeout_while(
                self.progress.lock().unwrap(),
                Duration::from_secs(10),
                |p| p.reader_blocked.is_none(),
            )
            .unwrap();
        progress.reader_blocked == Some(true)
    }

    pub fn resume(&self) {
        self.progress.lock().unwrap().resume = true;
        self.changed.notify_all();
    }
}
