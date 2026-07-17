#[cfg(all(feature = "net", feature = "fs"))]
pub(crate) struct AxFsUnixNamespace;

#[cfg(all(feature = "net", feature = "fs"))]
impl ax_net::unix::UnixNamespace for AxFsUnixNamespace {
    fn resolve(&self, path: &str) -> ax_errno::AxResult<alloc::sync::Arc<ax_net::unix::BindSlot>> {
        use ax_errno::AxError;
        use ax_fs_ng::vfs::{OpenOptions, current_fs_context};
        use axfs_ng_vfs::NodeType;

        let fs_context = current_fs_context();
        let opened = OpenOptions::new()
            .read(true)
            .path(true)
            .open(&fs_context.lock(), path)?;
        opened.with_operation(|node| {
            if node.metadata()?.node_type != NodeType::Socket {
                return Err(AxError::NotASocket);
            }
            node.get_user_data::<ax_net::unix::BindSlot>()
                .ok_or(AxError::ConnectionRefused)
        })
    }

    fn reserve_bind(&self, path: &str) -> ax_errno::AxResult<ax_net::unix::NamespaceBindSlot> {
        use ax_errno::AxError;
        use ax_fs_ng::vfs::{OpenOptions, current_fs_context};
        use axfs_ng_vfs::NodeType;

        let fs_context = current_fs_context();
        let created = OpenOptions::new()
            .write(true)
            .create_new(true)
            .node_type(NodeType::Socket)
            .open(&fs_context.lock(), path);
        let (opened, created) = match created {
            Ok(opened) => (opened, true),
            Err(AxError::AlreadyExists) => (
                OpenOptions::new()
                    .write(true)
                    .create(true)
                    .node_type(NodeType::Socket)
                    .open(&fs_context.lock(), path)?,
                false,
            ),
            Err(error) => return Err(error),
        };

        opened.with_operation(|node| {
            if node.metadata()?.node_type != NodeType::Socket {
                return Err(AxError::NotASocket);
            }
            let slot = node.get_or_insert_user_data_with(Default::default);
            Ok(if created {
                ax_net::unix::NamespaceBindSlot::created(slot)
            } else {
                ax_net::unix::NamespaceBindSlot::preexisting(slot)
            })
        })
    }

    fn rollback_bind(&self, path: &str) -> ax_errno::AxResult<()> {
        use ax_fs_ng::vfs::current_fs_context;

        let fs_context = current_fs_context();
        fs_context.lock().remove_file(path)
    }
}
