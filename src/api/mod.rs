mod process;
mod thread;

pub use process::*;
pub use thread::*;

use core::time::Duration;

/// A wait queue of threads.
pub trait WaitQueue: Default {
    /// Waits for a notification, with an optional timeout.
    ///
    /// Returns `true` if a notification came, `false` if the timeout expired.
    fn wait_timeout(&self, timeout: Option<Duration>) -> bool;

    /// Waits for a notification.
    fn wait(&self) {
        self.wait_timeout(None);
    }

    /// Notifies a waiting thread.
    ///
    /// Returns `true` if a thread was notified.
    fn notify_one(&self) -> bool;

    /// Notifies all waiting threads.
    fn notify_all(&self) {
        while self.notify_one() {}
    }
}
