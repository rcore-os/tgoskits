//! Construction-time ownership of process-scoped filesystem resources.

use alloc::sync::Arc;

use ax_errno::{AxError, AxResult};
use ax_fs_ng::vfs::{FS_CONTEXT, FsContext, OpenOptions, ROOT_FS_CONTEXT};
use ax_kspin::SpinRwLock as RwLock;
use ax_sync::PiMutex;
use flatten_objects::FlattenObjects;
use linux_raw_sys::general::{O_RDONLY, O_WRONLY};
use scope_local::ScopeCell;

use super::{FD_TABLE, File, FileDescriptor, FileDescriptorTable};

/// A complete resource scope that has never been visible to the scheduler.
///
/// The private field and consuming accessor make it impossible to construct a
/// [`crate::task::ProcessData`] with only one of its FD-table and filesystem
/// owners installed. All allocation and filesystem I/O happens before this
/// value is moved into the process.
pub(crate) struct PreparedProcessScope {
    scope: ScopeCell,
}

impl PreparedProcessScope {
    /// Builds PID 1's filesystem context and standard descriptors off-line.
    ///
    /// # Errors
    ///
    /// Returns [`AxError::BadState`] before constructing the process if the
    /// root filesystem context has not been published. Console lookup or file
    /// descriptor admission errors are returned without exposing the partial
    /// scope.
    pub(crate) fn prepare_init() -> AxResult<Self> {
        ensure_root_context_is_published()?;

        let mut scope = ScopeCell::new();
        let fs_context = {
            let slot = FS_CONTEXT.scope_cell_mut_unpublished(&mut scope);
            Arc::clone(&slot)
        };
        let fd_table = prepare_stdio_fd_table(&fs_context)?;
        *FD_TABLE.scope_cell_mut_unpublished(&mut scope) = fd_table;

        Ok(Self { scope })
    }

    /// Assembles inherited FD and filesystem owners for a forked process.
    pub(crate) fn from_resources(
        fd_table: FileDescriptorTable,
        fs_context: Arc<PiMutex<FsContext>>,
    ) -> Self {
        let mut scope = ScopeCell::new();
        *FD_TABLE.scope_cell_mut_unpublished(&mut scope) = fd_table;
        *FS_CONTEXT.scope_cell_mut_unpublished(&mut scope) = fs_context;
        Self { scope }
    }

    /// Consumes the proof and transfers its complete scope into ProcessData.
    pub(crate) fn into_scope(self) -> ScopeCell {
        self.scope
    }
}

fn ensure_root_context_is_published() -> AxResult<()> {
    match ROOT_FS_CONTEXT.snapshot() {
        Some(_) => Ok(()),
        None => Err(AxError::BadState),
    }
}

fn prepare_stdio_fd_table(fs_context: &Arc<PiMutex<FsContext>>) -> AxResult<FileDescriptorTable> {
    let cx = fs_context.lock();
    let open = |options: &mut OpenOptions, flags| {
        AxResult::Ok(Arc::new(File::new(
            options.open(&cx, "/dev/console")?.into_file()?,
            flags,
        )))
    };

    let tty_in = open(OpenOptions::new().read(true).write(false), O_RDONLY as _)?;
    let tty_out = open(OpenOptions::new().read(false).write(true), O_WRONLY as _)?;
    let mut fd_table = FlattenObjects::new();
    fd_table
        .add(FileDescriptor {
            inner: tty_in,
            cloexec: false,
        })
        .map_err(|_| AxError::TooManyOpenFiles)?;
    fd_table
        .add(FileDescriptor {
            inner: tty_out.clone(),
            cloexec: false,
        })
        .map_err(|_| AxError::TooManyOpenFiles)?;
    fd_table
        .add(FileDescriptor {
            inner: tty_out,
            cloexec: false,
        })
        .map_err(|_| AxError::TooManyOpenFiles)?;

    Ok(Arc::new(RwLock::new(fd_table)))
}
