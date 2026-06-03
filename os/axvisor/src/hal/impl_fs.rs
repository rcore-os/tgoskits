use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_errno::{AxResult, ax_err_type};
use ax_kspin::SpinNoIrq;
use axvisor_api::fs::{DirEntry, FileType, FsIf, Metadata};

#[cfg(feature = "fs")]
use std::fs;
use std::io::{self, Read, Write};
use std::sync::Mutex;

#[cfg(feature = "fs")]
static FILE_IDS: AtomicUsize = AtomicUsize::new(1);
#[cfg(feature = "fs")]
static FILES: SpinNoIrq<BTreeMap<usize, Arc<Mutex<fs::File>>>> = SpinNoIrq::new(BTreeMap::new());

fn map_io_err<T>(res: io::Result<T>) -> AxResult<T> {
    res.map_err(|err| ax_err_type!(Io, err.to_string()))
}

#[cfg(feature = "fs")]
fn get_file(file: usize) -> AxResult<Arc<Mutex<fs::File>>> {
    FILES
        .lock()
        .get(&file)
        .cloned()
        .ok_or_else(|| ax_err_type!(NotFound, "file handle not found"))
}

#[cfg(feature = "fs")]
fn map_file_type(file_type: fs::FileType) -> FileType {
    if file_type.is_dir() {
        FileType::Dir
    } else if file_type.is_file() {
        FileType::File
    } else {
        FileType::Other
    }
}

#[cfg(feature = "fs")]
fn map_metadata(metadata: fs::Metadata) -> Metadata {
    Metadata::new(
        metadata.len(),
        map_file_type(metadata.file_type()),
        metadata.permissions().mode(),
    )
}

struct FsImpl;

#[axvisor_api::api_impl]
impl FsIf for FsImpl {
    fn open_file(path: &str) -> AxResult<usize> {
        #[cfg(feature = "fs")]
        {
            let file = map_io_err(fs::File::open(path))?;
            let id = FILE_IDS.fetch_add(1, Ordering::Relaxed);
            FILES.lock().insert(id, Arc::new(Mutex::new(file)));
            Ok(id)
        }
        #[cfg(not(feature = "fs"))]
        {
            let _ = path;
            Err(ax_err_type!(Unsupported, "filesystem support is disabled"))
        }
    }

    fn create_file(path: &str) -> AxResult<usize> {
        #[cfg(feature = "fs")]
        {
            let file = map_io_err(fs::File::create(path))?;
            let id = FILE_IDS.fetch_add(1, Ordering::Relaxed);
            FILES.lock().insert(id, Arc::new(Mutex::new(file)));
            Ok(id)
        }
        #[cfg(not(feature = "fs"))]
        {
            let _ = path;
            Err(ax_err_type!(Unsupported, "filesystem support is disabled"))
        }
    }

    fn close_file(file: usize) {
        #[cfg(feature = "fs")]
        {
            FILES.lock().remove(&file);
        }
    }

    fn file_metadata(file: usize) -> AxResult<Metadata> {
        #[cfg(feature = "fs")]
        {
            let file = get_file(file)?;
            let file = file.lock();
            map_io_err(file.metadata()).map(map_metadata)
        }
        #[cfg(not(feature = "fs"))]
        {
            let _ = file;
            Err(ax_err_type!(Unsupported, "filesystem support is disabled"))
        }
    }

    fn file_read(file: usize, buf: &mut [u8]) -> AxResult<usize> {
        #[cfg(feature = "fs")]
        {
            let file = get_file(file)?;
            let mut file = file.lock();
            map_io_err(file.read(buf))
        }
        #[cfg(not(feature = "fs"))]
        {
            let _ = (file, buf);
            Err(ax_err_type!(Unsupported, "filesystem support is disabled"))
        }
    }

    fn file_write(file: usize, buf: &[u8]) -> AxResult<usize> {
        #[cfg(feature = "fs")]
        {
            let file = get_file(file)?;
            let mut file = file.lock();
            map_io_err(file.write(buf))
        }
        #[cfg(not(feature = "fs"))]
        {
            let _ = (file, buf);
            Err(ax_err_type!(Unsupported, "filesystem support is disabled"))
        }
    }

    fn file_flush(file: usize) -> AxResult<()> {
        #[cfg(feature = "fs")]
        {
            let file = get_file(file)?;
            let mut file = file.lock();
            map_io_err(file.flush())
        }
        #[cfg(not(feature = "fs"))]
        {
            let _ = file;
            Err(ax_err_type!(Unsupported, "filesystem support is disabled"))
        }
    }

    fn path_metadata(path: &str) -> AxResult<Metadata> {
        #[cfg(feature = "fs")]
        {
            map_io_err(fs::metadata(path)).map(map_metadata)
        }
        #[cfg(not(feature = "fs"))]
        {
            let _ = path;
            Err(ax_err_type!(Unsupported, "filesystem support is disabled"))
        }
    }

    fn fs_read_dir(path: &str) -> AxResult<Vec<DirEntry>> {
        #[cfg(feature = "fs")]
        {
            let mut entries = Vec::new();
            for entry in map_io_err(fs::read_dir(path))? {
                let entry = map_io_err(entry)?;
                let entry_path = entry.path();
                let file_name = entry.file_name();
                let file_name = file_name.to_string();
                let file_type = map_file_type(entry.file_type());
                entries.push(DirEntry::new(
                    file_name,
                    entry_path.as_str().to_string(),
                    file_type,
                ));
            }

            Ok(entries)
        }
        #[cfg(not(feature = "fs"))]
        {
            let _ = path;
            Err(ax_err_type!(Unsupported, "filesystem support is disabled"))
        }
    }

    fn fs_create_dir(path: &str) -> AxResult<()> {
        #[cfg(feature = "fs")]
        {
            map_io_err(fs::create_dir(path))
        }
        #[cfg(not(feature = "fs"))]
        {
            let _ = path;
            Err(ax_err_type!(Unsupported, "filesystem support is disabled"))
        }
    }

    fn fs_remove_dir(path: &str) -> AxResult<()> {
        #[cfg(feature = "fs")]
        {
            map_io_err(fs::remove_dir(path))
        }
        #[cfg(not(feature = "fs"))]
        {
            let _ = path;
            Err(ax_err_type!(Unsupported, "filesystem support is disabled"))
        }
    }

    fn fs_remove_file(path: &str) -> AxResult<()> {
        #[cfg(feature = "fs")]
        {
            map_io_err(fs::remove_file(path))
        }
        #[cfg(not(feature = "fs"))]
        {
            let _ = path;
            Err(ax_err_type!(Unsupported, "filesystem support is disabled"))
        }
    }

    fn fs_rename(from: &str, to: &str) -> AxResult<()> {
        #[cfg(feature = "fs")]
        {
            map_io_err(fs::rename(from, to))
        }
        #[cfg(not(feature = "fs"))]
        {
            let _ = (from, to);
            Err(ax_err_type!(Unsupported, "filesystem support is disabled"))
        }
    }

    fn fs_current_dir() -> AxResult<String> {
        #[cfg(feature = "fs")]
        {
            map_io_err(std::env::current_dir())
        }
        #[cfg(not(feature = "fs"))]
        {
            Err(ax_err_type!(Unsupported, "filesystem support is disabled"))
        }
    }

    fn fs_set_current_dir(path: &str) -> AxResult<()> {
        #[cfg(feature = "fs")]
        {
            map_io_err(std::env::set_current_dir(path))
        }
        #[cfg(not(feature = "fs"))]
        {
            let _ = path;
            Err(ax_err_type!(Unsupported, "filesystem support is disabled"))
        }
    }
}
