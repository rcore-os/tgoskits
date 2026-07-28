//! Process image metadata exported through exec and procfs.

use alloc::{string::String, sync::Arc, vec::Vec};

use ax_kspin::{SpinRwLock, SpinRwLockReadGuard};
use kernel_elf_parser::AuxEntry;

use super::ProcessData;

/// Metadata supplied when a process image is created.
pub struct ProcessImage {
    exe_path: String,
    cmdline: Arc<Vec<String>>,
    envp: Arc<Vec<String>>,
    auxv: Vec<AuxEntry>,
    root_path: String,
    cwd_path: String,
}

impl ProcessImage {
    pub fn new(
        exe_path: String,
        cmdline: Arc<Vec<String>>,
        envp: Arc<Vec<String>>,
        auxv: Vec<AuxEntry>,
        root_path: String,
        cwd_path: String,
    ) -> Self {
        Self {
            exe_path,
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
    exe_path: SpinRwLock<String>,
    cmdline: SpinRwLock<Arc<Vec<String>>>,
    envp: SpinRwLock<Arc<Vec<String>>>,
    auxv: SpinRwLock<Vec<AuxEntry>>,
    root_path: SpinRwLock<String>,
    cwd_path: SpinRwLock<String>,
}

impl ProcessImageState {
    pub(super) fn new(image: ProcessImage) -> Self {
        Self {
            exe_path: SpinRwLock::new(image.exe_path),
            cmdline: SpinRwLock::new(image.cmdline),
            envp: SpinRwLock::new(image.envp),
            auxv: SpinRwLock::new(image.auxv),
            root_path: SpinRwLock::new(image.root_path),
            cwd_path: SpinRwLock::new(image.cwd_path),
        }
    }
}

impl ProcessData {
    pub fn exe_path(&self) -> SpinRwLockReadGuard<'_, String> {
        self.image.exe_path.read()
    }

    pub fn set_exe_path(&self, path: String) {
        *self.image.exe_path.write() = path;
    }

    pub fn cmdline(&self) -> SpinRwLockReadGuard<'_, Arc<Vec<String>>> {
        self.image.cmdline.read()
    }

    pub fn set_cmdline(&self, cmdline: Arc<Vec<String>>) {
        *self.image.cmdline.write() = cmdline;
    }

    pub fn envp(&self) -> SpinRwLockReadGuard<'_, Arc<Vec<String>>> {
        self.image.envp.read()
    }

    pub fn set_envp(&self, envp: Arc<Vec<String>>) {
        *self.image.envp.write() = envp;
    }

    pub fn auxv(&self) -> SpinRwLockReadGuard<'_, Vec<AuxEntry>> {
        self.image.auxv.read()
    }

    pub fn set_auxv(&self, auxv: Vec<AuxEntry>) {
        *self.image.auxv.write() = auxv;
    }

    pub fn root_path(&self) -> SpinRwLockReadGuard<'_, String> {
        self.image.root_path.read()
    }

    pub fn set_root_path(&self, path: String) {
        *self.image.root_path.write() = path;
    }

    pub fn cwd_path(&self) -> SpinRwLockReadGuard<'_, String> {
        self.image.cwd_path.read()
    }

    pub fn set_cwd_path(&self, path: String) {
        *self.image.cwd_path.write() = path;
    }
}
