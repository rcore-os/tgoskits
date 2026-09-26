//! Files on their way into a guest filesystem.
//!
//! A file becomes usable by a guest config only once it sits at the path the
//! config names, so this module has one job: move bytes there without ever
//! exposing a half-transferred object. It is a *staging* store, not the guest
//! filesystem itself: bytes arrive in chunks into a namespace of our own, and a
//! separate placement step puts the finished file where the config will look
//! for it.
//!
//! # Why the bytes are staged at all
//!
//! The target may be a file a running guest already holds open (its disk, its
//! kernel), and the running guest is the owner of that file, not this plane.
//! Writing into it chunk by chunk would let a guest read a half-written disk.
//! Staging keeps the target untouched until every byte has arrived, and
//! [`place`] then refuses a target that already exists rather than replacing
//! something another guest may be reading. That refusal is also what keeps this
//! plane off a live guest's disk: such a path exists by definition, so a file
//! another guest is using can never be the target of a placement.
//!
//! # Where the truth lives
//!
//! The session table below is memory; the *offset* is not. Every entry point
//! that has to know how many bytes are already on disk asks the file itself
//! (`Metadata::len`), so a client that reconnects — or a control plane that
//! restarted — resumes from what is really there instead of from a counter that
//! promises more than the disk holds. The table therefore only carries what the
//! disk cannot say: which directory a session belongs to, how long it promised
//! to be, and where it has got to in the walk below.
//!
//! # States
//!
//! `uploading` are the bytes still arriving, `uploaded` the bytes that are
//! complete, `placing` the moment the target is being taken, `placed` the file
//! in its final path, and `failed` an attempt that stopped with a readable
//! reason. A full disk is not terminal: the bytes already written stay valid, so
//! a `send` after the operator frees space continues from the same offset.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::sync::LazyLock;

use ax_std::StdError;
use ax_std::fs::OpenOptions;
use ax_std::io::Error as IoError;
use ax_std::io::Write;
use ax_std::os::arceos::sync::NoPreemptMutex;

use super::pool;

/// The largest chunk a client may send in one request.
///
/// It is the request-body limit as well: the transport layer reads one chunk
/// per request, so this number is also the memory a single upload can occupy.
/// One mebibyte keeps a chunk small enough that serving it does not hold the
/// control-plane thread for long, and large enough that the per-request cost
/// stays in the noise.
pub const CHUNK_LIMIT: usize = 1024 * 1024;

/// The largest file this plane will accept, declared or streamed.
///
/// A guest disk image is the biggest thing that travels through here; 256 MiB
/// covers the images this workspace builds and keeps a runaway client from
/// filling the hypervisor's own filesystem.
pub const FILE_LIMIT: usize = 256 * 1024 * 1024;

/// Name of the staging namespace inside the target directory.
///
/// It starts with a dot, so it is not a name a config or a pool file can take
/// (the pool rejects dot-files), and it keeps staged objects out of the listing
/// a person reads while looking for something to boot.
pub const STAGING_DIR: &str = ".files";

/// Longest accepted session identifier, before it becomes a path component.
const SESSION_ID_MAX: usize = 64;

/// One staged object, as the API reports it.
pub struct SessionView {
    pub id: String,
    pub directory: String,
    /// Final name, known once `place` has been asked for it.
    pub name: Option<String>,
    /// Final path, known once the file is `placed`.
    pub path: Option<String>,
    pub total: usize,
    /// Bytes actually on disk, which is the only offset this plane trusts.
    pub written: usize,
    pub state: &'static str,
    pub detail: Option<String>,
}

/// Why an operation could not be carried out.
///
/// The variants name the *kind* of failure, not the status code: mapping a
/// failure to HTTP belongs to the transport layer, which is the layer that
/// speaks HTTP.
pub enum FileError {
    /// The session id is not a name this plane will turn into a path.
    InvalidId(String),
    /// The target directory is not an absolute path inside the guest filesystem.
    InvalidDirectory(String),
    /// The target directory does not exist.
    ///
    /// Nothing creates it on the way in: the operator picks a folder, and a
    /// folder that is not there is a mistake or a step that was skipped. The
    /// explicit operation for making one is [`make_directory`].
    NoSuchDirectory(String),
    /// A directory with that name is already there.
    Exists(String),
    /// The final name is not a plain file name.
    InvalidName(String),
    /// The declared or streamed size exceeds [`FILE_LIMIT`].
    TooLarge { total: usize, limit: usize },
    /// No session by that id.
    Unknown(String),
    /// The request does not fit the state the session is in. `offset` is where
    /// the client should continue from.
    Conflict { reason: String, offset: usize },
    /// The guest filesystem refused to write.
    Unwritable(String),
    /// The guest filesystem is out of space. Recoverable: the bytes already
    /// written stay valid, so the same session can continue after cleanup.
    StorageFull(String),
}

/// Where one session stands.
#[derive(Clone)]
enum State {
    Uploading,
    Uploaded,
    Placing,
    Placed,
    Failed(String),
}

impl State {
    fn name(&self) -> &'static str {
        match self {
            State::Uploading => "uploading",
            State::Uploaded => "uploaded",
            State::Placing => "placing",
            State::Placed => "placed",
            State::Failed(_) => "failed",
        }
    }
}

struct Session {
    directory: String,
    total: usize,
    name: Option<String>,
    path: Option<String>,
    state: State,
}

static SESSIONS: LazyLock<NoPreemptMutex<BTreeMap<String, Session>>> =
    LazyLock::new(|| NoPreemptMutex::new(BTreeMap::new()));

/// Opens a session, or picks up the one an earlier attempt left behind.
///
/// `total` is what the client promises the file will be. A session that already
/// exists keeps the promise it was opened with, and the offset that comes back
/// is always read from the disk: dragging the same file twice therefore resumes
/// instead of starting over, and never truncates bytes that already arrived. To
/// start over deliberately, `drop` the session first.
pub fn open(id: &str, directory: &str, total: usize) -> Result<SessionView, FileError> {
    let id = checked_id(id)?;
    let directory = checked_directory(directory)?;
    if total > FILE_LIMIT {
        return Err(FileError::TooLarge {
            total,
            limit: FILE_LIMIT,
        });
    }

    if ax_std::fs::read_dir(&directory).is_err() {
        return Err(FileError::NoSuchDirectory(directory));
    }
    // The staging namespace is this plane's own, not the operator's tree, so it
    // is the one directory the transfer may create on the way in.
    let staging = staging_dir(&directory);
    pool::ensure_directory(&staging).map_err(FileError::Unwritable)?;
    let path = staging_path(&directory, id);
    // `create` without `truncate`: a resumed upload must find its bytes, and a
    // second `open` for the same id must not throw them away.
    OpenOptions::new()
        .create(true)
        .write(true)
        .open(&path)
        .map_err(|error| map_std_error(&error))?;

    let written = disk_len(&path);
    let mut sessions = SESSIONS.lock();
    let session = sessions.entry(id.to_string()).or_insert_with(|| Session {
        directory: directory.clone(),
        total,
        name: None,
        path: None,
        state: State::Uploading,
    });
    session.state = if written >= session.total {
        State::Uploaded
    } else {
        State::Uploading
    };
    Ok(view(id, session, written))
}

/// Creates one directory level inside the guest filesystem.
///
/// This is the explicit "new folder" of the interface, one level at a time for
/// the same reason the config pool writes one level at a time: the guest
/// filesystem cannot create a chain of directories. The parent has to exist, and
/// a name that is already taken is a conflict rather than a silent suffix — a
/// client that wants "name (2)" asks for that name explicitly, so the interface
/// chooses it and the plane never invents one.
pub fn make_directory(parent: &str, name: &str) -> Result<String, FileError> {
    let parent = checked_directory(parent)?;
    let name = checked_directory_name(name)?;
    let path = format!("{parent}/{name}");
    if ax_std::fs::read_dir(&parent).is_err() {
        return Err(FileError::NoSuchDirectory(parent));
    }
    if ax_std::fs::read_dir(&path).is_ok() {
        return Err(FileError::Exists(path));
    }
    pool::ensure_directory(&path).map_err(FileError::Unwritable)?;
    Ok(path)
}

/// How many bytes are already on disk for one session.
///
/// Placed sessions report the finished length instead: their bytes are no
/// longer staged, and a client that asks again should not be told the file it
/// finished is empty.
pub fn resume(id: &str) -> Result<usize, FileError> {
    let id = checked_id(id)?;
    let (directory, state) = {
        let sessions = SESSIONS.lock();
        let session = sessions.get(id).ok_or_else(|| unknown(id))?;
        (session.directory.clone(), session.state.clone())
    };
    let written = disk_len(&staging_path(&directory, id));
    match state {
        State::Placed => Err(FileError::Conflict {
            reason: format!("{id} is placed"),
            offset: written,
        }),
        _ => Ok(written),
    }
}

/// Appends one chunk and reports the new offset.
///
/// The chunk's own start is compared against the length on disk: a client that
/// reconnects mid-flight has a stale idea of where it stopped, and answering
/// with [`FileError::Conflict`] plus the real offset is what makes the retry
/// land in the right place.
pub fn send(id: &str, start: usize, bytes: &[u8]) -> Result<usize, FileError> {
    let id = checked_id(id)?;
    let (directory, total) = {
        let sessions = SESSIONS.lock();
        let session = sessions.get(id).ok_or_else(|| unknown(id))?;
        (session.directory.clone(), session.total)
    };

    let path = staging_path(&directory, id);
    let written = disk_len(&path);
    if start != written {
        return Err(FileError::Conflict {
            reason: format!("chunk starts at {start} but {written} bytes are on disk"),
            offset: written,
        });
    }
    let end = written + bytes.len();
    if end > FILE_LIMIT {
        return Err(FileError::TooLarge {
            total: end,
            limit: FILE_LIMIT,
        });
    }
    if end > total {
        return Err(FileError::Conflict {
            reason: format!("{end} bytes exceed the declared {total}"),
            offset: written,
        });
    }

    // `append` and `write` go together: append decides *where* the bytes go,
    // write decides that the handle may write at all.
    let mut file = OpenOptions::new()
        .append(true)
        .write(true)
        .open(&path)
        .map_err(|error| map_std_error(&error))?;
    if let Err(error) = file.write_all(bytes) {
        record_failure(id, &error.to_string());
        return Err(map_io_error(&error));
    }

    let mut sessions = SESSIONS.lock();
    if let Some(session) = sessions.get_mut(id) {
        session.state = if end >= session.total {
            State::Uploaded
        } else {
            State::Uploading
        };
    }
    Ok(end)
}

/// Moves a finished object to the path a guest config will name.
///
/// The target must not exist. That is not a convenience: a config names its
/// kernel and its disks, and replacing a file some guest already boots from
/// would change that guest's contents behind its back, so a conflict is the only
/// honest answer and the client decides what to do about it.
pub fn place(id: &str, name: &str) -> Result<SessionView, FileError> {
    let id = checked_id(id)?;
    let name = checked_name(name)?;
    let (directory, total, state) = {
        let sessions = SESSIONS.lock();
        let session = sessions.get(id).ok_or_else(|| unknown(id))?;
        (
            session.directory.clone(),
            session.total,
            session.state.clone(),
        )
    };

    let written = match state {
        State::Uploaded => disk_len(&staging_path(&directory, id)),
        State::Placed => {
            return Err(FileError::Conflict {
                reason: format!("{id} is already placed"),
                offset: total,
            });
        }
        State::Placing => {
            return Err(FileError::Conflict {
                reason: format!("{id} is being placed"),
                offset: total,
            });
        }
        State::Uploading => {
            let written = disk_len(&staging_path(&directory, id));
            return Err(FileError::Conflict {
                reason: format!("{id} is not complete: {written} of {total} bytes"),
                offset: written,
            });
        }
        State::Failed(ref detail) => {
            return Err(FileError::Conflict {
                reason: format!("{id} failed earlier: {detail}"),
                offset: disk_len(&staging_path(&directory, id)),
            });
        }
    };
    if written != total {
        return Err(FileError::Conflict {
            reason: format!("{written} bytes on disk, {total} declared"),
            offset: written,
        });
    }

    let from = staging_path(&directory, id);
    let to = format!("{directory}/{name}");
    if ax_std::fs::metadata(&to).is_ok() {
        return Err(FileError::Conflict {
            reason: format!("{to} already exists"),
            offset: written,
        });
    }

    {
        let mut sessions = SESSIONS.lock();
        if let Some(session) = sessions.get_mut(id) {
            session.state = State::Placing;
            session.name = Some(name);
        }
    }
    if let Err(error) = ax_std::fs::rename(&from, &to) {
        record_failure(id, &error.to_string());
        return Err(map_std_error(&error));
    }

    let mut sessions = SESSIONS.lock();
    let session = sessions.get_mut(id).ok_or_else(|| unknown(id))?;
    session.state = State::Placed;
    session.path = Some(to);
    Ok(view(id, session, total))
}

/// Forgets a session and removes the bytes it staged.
///
/// A placed session is not dropped: its bytes are no longer staged, they are
/// the file a config points at, and deleting them here would be a deletion the
/// operator never asked for.
pub fn drop(id: &str) -> Result<(), FileError> {
    let id = checked_id(id)?;
    let (directory, state) = {
        let sessions = SESSIONS.lock();
        let session = sessions.get(id).ok_or_else(|| unknown(id))?;
        (session.directory.clone(), session.state.clone())
    };
    if let State::Placed = state {
        return Err(FileError::Conflict {
            reason: format!("{id} is placed; its bytes are a file a config may name"),
            offset: 0,
        });
    }

    let path = staging_path(&directory, id);
    // Not being there is the outcome the caller asked for, so only a genuine
    // failure to remove something that exists is an error.
    if let Err(error) = ax_std::fs::remove_file(&path) {
        if ax_std::fs::metadata(&path).is_ok() {
            return Err(map_std_error(&error));
        }
    }
    SESSIONS.lock().remove(id);
    Ok(())
}

/// Every session a client can still act on, by id.
///
/// Sessions whose bytes are still arriving are left out on purpose: a listing is
/// how an operator picks something to boot, and an object that is not whole yet
/// must not appear there. The client that is uploading already knows its own
/// session from the offset each chunk returns.
pub fn list() -> Vec<SessionView> {
    let sessions = SESSIONS.lock();
    sessions
        .iter()
        .filter(|(_, session)| !matches!(session.state, State::Uploading))
        .map(|(id, session)| {
            let written = match session.state {
                State::Placed => session.total,
                _ => disk_len(&staging_path(&session.directory, id)),
            };
            view(id, session, written)
        })
        .collect()
}

fn view(id: &str, session: &Session, written: usize) -> SessionView {
    let detail = match &session.state {
        State::Failed(detail) => Some(detail.clone()),
        _ => None,
    };
    SessionView {
        id: id.to_string(),
        directory: session.directory.clone(),
        name: session.name.clone(),
        path: session.path.clone(),
        total: session.total,
        written,
        state: session.state.name(),
        detail,
    }
}

fn record_failure(id: &str, detail: &str) {
    let mut sessions = SESSIONS.lock();
    if let Some(session) = sessions.get_mut(id) {
        session.state = State::Failed(detail.to_string());
    }
}

fn unknown(id: &str) -> FileError {
    FileError::Unknown(id.to_string())
}

/// Bytes on disk, or zero when the file is not there yet.
fn disk_len(path: &str) -> usize {
    ax_std::fs::metadata(path)
        .map(|metadata| metadata.len() as usize)
        .unwrap_or(0)
}

/// The staging namespace of one target directory.
fn staging_dir(directory: &str) -> String {
    format!("{}/{STAGING_DIR}", directory.trim_end_matches('/'))
}

fn staging_path(directory: &str, id: &str) -> String {
    format!("{}/{id}", staging_dir(directory))
}

/// A session id has to be usable as one path component and nothing else.
fn checked_id(id: &str) -> Result<&str, FileError> {
    let id = id.trim();
    let acceptable = !id.is_empty()
        && id.len() <= SESSION_ID_MAX
        && !id.starts_with('.')
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'));
    if acceptable {
        Ok(id)
    } else {
        Err(FileError::InvalidId(id.to_string()))
    }
}

/// The target directory has to be an absolute path inside the guest filesystem.
///
/// `..` is refused rather than resolved: a staging namespace is only meaningful
/// relative to the directory the operator picked, and a path that walks out of
/// it would place files somewhere nobody selected. The staging namespace itself
/// is refused as a target too: bytes are already staged there.
fn checked_directory(directory: &str) -> Result<String, FileError> {
    let trimmed = directory.trim();
    let normalized = if trimmed == "/" {
        "/".to_string()
    } else {
        trimmed.trim_end_matches('/').to_string()
    };
    let acceptable = normalized.starts_with('/')
        && !normalized.contains("..")
        && !normalized.contains("//")
        && !normalized.ends_with(STAGING_DIR);
    if acceptable {
        Ok(normalized)
    } else {
        Err(FileError::InvalidDirectory(directory.to_string()))
    }
}

/// A directory name is one path component: the parent comes from the request.
fn checked_directory_name(name: &str) -> Result<String, FileError> {
    let name = name.trim();
    let acceptable =
        !name.is_empty() && !name.contains('/') && !name.starts_with('.') && !name.contains("..");
    if acceptable {
        Ok(name.to_string())
    } else {
        Err(FileError::InvalidName(name.to_string()))
    }
}

/// A final name is a plain file name: the directory comes from the session.
fn checked_name(name: &str) -> Result<String, FileError> {
    let name = name.trim();
    let acceptable =
        !name.is_empty() && !name.contains('/') && !name.starts_with('.') && !name.contains("..");
    if acceptable {
        Ok(name.to_string())
    } else {
        Err(FileError::InvalidName(name.to_string()))
    }
}

/// Distinguishes "the disk is full" from "the write failed".
///
/// A full disk is the one write failure a client can act on: the offset stays
/// valid, so the upload continues where it stopped once space is freed. The kind
/// is the primary signal; the message is a fallback, because the guest
/// filesystem stack does not guarantee how the out-of-space condition is
/// spelled once it has travelled through it.
fn map_io_error(error: &IoError) -> FileError {
    if *error == IoError::StorageFull {
        FileError::StorageFull(error.to_string())
    } else {
        FileError::Unwritable(error.to_string())
    }
}

/// The same distinction for the calls that report the facade's own error.
///
/// The filesystem operations wrap the io error, so an out-of-space condition
/// arrives one level deeper here than it does from a write.
fn map_std_error(error: &StdError) -> FileError {
    match error {
        StdError::Io(io) => map_io_error(io),
        other => FileError::Unwritable(other.to_string()),
    }
}
