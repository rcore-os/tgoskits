#[cfg(all(feature = "net", feature = "fs"))]
pub(crate) struct AxFsUnixNamespace;

#[cfg(all(feature = "net", feature = "fs"))]
impl ax_net::unix::UnixNamespace for AxFsUnixNamespace {
    fn resolve(&self, path: &str) -> ax_net::NetResult<alloc::sync::Arc<ax_net::unix::BindSlot>> {
        use axfs_ng_vfs::NodeType;

        let loc = ax_fs_ng::vfs::current_fs_context()
            .lock()
            .resolve(path)
            .map_err(namespace_vfs_error)?;
        if loc.metadata().map_err(namespace_vfs_error)?.node_type != NodeType::Socket {
            return Err(ax_net::NetError::NotASocket);
        }
        loc.user_data()
            .get::<ax_net::unix::BindSlot>()
            .ok_or(ax_net::NetError::ConnectionRefused)
    }

    fn bind(&self, path: &str) -> ax_net::NetResult<alloc::sync::Arc<ax_net::unix::BindSlot>> {
        use ax_fs_ng::vfs::OpenOptions;
        use axfs_ng_vfs::NodeType;

        let loc = OpenOptions::new()
            .write(true)
            .create(true)
            .node_type(NodeType::Socket)
            .open(&ax_fs_ng::vfs::current_fs_context().lock(), path)
            .map_err(namespace_vfs_error)?
            .into_location();

        if loc.metadata().map_err(namespace_vfs_error)?.node_type != NodeType::Socket {
            return Err(ax_net::NetError::NotASocket);
        }

        Ok(loc.user_data().get_or_insert_with(Default::default))
    }

    fn unbind(&self, path: &str) -> ax_net::NetResult<()> {
        ax_fs_ng::vfs::current_fs_context()
            .lock()
            .remove_file(path)
            .map_err(namespace_vfs_error)
    }
}

#[cfg(all(feature = "net", feature = "fs"))]
fn namespace_vfs_error(error: axfs_ng_vfs::VfsError) -> ax_net::NetError {
    use ax_net::NetError;
    use axfs_ng_vfs::VfsError;

    match error {
        VfsError::AlreadyExists => NetError::AlreadyExists,
        VfsError::BadAddress => NetError::BadAddress,
        VfsError::BadFileDescriptor => NetError::BadFileDescriptor,
        VfsError::BadState => NetError::BadState,
        VfsError::CrossesDevices => NetError::CrossesDevices,
        // Unix socket path operations cannot expose the filesystem-only
        // categories through `NetError`; degrade only at this namespace edge.
        VfsError::DataMissing => NetError::InvalidData,
        VfsError::DirectoryNotEmpty => NetError::DirectoryNotEmpty,
        VfsError::FilesystemCorrupted => NetError::InvalidData,
        VfsError::FilesystemLoop => NetError::FilesystemLoop,
        VfsError::FileTooLarge => NetError::FileTooLarge,
        VfsError::InvalidData => NetError::InvalidData,
        VfsError::InvalidInput => NetError::InvalidInput,
        VfsError::Interrupted => ax_task::future::Interrupted.into(),
        VfsError::Io => NetError::BackendIo,
        VfsError::IsADirectory => NetError::IsADirectory,
        VfsError::NameTooLong => NetError::NameTooLong,
        VfsError::NoMemory => NetError::NoMemory,
        VfsError::NoSuchDevice => NetError::NoSuchDevice,
        VfsError::NoSuchDeviceOrAddress => NetError::NoSuchDeviceOrAddress,
        VfsError::NotADirectory => NetError::NotADirectory,
        VfsError::NotATty => NetError::NotATty,
        VfsError::NotFound => NetError::NotFound,
        VfsError::OperationNotPermitted => NetError::OperationNotPermitted,
        VfsError::OperationNotSupported => NetError::OperationNotSupported,
        VfsError::PermissionDenied => NetError::PermissionDenied,
        VfsError::QuotaExceeded => NetError::StorageFull,
        VfsError::ReadOnlyFilesystem => NetError::ReadOnlyFilesystem,
        VfsError::ResourceBusy => NetError::ResourceBusy,
        VfsError::StorageFull => NetError::StorageFull,
        VfsError::TimedOut => NetError::TimedOut,
        VfsError::TooManyLinks => NetError::OperationNotSupported,
        VfsError::Unsupported => NetError::Unsupported,
        VfsError::ValueOverflow => NetError::InvalidInput,
        VfsError::WouldBlock => NetError::WouldBlock,
    }
}
