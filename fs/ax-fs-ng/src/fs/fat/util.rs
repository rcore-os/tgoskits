use core::time::Duration;

use axfs_ng_vfs::{DeviceId, Metadata, NodePermission, NodeType, VfsError};
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Timelike, Utc};

use super::{ff, fs::FatFilesystemInner};

// Inclusive UTC endpoints of the FAT timestamp range.
const FAT_MIN_TIME: Duration = Duration::from_secs(315_532_800); // 1980-01-01 00:00:00
const FAT_MAX_TIME: Duration = Duration::from_secs(4_354_819_199); // 2107-12-31 23:59:59

pub fn dos_to_unix(datetime: fatfs::DateTime) -> Duration {
    // let date: NaiveDateTime = date.into();
    let Some(date) = NaiveDate::from_ymd_opt(
        datetime.date.year as _,
        datetime.date.month as _,
        datetime.date.day as _,
    ) else {
        return Duration::default();
    };
    let Some(date) = date.and_hms_milli_opt(
        datetime.time.hour as _,
        datetime.time.min as _,
        datetime.time.sec as _,
        datetime.time.millis as _,
    ) else {
        return Duration::default();
    };
    let Some(datetime) = Utc.from_local_datetime(&date).single() else {
        return Duration::default();
    };
    datetime
        .signed_duration_since(DateTime::UNIX_EPOCH)
        .to_std()
        .unwrap_or_default()
}

pub fn unix_to_dos(datetime: Duration) -> fatfs::DateTime {
    // Bound the whole instant before chrono arithmetic, including Duration::MAX.
    let dt = DateTime::UNIX_EPOCH + datetime.clamp(FAT_MIN_TIME, FAT_MAX_TIME);
    let dt = dt.naive_utc();

    fatfs::DateTime::new(
        fatfs::Date::new(dt.year() as _, dt.month() as _, dt.day() as _),
        fatfs::Time::new(dt.hour() as _, dt.minute() as _, (dt.second() & !1) as _, 0),
    )
}

pub fn file_metadata(fs: &FatFilesystemInner, file: &ff::File, node_type: NodeType) -> Metadata {
    let size = file.size().unwrap_or(0) as u64;
    let block_size = fs.inner.bytes_per_sector();
    let mtime = dos_to_unix(file.modified());
    Metadata {
        // TODO: inode
        inode: 1,
        device: 0,
        nlink: 1,
        mode: NodePermission::default(),
        node_type,
        uid: 0,
        gid: 0,
        size,
        block_size: block_size as _,
        // TODO: The correct block count should be obtained from
        // `file.extents()`. However it would be costly. This implementation
        // would be enough for now.
        blocks: size / block_size as u64,
        rdev: DeviceId::default(),
        atime: dos_to_unix(fatfs::DateTime::new(
            file.accessed(),
            fatfs::Time::new(0, 0, 0, 0),
        )),
        mtime,
        ctime: mtime,
    }
}

pub fn update_file_times(file: &mut ff::File, atime: Option<Duration>, mtime: Option<Duration>) {
    // Normalize all participating fields before the first editor mutation.
    let atime = atime.map(|time| unix_to_dos(time).date);
    let mtime = mtime.map(unix_to_dos);
    if let Some(atime) = atime {
        #[allow(deprecated)]
        file.set_accessed(atime);
    }
    if let Some(mtime) = mtime {
        #[allow(deprecated)]
        file.set_modified(mtime);
    }
}

pub fn into_vfs_err<E>(err: fatfs::Error<E>) -> VfsError {
    use fatfs::Error::*;
    match err {
        AlreadyExists => VfsError::AlreadyExists,
        CorruptedFileSystem => VfsError::InvalidData,
        DirectoryIsNotEmpty => VfsError::DirectoryNotEmpty,
        InvalidFileNameLength => VfsError::NameTooLong,
        InvalidInput => VfsError::InvalidInput,
        UnsupportedFileNameCharacter => VfsError::InvalidData,
        NotEnoughSpace => VfsError::StorageFull,
        NotFound => VfsError::NotFound,
        UnexpectedEof | WriteZero => VfsError::Io,
        _ => VfsError::Io,
    }
}
