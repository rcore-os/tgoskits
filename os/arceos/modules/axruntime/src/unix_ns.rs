#[cfg(all(feature = "net", feature = "fs-ng"))]
pub(crate) struct AxFsUnixNamespace;

#[cfg(all(feature = "net", feature = "fs-ng"))]
impl ax_net::unix::UnixNamespace for AxFsUnixNamespace {
    fn resolve(&self, path: &str) -> ax_errno::AxResult<alloc::sync::Arc<ax_net::unix::BindSlot>> {
        use ax_errno::AxError;
        use ax_fs_ng::FS_CONTEXT;
        use axfs_ng_vfs::NodeType;

        let loc = FS_CONTEXT.lock().resolve(path)?;
        if loc.metadata()?.node_type != NodeType::Socket {
            return Err(AxError::NotASocket);
        }
        loc.user_data()
            .get::<ax_net::unix::BindSlot>()
            .ok_or(ax_errno::AxError::ConnectionRefused)
    }

    fn bind(&self, path: &str) -> ax_errno::AxResult<alloc::sync::Arc<ax_net::unix::BindSlot>> {
        use ax_errno::AxError;
        use ax_fs_ng::{FS_CONTEXT, OpenOptions};
        use axfs_ng_vfs::NodeType;

        let loc = OpenOptions::new()
            .write(true)
            .create(true)
            .node_type(NodeType::Socket)
            .open(&FS_CONTEXT.lock(), path)?
            .into_location();

        if loc.metadata()?.node_type != NodeType::Socket {
            return Err(AxError::NotASocket);
        }

        Ok(loc.user_data().get_or_insert_with(Default::default))
    }

    fn unbind(&self, path: &str) -> ax_errno::AxResult<()> {
        use ax_fs_ng::FS_CONTEXT;
        FS_CONTEXT.lock().remove_file(path)
    }
}
