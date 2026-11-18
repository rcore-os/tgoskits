//! Lock-external copying through the existing canonical native page caches.

use axfs_ng_vfs::{NodeFlags, StagingNode, VfsError, VfsResult};

use super::FileBackend;

/// Copies the factory-selected native source into its new staging inode.
///
/// The borrowed stage retains the original full write preparation. This does
/// not open as root, acquire another graph, or publish a logical name. Close
/// every cache endpoint before metadata restoration, sealing, or cleanup.
/// Composite sources/targets select each actual backing with its saved mounter
/// before the native endpoint opens. All intermediate path uses survive I/O.
pub fn copy_staging_data(stage: &StagingNode) -> VfsResult<()> {
    let Some(io) = stage.native_data()? else {
        return Ok(());
    };
    let backend = |location: &axfs_ng_vfs::Location| {
        let flags = location.flags();
        if !flags.contains(NodeFlags::NON_CACHEABLE) || flags.contains(NodeFlags::ALWAYS_CACHE) {
            FileBackend::new_cached(location.clone())
        } else {
            FileBackend::new_direct(location.clone())
        }
    };
    let source = backend(io.source())?;
    let target = backend(io.target())?;
    let mut offset = 0;
    let mut buffer = [0; 4096];
    while offset < io.len() {
        let limit = (io.len() - offset).min(buffer.len() as u64) as usize;
        let read = source.read_at(&mut buffer[..limit], offset)?;
        if read == 0 {
            break;
        }
        let mut written = 0;
        while written < read {
            let count = target.write_at(&buffer[written..read], offset + written as u64)?;
            if count == 0 {
                return Err(VfsError::InvalidData);
            }
            written += count;
        }
        offset += read as u64;
    }
    target.set_len(io.len())?;
    // Native metadata must agree before source timestamps are restored.
    target.sync(false)
}
