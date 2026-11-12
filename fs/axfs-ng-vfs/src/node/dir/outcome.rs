//! Observations survive backend persistence failures and attempt retries.

use alloc::string::String;

use crate::{DirEntry, NodeIdentity, NodeType, VfsError, VfsResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespaceState {
    Unchanged,
    Retry,
    Applied,
    Uncertain,
}

/// Actual backend object selected by a namespace attempt, not a pathname probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceNode {
    pub inode: u64,
    pub node_type: NodeType,
    pub identity: Option<NodeIdentity>,
}

impl NamespaceNode {
    pub fn new(inode: u64, node_type: NodeType) -> Self {
        Self {
            inode,
            node_type,
            identity: None,
        }
    }

    pub fn matches(&self, entry: &DirEntry) -> bool {
        match (&self.identity, entry.identity()) {
            (Some(left), Some(right)) => left == right,
            (None, None) => self.inode == entry.inode() && self.node_type == entry.node_type(),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameAction {
    Noop,
    Move,
    Exchange,
}

#[derive(Debug, Clone)]
pub struct RenameEffect {
    pub source: NamespaceNode,
    pub target: Option<NamespaceNode>,
    pub action: RenameAction,
    pub source_name: String,
    pub destination_name: String,
    /// Policies determined by the locked backend, never queried during publication.
    pub source_cacheable: bool,
    pub destination_cacheable: bool,
}

/// An alias selected by the current backend attempt.
#[derive(Debug, Clone)]
pub struct NamespaceEntry {
    pub entry: DirEntry,
    pub canonical_name: String,
    pub cacheable: bool,
}

#[derive(Debug, Clone)]
pub struct CreateEffect {
    pub selected: NamespaceEntry,
    pub created: bool,
}

/// The actual effect of one data command, independent from its completion.
#[derive(Debug, Clone)]
pub enum NamespaceEffect {
    Create(CreateEffect),
    Link(NamespaceEntry),
    Unlink(NamespaceEntry),
    Rename(RenameEffect),
}

/// One attempt's namespace effect and its independently fallible completion.
///
/// VFS must publish `Applied`, or invalidate `Uncertain`, before returning the
/// completion error. `Retry` never carries permission to replay an old attempt.
#[derive(Debug, Clone)]
#[must_use = "publish or invalidate the namespace effect before returning its completion result"]
pub struct NamespaceOutcome<T> {
    state: NamespaceState,
    observation: Option<T>,
    result: VfsResult<()>,
}

impl<T> NamespaceOutcome<T> {
    pub fn unchanged(result: VfsResult<()>) -> Self {
        Self {
            state: NamespaceState::Unchanged,
            observation: None,
            result,
        }
    }

    pub fn unchanged_observed(observation: T, result: VfsResult<()>) -> Self {
        Self {
            state: NamespaceState::Unchanged,
            observation: Some(observation),
            result,
        }
    }

    pub fn retry() -> Self {
        Self {
            state: NamespaceState::Retry,
            observation: None,
            result: Err(VfsError::WouldBlock),
        }
    }

    pub fn applied(observation: T, result: VfsResult<()>) -> Self {
        Self {
            state: NamespaceState::Applied,
            observation: Some(observation),
            result,
        }
    }

    pub fn uncertain(observation: Option<T>, result: VfsResult<()>) -> Self {
        Self {
            state: NamespaceState::Uncertain,
            observation,
            result,
        }
    }

    pub fn state(&self) -> NamespaceState {
        self.state
    }

    pub fn observation(&self) -> Option<&T> {
        self.observation.as_ref()
    }

    /// Returns the original attempt result without consuming its observation.
    pub fn result(&self) -> VfsResult<()> {
        self.result
    }

    /// Merges lock-external completion, preserving the first real error.
    /// A completion failure ends Retry: its WouldBlock is only a control marker.
    /// Other states and their actual observations remain unchanged.
    pub fn with_completion(mut self, completion: VfsResult<()>) -> Self {
        if self.state == NamespaceState::Retry
            && let Err(error) = completion
        {
            return Self::unchanged(Err(error));
        }
        self.result = self.result.and(completion);
        self
    }

    pub fn into_result(self) -> VfsResult<()> {
        self.result
    }

    /// Consumes a completed command without probing its former pathname.
    pub fn into_observation(self) -> VfsResult<T> {
        self.result?;
        self.observation.ok_or(VfsError::BadState)
    }

    /// Changes only the observation type, preserving state and the first error.
    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> NamespaceOutcome<U> {
        NamespaceOutcome {
            state: self.state,
            observation: self.observation.map(map),
            result: self.result,
        }
    }
}
