//! Native inode paths selected from retained open descriptions.

use alloc::sync::Arc;

use axfs_ng_vfs::{PathHandle, VfsError, VfsResult};

use super::{
    Directory, File, FileLike, MountTableFile, NamespaceObject, NsFd, PacketSocket, Pipe, Socket,
    memfd::Memfd, netlink::NetlinkSocket,
};

/// Retains the selected mount use without interpreting the display path.
///
/// The caller retains the description while selecting its path. Object-vnode
/// creation may sleep; call outside descriptor-table and backend locks. This
/// ordinary projection is not a nonblocking CACHED lookup capability.
pub(crate) fn descriptor_path(file: &Arc<dyn FileLike>) -> VfsResult<PathHandle> {
    if let Some(file) = file.downcast_ref::<File>() {
        return Ok(file.inner().path_handle().clone());
    }
    if let Some(directory) = file.downcast_ref::<Directory>() {
        return Ok(directory.path_handle().clone());
    }
    if let Some(memfd) = file.downcast_ref::<Memfd>() {
        return Ok(memfd.inner().inner().path_handle().clone());
    }
    if let Some(table) = file.downcast_ref::<MountTableFile>() {
        return Ok(table.inner().inner().path_handle().clone());
    }
    let location = if let Some(pipe) = file.downcast_ref::<Pipe>() {
        if let Some(named) = pipe.named_file() {
            return Ok(named.inner().path_handle().clone());
        }
        pipe.location()
    } else if let Some(socket) = file.downcast_ref::<Socket>() {
        socket.location()
    } else if let Some(socket) = file.downcast_ref::<PacketSocket>() {
        socket.location()
    } else if let Some(socket) = file.downcast_ref::<NetlinkSocket>() {
        socket.location()
    } else if let Some(namespace) = file.downcast_ref::<NsFd>() {
        NamespaceObject::from_fd(namespace).location()
    } else {
        return Err(VfsError::OperationNotSupported);
    };
    PathHandle::acquire(location)
}
