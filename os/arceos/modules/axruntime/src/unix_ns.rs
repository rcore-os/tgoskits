#[cfg(all(feature = "net", feature = "fs"))]
pub(crate) struct AxFsUnixNamespace;

#[cfg(all(feature = "net", feature = "fs"))]
impl ax_net::unix::UnixNamespace for AxFsUnixNamespace {
    fn resolve(&self, path: &str) -> ax_errno::AxResult<alloc::sync::Arc<ax_net::unix::BindSlot>> {
        use ax_errno::AxError;
        use axfs_ng_vfs::NodeType;

        let loc = ax_fs_ng::vfs::current_fs_context().lock().resolve(path)?;
        if loc.metadata()?.node_type != NodeType::Socket {
            return Err(AxError::NotASocket);
        }
        loc.user_data()
            .get::<ax_net::unix::BindSlot>()
            .ok_or(ax_errno::AxError::ConnectionRefused)
    }

    fn bind(&self, path: &str) -> ax_errno::AxResult<alloc::sync::Arc<ax_net::unix::BindSlot>> {
        use ax_errno::AxError;
        use ax_fs_ng::vfs::OpenOptions;
        use axfs_ng_vfs::NodeType;

        let loc = OpenOptions::new()
            .write(true)
            .create(true)
            .node_type(NodeType::Socket)
            .open(&ax_fs_ng::vfs::current_fs_context().lock(), path)?
            .into_location();

        if loc.metadata()?.node_type != NodeType::Socket {
            return Err(AxError::NotASocket);
        }

        Ok(loc.user_data().get_or_insert_with(Default::default))
    }

    fn unbind(&self, path: &str) -> ax_errno::AxResult<()> {
        ax_fs_ng::vfs::current_fs_context().lock().remove_file(path)
    }
}
