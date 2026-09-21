//! Process image metadata exported through exec and procfs.

use alloc::{string::String, sync::Arc, vec::Vec};
use core::mem;

use kernel_elf_parser::AuxEntry;

use super::ProcessData;
use axfs_ng_vfs::Location;
use crate::sync::Mutex;

/// Metadata supplied when a process image is created.
pub struct ProcessImage {
    exe_path: String,
    /// Backing file of the executable image, when the image was created by
    /// `execve`-style loading. `/proc/<pid>/exe` opens this file directly (a
    /// magic link must not re-resolve the display path: a memfd-exec'd runc
    /// reports `/memfd:... (deleted)`, which is not a resolvable path).
    pub exe_location: Option<Location>,
    cmdline: Arc<Vec<String>>,
    envp: Arc<Vec<String>>,
    auxv: Vec<AuxEntry>,
    root_path: String,
    cwd_path: String,
}

impl ProcessImage {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        exe_path: String,
        exe_location: Option<Location>,
        cmdline: Arc<Vec<String>>,
        envp: Arc<Vec<String>>,
        auxv: Vec<AuxEntry>,
        root_path: String,
        cwd_path: String,
    ) -> Self {
        Self {
            exe_path,
            exe_location,
            cmdline,
            envp,
            auxv,
            root_path,
            cwd_path,
        }
    }
}

/// Independently synchronized image metadata shared by a thread group.
pub(super) struct ProcessImageState {
    exe_path: Mutex<Arc<String>>,
    exe_location: Mutex<Option<Location>>,
    cmdline: Mutex<Arc<Vec<String>>>,
    envp: Mutex<Arc<Vec<String>>>,
    auxv: Mutex<Arc<Vec<AuxEntry>>>,
    root_path: Mutex<Arc<String>>,
    cwd_path: Mutex<Arc<String>>,
}

impl ProcessImageState {
    pub(super) fn new(image: ProcessImage) -> Self {
        Self {
            exe_path: Mutex::new(Arc::new(image.exe_path)),
            exe_location: Mutex::new(image.exe_location),
            cmdline: Mutex::new(image.cmdline),
            envp: Mutex::new(image.envp),
            auxv: Mutex::new(Arc::new(image.auxv)),
            root_path: Mutex::new(Arc::new(image.root_path)),
            cwd_path: Mutex::new(Arc::new(image.cwd_path)),
        }
    }
}

fn snapshot<T>(slot: &Mutex<Arc<T>>) -> Arc<T> {
    slot.lock().clone()
}

fn replace_snapshot<T>(slot: &Mutex<Arc<T>>, replacement: Arc<T>) {
    let previous = {
        let mut current = slot.lock();
        mem::replace(&mut *current, replacement)
    };
    // The old snapshot may own the final allocation reference.
    drop(previous);
}

impl ProcessData {
    pub fn exe_path(&self) -> Arc<String> {
        snapshot(&self.image.exe_path)
    }

    pub fn set_exe_path(&self, path: String) {
        replace_snapshot(&self.image.exe_path, Arc::new(path));
    }

    /// Backing executable file of the current image; opened by
    /// `/proc/<pid>/exe` (a magic link must not re-resolve the display path).
    pub fn exe_location(&self) -> Option<Location> {
        self.image.exe_location.lock().clone()
    }

    pub fn set_exe_location(&self, location: Option<Location>) {
        *self.image.exe_location.lock() = location;
    }

    pub fn cmdline(&self) -> Arc<Vec<String>> {
        snapshot(&self.image.cmdline)
    }

    pub fn set_cmdline(&self, cmdline: Arc<Vec<String>>) {
        replace_snapshot(&self.image.cmdline, cmdline);
    }

    pub fn envp(&self) -> Arc<Vec<String>> {
        snapshot(&self.image.envp)
    }

    pub fn set_envp(&self, envp: Arc<Vec<String>>) {
        replace_snapshot(&self.image.envp, envp);
    }

    pub fn auxv(&self) -> Arc<Vec<AuxEntry>> {
        snapshot(&self.image.auxv)
    }

    pub fn set_auxv(&self, auxv: Vec<AuxEntry>) {
        replace_snapshot(&self.image.auxv, Arc::new(auxv));
    }

    pub fn root_path(&self) -> Arc<String> {
        snapshot(&self.image.root_path)
    }

    pub fn set_root_path(&self, path: String) {
        replace_snapshot(&self.image.root_path, Arc::new(path));
    }

    pub fn cwd_path(&self) -> Arc<String> {
        snapshot(&self.image.cwd_path)
    }

    pub fn set_cwd_path(&self, path: String) {
        replace_snapshot(&self.image.cwd_path, Arc::new(path));
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::ProcessImageState;
    use crate::sync::Mutex;

    #[test]
    fn process_image_heap_fields_use_sleepable_pi_locks() {
        fn assert_pi_mutex<T>(_: &Mutex<T>) {}
        fn assert_image_lock_types(image: &ProcessImageState) {
            assert_pi_mutex(&image.exe_path);
            assert_pi_mutex(&image.cmdline);
            assert_pi_mutex(&image.envp);
            assert_pi_mutex(&image.auxv);
            assert_pi_mutex(&image.root_path);
            assert_pi_mutex(&image.cwd_path);
        }

        let _ = assert_image_lock_types as fn(&ProcessImageState);
    }
}
