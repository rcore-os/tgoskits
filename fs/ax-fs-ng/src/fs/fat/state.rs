//! Shared FAT inode holders, weakly rediscoverable independently of dentries.

use alloc::{borrow::ToOwned, string::String, sync::Arc, vec};
use core::mem;

use axfs_ng_vfs::{NodeIdentity, VfsError, VfsResult};
use fatfs::Write;

use super::{
    FsRef, ff,
    fs::{FatFilesystem, FatFilesystemGuard},
    util::into_vfs_err,
};

pub(super) enum FatHandle {
    File(ff::File<'static>),
    Dir(ff::Dir<'static>),
}

pub(super) struct FatName {
    pub parent: Option<Arc<FatInode>>,
    pub canonical: String,
    pub trusted: bool,
}

pub(super) struct FatInode {
    pub fs: Arc<FatFilesystem>,
    pub inode: u64,
    pub generation: u64,
    pub handle: FsRef<Option<FatHandle>>,
    pub name: FsRef<FatName>,
}

// SAFETY: fatfs is accessed only through a matching filesystem mutex guard,
// including final handle destruction. The lifetime-erased handles are private;
// their owning Arc<FatFilesystem> outlives every access and handle destructor.
unsafe impl Send for FatInode {}
// SAFETY: FsRef requires a matching guard for reads and its exclusive borrow for
// writes. Neither the raw fatfs handle nor an unchecked mutable cell is public.
unsafe impl Sync for FatInode {}

impl FatInode {
    pub fn canonical_name(entry: &ff::DirEntry<'_>) -> VfsResult<String> {
        if let Some(name) = entry.long_file_name_as_ucs2_units() {
            return String::from_utf16(name).map_err(|_| VfsError::InvalidData);
        }
        // The configured LossyOemCpConverter cannot distinguish non-ASCII SFNs.
        // Do not turn two lossy displays into one logical inode.
        if !entry.short_file_name_as_bytes().is_ascii() {
            return Err(VfsError::OperationNotSupported);
        }
        Ok(entry.file_name())
    }

    pub fn identity(&self) -> NodeIdentity {
        NodeIdentity::new(vec![
            Arc::as_ptr(&self.fs) as usize as u64,
            self.inode,
            self.generation,
        ])
    }

    pub fn root(fs: Arc<FatFilesystem>, dir: ff::Dir<'_>, inode: u64) -> Arc<Self> {
        // SAFETY: this private constructor receives a root handle from fs after
        // fs has been placed in its Arc allocation. fs.inner is never replaced.
        // Drop takes and destroys the handle before releasing the owning Arc.
        let dir = unsafe { mem::transmute::<ff::Dir<'_>, ff::Dir<'static>>(dir) };
        Arc::new(Self {
            handle: FsRef::new(&fs, Some(FatHandle::Dir(dir))),
            name: FsRef::new(
                &fs,
                FatName {
                    parent: None,
                    canonical: String::new(),
                    trusted: true,
                },
            ),
            fs,
            inode,
            generation: 0,
        })
    }

    pub fn get(
        parent: &Arc<Self>,
        entry: ff::DirEntry<'_>,
        guard: &mut FatFilesystemGuard<'_>,
    ) -> VfsResult<Arc<Self>> {
        let canonical = Self::canonical_name(&entry)?;
        let key = (parent.inode, canonical.clone());
        if let Some(inode) = guard.active.get(&key).and_then(alloc::sync::Weak::upgrade) {
            guard.retain(&inode);
            if !inode.name.borrow(guard).trusted {
                return Err(VfsError::BadState);
            }
            return Ok(inode);
        }
        let (inode, generation) = guard.alloc_identity()?;
        if generation.is_multiple_of(64) {
            guard.active.retain(|_, inode| inode.strong_count() != 0);
        }
        let handle = if entry.is_dir() {
            // SAFETY: entry was obtained through parent's guarded directory in
            // this fs; the new holder retains fs and parent, and destroys the
            // erased handle under this same owner mutex before either is freed.
            FatHandle::Dir(unsafe {
                mem::transmute::<ff::Dir<'_>, ff::Dir<'static>>(entry.to_dir())
            })
        } else {
            // SAFETY: the same owner and destruction proof applies to File.
            FatHandle::File(unsafe {
                mem::transmute::<ff::File<'_>, ff::File<'static>>(entry.to_file())
            })
        };
        let fs = parent.fs.clone();
        let result = Arc::new(Self {
            handle: FsRef::new(&fs, Some(handle)),
            name: FsRef::new(
                &fs,
                FatName {
                    parent: Some(parent.clone()),
                    canonical,
                    trusted: true,
                },
            ),
            fs,
            inode,
            generation,
        });
        guard.active.insert(key, Arc::downgrade(&result));
        guard.retain(&result);
        Ok(result)
    }

    pub fn directory(&self, guard: &FatFilesystemGuard<'_>) -> VfsResult<ff::Dir<'static>> {
        match self.handle.borrow(guard).as_ref() {
            Some(FatHandle::Dir(dir)) => Ok(dir.clone()),
            _ => Err(VfsError::BadState),
        }
    }

    pub fn close_handle(&self, guard: &mut FatFilesystemGuard<'_>) -> VfsResult<()> {
        match self.handle.borrow_mut(guard).as_mut() {
            Some(FatHandle::File(file)) => file.flush().map_err(into_vfs_err)?,
            // The stored Dir is only cloned, never mutated. Every clone and its
            // editor is destroyed under this guard before the original is closed.
            Some(FatHandle::Dir(_)) => {}
            _ => return Err(VfsError::OperationNotSupported),
        }
        // The editor is now clean; Drop cannot write a stale directory slot.
        drop(self.handle.borrow_mut(guard).take());
        Ok(())
    }

    pub fn invalidate_name(&self, guard: &mut FatFilesystemGuard<'_>) {
        let key = {
            let name = self.name.borrow(guard);
            name.parent
                .as_ref()
                .map(|parent| (parent.inode, name.canonical.clone()))
        };
        if let Some(key) = key
            && guard
                .active
                .get(&key)
                .is_some_and(|inode| core::ptr::eq(inode.as_ptr(), self))
        {
            guard.active.remove(&key);
        }
        self.name.borrow_mut(guard).trusted = false;
    }

    pub fn move_handle(
        self: &Arc<Self>,
        parent: Arc<Self>,
        entry: ff::DirEntry<'_>,
        guard: &mut FatFilesystemGuard<'_>,
    ) {
        let canonical = entry.file_name();
        let handle = if entry.is_dir() {
            // SAFETY: the destination entry is obtained under the matching
            // guard, fs is retained, and final destruction precedes fs release.
            FatHandle::Dir(unsafe {
                mem::transmute::<ff::Dir<'_>, ff::Dir<'static>>(entry.to_dir())
            })
        } else {
            // SAFETY: the same ownership/lifetime proof applies to File.
            FatHandle::File(unsafe {
                mem::transmute::<ff::File<'_>, ff::File<'static>>(entry.to_file())
            })
        };
        *self.handle.borrow_mut(guard) = Some(handle);
        let old = {
            let name = self.name.borrow_mut(guard);
            mem::replace(
                name,
                FatName {
                    parent: Some(parent),
                    canonical: canonical.clone(),
                    trusted: true,
                },
            )
        };
        if let Some(parent) = &old.parent {
            guard
                .active
                .remove(&(parent.inode, old.canonical.to_owned()));
        }
        let parent_inode = self
            .name
            .borrow(guard)
            .parent
            .as_ref()
            .expect("linked FAT parent")
            .inode;
        guard
            .active
            .insert((parent_inode, canonical), Arc::downgrade(self));
        // Guard retention makes dropping an ancestor safe before unlock.
        if let Some(parent) = &old.parent {
            guard.retain(parent);
        }
        drop(old);
    }
}

impl Drop for FatInode {
    fn drop(&mut self) {
        let mut guard = self.fs.lock();
        let handle = self.handle.borrow_mut(&mut guard).take();
        // fatfs File::drop may flush. Keep its FileSystem and mutex alive.
        drop(handle);
        let parent = self.name.borrow_mut(&mut guard).parent.take();
        guard.release_inode(self.inode);
        drop(guard);
        drop(parent);
    }
}
