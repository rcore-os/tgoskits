// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Candidate guest configs that a start request may create on demand.
//!
//! A candidate is any guest config under the guest tree: [`scan`] reads the
//! directory a new config is written to ([`directory`], `AXVISOR_VM_POOL` when
//! set and [`DEFAULT_VM_ROOT`] otherwise), then every `AXVISOR_VM_DIRS` entry,
//! then [`DEFAULT_VM_ROOT`] itself, each of them recursively to
//! [`MAX_SCAN_DEPTH`]. An entry is only a candidate: nothing is created at
//! startup, so an idle pool costs no guest memory and may list more guests than
//! the machine can run at once. This is the difference from
//! [`DEFAULT_VM_CONFIG_DIR`], whose configs the startup path creates immediately
//! as the default guest set.
//!
//! The tree is the single authority for the candidate set: the scan reads it on
//! every call and keeps no cache, so a config that the host adds, replaces or
//! repairs is visible to the next query without a reboot. Reading a whole tree
//! also means meeting documents that are no guest config at all —
//! manifests, metadata, editor settings. Those are skipped, because "unusable
//! config" is a report an operator can act on and "some other tool's file" is
//! not. A file that *is* a config attempt and cannot be used becomes an [`Issue`]
//! instead of disappearing, so the shell and the control plane can say why it
//! does nothing; a config that parses but names a guest image the filesystem
//! does not have is one of those values, because it is a pool file that cannot
//! become a VM.

use alloc::{
    collections::BTreeSet,
    format,
    string::{String, ToString},
    vec::Vec,
};

use ax_std::fs::FileTypeExt;
use axvmconfig::GuestConfig;

/// Guest tree the pool reads, and writes a new config into, by default.
///
/// A guest's configuration lives next to the images it names, so that tree is
/// what a scan reads: every `.toml` under it is a candidate. There is no drop-in
/// folder of the pool's own any more, and nothing creates one at startup.
pub const DEFAULT_VM_ROOT: &str = "/guest";

/// Directory of configs that the startup path creates as the default guest set.
pub const DEFAULT_VM_CONFIG_DIR: &str = "/guest/vm_default";

/// How many directory levels below a source a scan descends.
///
/// The scan reads a whole filesystem, so it needs a floor: an operator's
/// pathological tree (a deep package cache, say) must not hold the control
/// plane for as long as it takes to walk it. Configs deeper than this are not
/// candidates.
pub const MAX_SCAN_DEPTH: usize = 8;

/// Directory a new config is written to: `[env] AXVISOR_VM_POOL` wins over
/// [`DEFAULT_VM_ROOT`].
pub fn directory() -> &'static str {
    option_env!("AXVISOR_VM_POOL").unwrap_or(DEFAULT_VM_ROOT)
}

/// Every directory the pool is read from, in precedence order.
///
/// The directory a config is written to comes first, so a config the operator
/// just saved wins over one of the same id found later; `AXVISOR_VM_DIRS`
/// appends `:`-separated directories for a host that keeps its configs outside
/// the tree; and [`DEFAULT_VM_ROOT`] comes last, so a config anywhere under the
/// guest tree is a candidate as well. Every source is read recursively, to
/// [`MAX_SCAN_DEPTH`].
///
/// The startup directory is not listed separately: the guest tree covers it. A
/// config there is listed by that scan, and because its id is already registered
/// as a default guest, a second file claiming the id is reported by the
/// duplicate rule and a start request for it fails like any other taken id.
pub fn sources() -> Vec<String> {
    let mut sources: Vec<String> = Vec::new();
    let mut push = |directory: &str| {
        let directory = directory.trim();
        if !directory.is_empty() && !sources.iter().any(|known| known == directory) {
            sources.push(directory.to_string());
        }
    };
    push(directory());
    for extra in option_env!("AXVISOR_VM_DIRS").unwrap_or("").split(':') {
        push(extra);
    }
    push(DEFAULT_VM_ROOT);
    sources
}

/// One pool config that a start request can turn into a running VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    path: String,
    source: String,
    id: usize,
    name: String,
    toml: String,
}

impl Entry {
    /// Path of the config file, as reported by the filesystem.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Directory this entry was read from.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// `base.id` the config asks for, which is also the runtime VM id.
    pub fn id(&self) -> usize {
        self.id
    }

    /// `base.name` from the config.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Raw TOML text, for callers that create the VM themselves.
    pub fn toml(&self) -> &str {
        &self.toml
    }
}

/// Why one pool file cannot be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueKind {
    /// The pool directory is missing or cannot be opened; the pool is empty.
    DirectoryUnavailable(String),
    /// The file exists but could not be read as UTF-8 text.
    Unreadable(String),
    /// The file holds no text at all.
    Empty,
    /// The file is not a guest config TOML document.
    InvalidToml(String),
    /// An earlier file in the same scan already claims this `base.id`, so this
    /// config cannot be registered while that one is in the pool.
    DuplicateId { id: usize, claimed_by: String },
    /// The config reads its guest images from the filesystem and names one that
    /// does not exist, so creating it would fail partway through.
    MissingImage(String),
}

impl IssueKind {
    /// Stable token naming the kind, for machine readers such as the control
    /// plane's JSON. The `Display` text is human-facing and may change; this
    /// token is the part clients may match on.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DirectoryUnavailable(_) => "directory-unavailable",
            Self::Unreadable(_) => "unreadable",
            Self::Empty => "empty",
            Self::InvalidToml(_) => "invalid-toml",
            Self::DuplicateId { .. } => "duplicate-id",
            Self::MissingImage(_) => "missing-image",
        }
    }
}

impl core::fmt::Display for IssueKind {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DirectoryUnavailable(error) => {
                write!(formatter, "directory is unavailable: {error}")
            }
            Self::Unreadable(error) => write!(formatter, "cannot be read: {error}"),
            Self::Empty => formatter.write_str("is empty"),
            Self::InvalidToml(error) => write!(formatter, "is not a guest config: {error}"),
            Self::DuplicateId { id, claimed_by } => write!(
                formatter,
                "asks for VM id {id}, which `{claimed_by}` already claims in this pool"
            ),
            Self::MissingImage(path) => {
                write!(formatter, "names an image that does not exist: {path}")
            }
        }
    }
}

/// One unusable pool file, or the pool directory itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    path: String,
    kind: IssueKind,
}

impl Issue {
    /// Path of the file that could not be used, or of the directory itself.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// What is wrong with the reported path.
    pub fn kind(&self) -> &IssueKind {
        &self.kind
    }
}

impl core::fmt::Display for Issue {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}: {}", self.path, self.kind)
    }
}

/// Result of one pool scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pool {
    directory: String,
    sources: Vec<String>,
    entries: Vec<Entry>,
    issues: Vec<Issue>,
}

impl Pool {
    /// First directory this scan read, the one a new config is written to.
    pub fn directory(&self) -> &str {
        &self.directory
    }

    /// Every directory this scan read, in precedence order.
    pub fn sources(&self) -> &[String] {
        &self.sources
    }

    /// Startable configs, in the order the filesystem reported them.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Files that could not become an entry, plus the directory itself when it
    /// could not be opened.
    pub fn issues(&self) -> &[Issue] {
        &self.issues
    }

    /// The entry that claims `id`, if the pool holds a usable config for it.
    pub fn entry(&self, id: usize) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.id == id)
    }
}

/// Scan every directory reported by [`sources`].
pub fn scan() -> Pool {
    scan_dirs(&sources())
}

/// Scan one directory of guest configs.
///
/// A missing or unreadable directory yields an empty pool with a single
/// [`IssueKind::DirectoryUnavailable`] issue rather than an error, so callers
/// can render "no pool provisioned" without special-casing failures.
pub fn scan_dir(directory: &str) -> Pool {
    scan_dirs(&[directory.to_string()])
}

/// Scan `directories` in order, each one recursively.
///
/// The first directory that offers a given `base.id` wins and later copies are
/// reported as [`IssueKind::DuplicateId`], so a precedence order (see
/// [`sources`]) decides which of two files with the same id is startable. The
/// filesystem root is a source too, so a file is reachable twice; it is read
/// once, through the first source that reaches it. Nothing is cached: a config
/// the host adds, replaces or removes is visible to the next scan.
pub fn scan_dirs(directories: &[String]) -> Pool {
    let mut scan = Scan::default();
    for directory in directories {
        scan.walk(directory, 0);
    }
    Pool {
        directory: directories.first().cloned().unwrap_or_default(),
        sources: directories.to_vec(),
        entries: scan.entries,
        issues: scan.issues,
    }
}

/// One scan's accumulated result, carried through the recursive walk.
#[derive(Default)]
struct Scan {
    entries: Vec<Entry>,
    issues: Vec<Issue>,
    /// Config paths already read. The filesystem root reaches every file a
    /// narrower source reaches and both are read, so without this the same file
    /// would be claimed twice and reported as its own duplicate.
    read: BTreeSet<String>,
}

impl Scan {
    /// Read `directory`, then every directory below it, to [`MAX_SCAN_DEPTH`].
    fn walk(&mut self, directory: &str, depth: usize) {
        let read_dir = match ax_std::fs::read_dir(directory) {
            Ok(read_dir) => read_dir,
            Err(error) => {
                self.issues.push(Issue {
                    path: directory.to_string(),
                    kind: IssueKind::DirectoryUnavailable(error.to_string()),
                });
                return;
            }
        };

        for dir_entry in read_dir {
            let (path, is_directory) = match dir_entry {
                Ok(dir_entry) => (dir_entry.path(), dir_entry.file_type().is_dir()),
                Err(error) => {
                    self.issues.push(Issue {
                        path: directory.to_string(),
                        kind: IssueKind::Unreadable(format!("directory entry: {error}")),
                    });
                    continue;
                }
            };
            // A symlink to a directory has its own type, so it is not descended
            // into; that also keeps a link pointing back up the tree finite.
            if is_directory {
                if depth < MAX_SCAN_DEPTH {
                    self.walk(&path, depth + 1);
                }
                continue;
            }
            if !path.ends_with(".toml") || !self.read.insert(path.clone()) {
                continue;
            }

            let entry = match parse_entry(&path, directory) {
                Ok(Some(entry)) => entry,
                Ok(None) => continue,
                Err(kind) => {
                    self.issues.push(Issue { path, kind });
                    continue;
                }
            };
            match self.entries.iter().find(|known| known.id == entry.id) {
                Some(known) => self.issues.push(Issue {
                    path: entry.path,
                    kind: IssueKind::DuplicateId {
                        id: entry.id,
                        claimed_by: known.path.clone(),
                    },
                }),
                None => self.entries.push(entry),
            }
        }
    }
}

/// Report the pool contents through the host log.
///
/// The startup path only reports the pool: creating a candidate is the start
/// request's job. Reporting here makes a broken pool visible in the serial log
/// before anything asks for it.
pub fn log_startup_state() {
    let pool = scan();
    info!(
        "VM pool: {} folder(s): {}",
        pool.sources().len(),
        pool.sources().join(", ")
    );
    info!(
        "VM pool `{}`: {} config(s)",
        pool.directory(),
        pool.entries().len()
    );
    for entry in pool.entries() {
        info!(
            "  VM pool entry: VM[{}] `{}` from {}",
            entry.id(),
            entry.name(),
            entry.path()
        );
    }
    log_issues(&pool);
}

/// Log every issue of `pool` through the host log.
///
/// An unprovisioned pool is a normal state and stays at info level; a file that
/// cannot be started is a warning.
pub fn log_issues(pool: &Pool) {
    for issue in pool.issues() {
        match issue.kind() {
            IssueKind::DirectoryUnavailable(_) => info!("VM pool: {issue}"),
            _ => warn!("VM pool: {issue}"),
        }
    }
}

/// Read one candidate config.
///
/// `Ok(None)` means "not a guest config at all": the scan reads a whole
/// filesystem and meets every `.toml` on it, and only a document carrying one of
/// the guest config's own top-level keys is a candidate. A candidate that cannot
/// be used is still an `Err`, which is what keeps a damaged config visible
/// instead of silently absent.
fn parse_entry(path: &str, source: &str) -> Result<Option<Entry>, IssueKind> {
    let toml = ax_std::fs::read_to_string(path)
        .map_err(|error| IssueKind::Unreadable(error.to_string()))?;
    if toml.trim().is_empty() {
        return Err(IssueKind::Empty);
    }
    if !looks_like_guest_config(&toml) {
        return Ok(None);
    }

    let config = GuestConfig::from_toml(&toml)
        .map_err(|error| IssueKind::InvalidToml(format!("{error}")))?;
    // A config that names an image which is not there cannot become a VM, so it
    // is reported instead of listed as startable. The file may be provisioned
    // later; the next scan picks it up because nothing is cached.
    if let Some(issue) = unusable_image(&config) {
        return Err(issue);
    }
    Ok(Some(Entry {
        path: path.to_string(),
        source: source.to_string(),
        id: config.base.id,
        name: config.base.name.clone(),
        toml,
    }))
}

/// Whether `toml` is an attempt at a guest config rather than an unrelated
/// document.
///
/// A whole-filesystem scan meets every `.toml` an operator, a package or a tool
/// left anywhere on it — manifests, metadata, editor settings. None of those is
/// a damaged guest config, and reporting them would bury the files that are. So
/// a document is a candidate only if it names one of the guest config's own
/// top-level keys, as a table or as a key assignment; whitespace does not
/// distinguish the two and is ignored.
fn looks_like_guest_config(toml: &str) -> bool {
    toml.lines().any(|line| {
        let compact: String = line
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        ["base", "kernel"]
            .into_iter()
            .any(|key| line_names_top_level_key(&compact, key))
    })
}

/// Why a document that names no guest config key is not one.
const NO_GUEST_CONFIG_KEY: &str = "it names neither a `base` nor a `kernel` table";

/// Whether one compacted line names `key` at the top level.
fn line_names_top_level_key(compact: &str, key: &str) -> bool {
    if let Some(rest) = compact.strip_prefix('[') {
        // `[base]` and `[base.something]` are the key; `[database]` is not.
        return rest
            .strip_prefix(key)
            .is_some_and(|rest| rest.starts_with(']') || rest.starts_with('.'));
    }
    compact
        .strip_prefix(key)
        .is_some_and(|rest| rest.starts_with('=') || rest.starts_with('.'))
}

/// One entry of a browsed folder: a subdirectory or a config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    path: String,
    parent: Option<String>,
    directories: Vec<Directory>,
    files: Vec<File>,
    entries: Vec<Entry>,
    issues: Vec<Issue>,
}

/// One file in a browsed folder that is not a directory.
///
/// A folder view lists what is there, and what a file is *for* belongs to
/// whoever references it, so this carries only what the filesystem knows: the
/// name, the path, and how long it is. The configuration pool's own view of the
/// same folder is [`Entry`], which exists only for files that can become a VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    name: String,
    path: String,
    size: usize,
}

impl File {
    /// Last component of the path.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Path to read.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Length in bytes, or zero when the file could not be measured.
    pub fn size(&self) -> usize {
        self.size
    }
}

/// One subdirectory that can be browsed into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directory {
    name: String,
    path: String,
}

impl Directory {
    /// Last component of the path.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Path to browse into.
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl Folder {
    /// Directory that was read.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Directory one level up, unless this is the filesystem root.
    pub fn parent(&self) -> Option<&str> {
        self.parent.as_deref()
    }

    /// Subdirectories, by name.
    pub fn directories(&self) -> &[Directory] {
        &self.directories
    }

    /// Files in this folder, by name.
    pub fn files(&self) -> &[File] {
        &self.files
    }

    /// Startable configs in this folder, by name.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// `.toml` files that cannot be started, plus the folder itself when it
    /// could not be read.
    pub fn issues(&self) -> &[Issue] {
        &self.issues
    }
}

/// Browse one directory of the guest filesystem.
///
/// This is what lets the operator pick a config from anywhere instead of only
/// from the pool: every `.toml` is parsed on the spot, so the result already
/// says which files are startable and why the others are not — including one
/// that is no guest config at all, which is reported because this answer is
/// about the folder the caller asked for. A directory that cannot be read comes
/// back as an empty [`Folder`] with a [`IssueKind::DirectoryUnavailable`] issue,
/// the same shape a pool scan uses.
pub fn browse(path: &str) -> Folder {
    let mut directories = Vec::new();
    let mut files = Vec::new();
    let mut entries = Vec::new();
    let mut issues = Vec::new();

    match ax_std::fs::read_dir(path) {
        Ok(read_dir) => {
            for dir_entry in read_dir {
                let (entry_path, is_directory) = match dir_entry {
                    Ok(dir_entry) => (dir_entry.path(), dir_entry.file_type().is_dir()),
                    Err(error) => {
                        issues.push(Issue {
                            path: path.to_string(),
                            kind: IssueKind::Unreadable(format!("directory entry: {error}")),
                        });
                        continue;
                    }
                };
                let name = entry_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&entry_path)
                    .to_string();
                // `ax_std::fs::metadata` opens the path, which fails on a
                // directory, so the entry's own type is what decides whether
                // this is a folder to walk into or a file to try to parse.
                if is_directory {
                    directories.push(Directory {
                        name,
                        path: entry_path,
                    });
                } else {
                    // Every file is listed, whatever it holds: a folder view
                    // answers "what is in here", and a file that cannot be
                    // measured is still a file whose name is known.
                    files.push(File {
                        size: ax_std::fs::metadata(&entry_path)
                            .map(|metadata| metadata.len() as usize)
                            .unwrap_or(0),
                        name,
                        path: entry_path.clone(),
                    });
                    if entry_path.ends_with(".toml") {
                        match parse_entry(&entry_path, path) {
                            Ok(Some(entry)) => entries.push(entry),
                            // A browse is how a caller confirms what a folder
                            // holds, so a `.toml` that is no guest config is
                            // reported instead of hidden. The scan skips those
                            // because a whole filesystem is full of them; this
                            // answer is about one folder the caller asked for.
                            Ok(None) => issues.push(Issue {
                                path: entry_path,
                                kind: IssueKind::InvalidToml(NO_GUEST_CONFIG_KEY.to_string()),
                            }),
                            Err(kind) => issues.push(Issue {
                                path: entry_path,
                                kind,
                            }),
                        }
                    }
                }
            }
        }
        Err(error) => issues.push(Issue {
            path: path.to_string(),
            kind: IssueKind::DirectoryUnavailable(error.to_string()),
        }),
    }

    directories.sort_by(|left, right| left.name.cmp(&right.name));
    files.sort_by(|left, right| left.name.cmp(&right.name));
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    issues.sort_by(|left, right| left.path.cmp(&right.path));

    let parent = {
        let trimmed = path.trim_end_matches('/');
        match trimmed.rsplit_once('/') {
            // `/guest` and `/guest/` both sit directly under the root.
            Some(("", _)) => Some("/".to_string()),
            Some((parent, _)) => Some(parent.to_string()),
            // No separator at all: a relative single component, or the root.
            None => None,
        }
    };

    Folder {
        path: path.to_string(),
        parent,
        directories,
        files,
        entries,
        issues,
    }
}

/// Why a config could not be stored in the pool directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// The name is not a plain `*.toml` file name.
    InvalidName(String),
    /// The text is not a guest config TOML document.
    InvalidToml(String),
    /// The pool directory or the file could not be written.
    Unwritable(String),
}

impl core::fmt::Display for SaveError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidName(name) => write!(
                formatter,
                "`{name}` is not a file name (a plain name ending in .toml is required)"
            ),
            Self::InvalidToml(error) => write!(formatter, "not a guest config: {error}"),
            Self::Unwritable(error) => write!(formatter, "cannot be written: {error}"),
        }
    }
}

/// Create a directory if it is not there yet, reporting the failure as text.
///
/// `ax_std::fs::create_dir_all` is not usable on the guest filesystem (it
/// reports "recursive directory creation is not supported"), so the pool only
/// ever ensures one level. An existing directory is left untouched, which is
/// what makes this callable on every start and on every save.
/// Creates one directory level if it is not there yet.
///
/// Shared with the file transfer ([`super::files`]), which stages bytes in a
/// namespace of its own inside the target directory and therefore needs the same
/// one-level creation.
pub(crate) fn ensure_directory(directory: &str) -> Result<(), String> {
    if ax_std::fs::read_dir(directory).is_ok() {
        return Ok(());
    }
    ax_std::fs::create_dir(directory).map_err(|error| error.to_string())
}

/// Store a guest config in the directory new configs go to, returning its path.
///
/// This is how a config reaches the pool on a machine whose shell cannot write
/// a multi-line file: the control plane validates the text and writes it as a
/// pool file, after which it is a candidate like any other. The name is
/// restricted to a plain `*.toml` file name so the request cannot write outside
/// that directory.
pub fn save(name: &str, toml: &str) -> Result<String, SaveError> {
    save_in(&directory(), name, toml)
}

/// Store a guest config in a specific directory, returning its path.
///
/// [`save`] passes the directory new configs go to; taking the directory as an
/// argument is what makes the write path testable against a temporary fixture
/// instead of the deployed tree. Validation is identical in both cases: the name
/// must be a plain `*.toml` file name (no separators, no leading dot) and the
/// text must parse as a guest config before anything is written.
pub fn save_in(directory: &str, name: &str, toml: &str) -> Result<String, SaveError> {
    let name = name.trim();
    if name.is_empty() || !name.ends_with(".toml") || name.contains('/') || name.starts_with('.') {
        return Err(SaveError::InvalidName(name.to_string()));
    }
    GuestConfig::from_toml(toml).map_err(|error| SaveError::InvalidToml(format!("{error}")))?;

    // The guest filesystem cannot create parent directories — `create_dir_all`
    // reports "recursive directory creation is not supported" — so an absent
    // target directory is created one level deep, which is all a save needs.
    ensure_directory(directory).map_err(SaveError::Unwritable)?;
    let path = format!("{directory}/{name}");
    ax_std::fs::write(&path, toml).map_err(|error| SaveError::Unwritable(error.to_string()))?;
    Ok(path)
}
///
/// The first guest image a config names but that is missing from the guest
/// filesystem.
///
/// Only a config that reads its images from the guest filesystem names files
/// this plane can look up: a config whose images are embedded in the hypervisor
/// (`image_location = "memory"`) has nothing to check here, and its kernel path
/// is allowed to be absent from the guest filesystem. A filesystem config that
/// names an absent kernel, ramdisk or DTB cannot become a VM, so it is reported
/// rather than listed: the operator would otherwise pick a config that fails on
/// start.
fn unusable_image(config: &GuestConfig) -> Option<IssueKind> {
    missing_guest_image(config).map(IssueKind::MissingImage)
}

/// The first path a config names that is not in the guest filesystem.
///
/// This is the predicate two planes ask: the pool scan uses it to report a
/// config that cannot become a VM, and the creation path uses it to refuse a
/// request that names a file a transfer has not placed yet. Sharing it is the
/// point — "is this file in place" is one fact, and neither caller has to know
/// the other exists.
pub(crate) fn missing_guest_image(config: &GuestConfig) -> Option<String> {
    if config.kernel.image_location.as_deref() != Some("fs") {
        return None;
    }

    [
        Some(config.kernel.kernel_path.as_str()),
        config.kernel.ramdisk_path.as_deref(),
        config.kernel.dtb_path.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|path| !path.is_empty())
    .find(|path| ax_std::fs::metadata(path).is_err())
    .map(|path| path.to_string())
}
