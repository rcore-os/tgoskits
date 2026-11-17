//! One operation owns its path frames, accepted uses, caller and link budget.

use alloc::{borrow::Cow, collections::VecDeque, string::String, vec::Vec};

use axfs_ng_vfs::{
    LinkCaller, LinkTarget, Location, LookupStep, MutationCredentials, NodePermission, NodeType,
    ParentStep, PathHandle, PathObservation, VfsError, VfsResult,
};

use super::{FsView, LookupPolicy, LookupRestrictions, LookupScope, SYMLINKS_MAX};

type SearchCheck<'a> = Option<&'a dyn Fn(&Location) -> VfsResult<()>>;

enum Component<'a> {
    Root,
    Current,
    Parent,
    Name(Cow<'a, str>),
    Directory,
}

struct LinkBudget {
    used: usize,
}

impl LinkBudget {
    fn charge(&mut self) -> VfsResult<()> {
        if self.used == SYMLINKS_MAX {
            return Err(VfsError::FilesystemLoop);
        }
        self.used += 1;
        Ok(())
    }
}

/// Frozen operation state; helpers borrow it instead of starting another walk.
pub(crate) struct LookupSession<'a> {
    root: PathHandle,
    policy: LookupPolicy,
    observation: Option<PathObservation>,
    current: PathHandle,
    pending: VecDeque<Component<'a>>,
    searched: Vec<PathHandle>,
    caller: &'a dyn LinkCaller,
    credentials: MutationCredentials<'a>,
    search: SearchCheck<'a>,
    budget: LinkBudget,
}

enum FinalEntry<'a> {
    Name(Cow<'a, str>),
    Resolved,
}

/// The unresolved final component and its original operation, consumed together.
pub(crate) struct Continuation<'a> {
    session: LookupSession<'a>,
    entry: FinalEntry<'a>,
    directory: bool,
}

pub(crate) struct LookupOpenIntent {
    pub node: axfs_ng_vfs::OpenOptions,
    pub follow: bool,
    pub path_only: bool,
    pub directory: bool,
}

pub(crate) struct ResolvedOpen {
    pub path: PathHandle,
    pub created: bool,
}

impl<'a> LookupSession<'a> {
    pub(crate) fn new(view: &FsView<'a>, path: &'a str) -> VfsResult<Self> {
        if path.is_empty() {
            return Err(VfsError::NotFound);
        }
        let policy = view.lookup_policy();
        let scoped = policy.scope != LookupScope::ProcessRoot;
        if policy.scope == LookupScope::Beneath && path.starts_with('/') {
            return Err(VfsError::CrossesDevices);
        }
        let root = if scoped { view.cwd() } else { view.root() }.clone();
        let mut pending = components(path);
        let current = if matches!(pending.front(), Some(Component::Root)) {
            pending.pop_front();
            root.clone()
        } else {
            view.cwd().clone()
        };
        Ok(Self {
            root,
            policy,
            observation: scoped.then(PathObservation::capture).transpose()?,
            current,
            pending,
            searched: Vec::new(),
            caller: view.caller(),
            credentials: *view.credentials(),
            search: None,
            budget: LinkBudget { used: 0 },
        })
    }

    pub(crate) fn with_search(mut self, search: &'a dyn Fn(&Location) -> VfsResult<()>) -> Self {
        self.search = Some(search);
        self
    }

    pub(crate) fn parent(mut self) -> VfsResult<Continuation<'a>> {
        while let Some(component) = self.pending.pop_front() {
            match component {
                Component::Root => self.jump_root()?,
                Component::Current => {
                    self.check_search()?;
                    self.remember_search();
                }
                Component::Parent => self.ascend()?,
                Component::Directory => self.current.location().check_is_dir()?,
                Component::Name(name) => {
                    if self
                        .pending
                        .iter()
                        .all(|part| matches!(part, Component::Directory))
                    {
                        let directory = !self.pending.is_empty();
                        return Ok(Continuation {
                            session: self,
                            entry: FinalEntry::Name(name),
                            directory,
                        });
                    }
                    let child = self.lookup(&name)?.ok_or(VfsError::NotFound)?;
                    self.remember_search();
                    if child.location().node_type() == NodeType::Symlink {
                        self.follow(child)?;
                    } else {
                        self.current = child;
                    }
                }
            }
        }
        Ok(Continuation {
            session: self,
            entry: FinalEntry::Resolved,
            directory: false,
        })
    }

    fn check_search(&self) -> VfsResult<()> {
        self.check_directory(&self.current)
    }

    fn check_directory(&self, directory: &PathHandle) -> VfsResult<()> {
        directory.location().check_is_dir()?;
        self.credentials.check_permission(
            &directory.location().metadata()?,
            NodePermission::OTHER_EXEC,
        )?;
        if let Some(check) = self.search {
            check(directory.location())?;
        }
        Ok(())
    }

    fn remember_search(&mut self) {
        self.searched.push(self.current.clone());
    }

    fn validate_searches(&self) -> VfsResult<()> {
        self.validate_observation()?;
        for directory in &self.searched {
            self.check_directory(directory)?;
        }
        Ok(())
    }

    fn bind(&self, location: Location) -> VfsResult<PathHandle> {
        self.validate_observation()?;
        if self
            .policy
            .restrictions
            .contains(LookupRestrictions::NO_XDEV)
            && !alloc::sync::Arc::ptr_eq(
                self.current.location().mountpoint(),
                location.mountpoint(),
            )
        {
            return Err(VfsError::CrossesDevices);
        }
        if alloc::sync::Arc::ptr_eq(self.current.location().mountpoint(), location.mountpoint()) {
            self.current.rebind(location)
        } else {
            PathHandle::acquire(location)
        }
    }

    fn lookup(&self, name: &str) -> VfsResult<Option<PathHandle>> {
        loop {
            self.validate_observation()?;
            self.check_search()?;
            match self.current.location().lookup_component(name) {
                LookupStep::Found(location) => return self.bind(location).map(Some),
                LookupStep::Missing => return Ok(None),
                LookupStep::Failed(error) => return Err(error),
                LookupStep::Revalidate(_) => {
                    // No candidate, backend gate or command admission survives
                    // this boundary. The original paths and budget stay owned.
                    let runtime =
                        crate::os::runtime_ops().map_err(crate::block_error_to_vfs_error)?;
                    runtime
                        .yield_now()
                        .map_err(crate::block_error_to_vfs_error)?;
                }
            }
        }
    }

    fn ascend(&mut self) -> VfsResult<()> {
        self.validate_observation()?;
        self.check_search()?;
        self.remember_search();
        let mut location = self.current.location().clone();
        loop {
            if location.ptr_eq(self.root.location()) {
                return if self.policy.scope == LookupScope::Beneath {
                    Err(VfsError::CrossesDevices)
                } else {
                    self.validate_observation()
                };
            }
            match location.parent_step()? {
                ParentStep::Parent { parent, .. }
                | ParentStep::Unlinked {
                    parent: Some(parent),
                    ..
                } => {
                    self.current = self.bind(parent)?;
                    return self.validate_observation();
                }
                ParentStep::Covered(covered) => {
                    if covered.ptr_eq(self.root.location()) {
                        location = covered;
                    } else if self
                        .policy
                        .restrictions
                        .contains(LookupRestrictions::NO_XDEV)
                    {
                        return Err(VfsError::CrossesDevices);
                    } else {
                        location = covered;
                    }
                }
                ParentStep::Root | ParentStep::Unlinked { parent: None, .. } => {
                    return self.validate_observation();
                }
            }
        }
    }

    fn follow(&mut self, link: PathHandle) -> VfsResult<()> {
        if self
            .policy
            .restrictions
            .contains(LookupRestrictions::NO_SYMLINKS)
        {
            return Err(VfsError::FilesystemLoop);
        }
        self.budget.charge()?;
        match link.location().entry().link_target(self.caller)? {
            LinkTarget::Text(target) => {
                if target.is_empty() {
                    return Err(VfsError::NotFound);
                }
                let mut target = owned_components(&target);
                target.append(&mut self.pending);
                self.pending = target;
            }
            LinkTarget::Object(target) => {
                if self
                    .policy
                    .restrictions
                    .contains(LookupRestrictions::NO_MAGICLINKS)
                {
                    return Err(VfsError::FilesystemLoop);
                }
                if self.policy.scope != LookupScope::ProcessRoot {
                    return Err(VfsError::CrossesDevices);
                }
                if self
                    .policy
                    .restrictions
                    .contains(LookupRestrictions::NO_XDEV)
                    && !alloc::sync::Arc::ptr_eq(
                        self.current.location().mountpoint(),
                        target.location().mountpoint(),
                    )
                {
                    return Err(VfsError::CrossesDevices);
                }
                self.current = target;
            }
        }
        Ok(())
    }

    fn jump_root(&mut self) -> VfsResult<()> {
        if self.policy.scope == LookupScope::Beneath {
            return Err(VfsError::CrossesDevices);
        }
        if self
            .policy
            .restrictions
            .contains(LookupRestrictions::NO_XDEV)
            && !alloc::sync::Arc::ptr_eq(
                self.current.location().mountpoint(),
                self.root.location().mountpoint(),
            )
        {
            return Err(VfsError::CrossesDevices);
        }
        self.validate_observation()?;
        self.current = self.root.clone();
        Ok(())
    }

    fn validate_observation(&self) -> VfsResult<()> {
        if let Some(observation) = self.observation {
            observation.validate()?;
        }
        Ok(())
    }

    fn validate_result(&self, path: &PathHandle) -> VfsResult<()> {
        self.validate_observation()?;
        if self.observation.is_none() {
            return Ok(());
        }
        let mut current = path.location().clone();
        let mut visited = Vec::new();
        loop {
            self.validate_observation()?;
            if current.ptr_eq(self.root.location()) {
                return self.validate_observation();
            }
            if visited.iter().any(|past: &Location| past.ptr_eq(&current)) {
                return Err(VfsError::WouldBlock);
            }
            visited.push(current.clone());
            match current.parent_step()? {
                ParentStep::Parent { parent, .. }
                | ParentStep::Unlinked {
                    parent: Some(parent),
                    ..
                } => current = parent,
                ParentStep::Covered(covered) => current = covered,
                ParentStep::Root | ParentStep::Unlinked { parent: None, .. } => {
                    self.validate_observation()?;
                    return Err(VfsError::CrossesDevices);
                }
            }
        }
    }
}

impl<'a> Continuation<'a> {
    pub(crate) fn finish_open(self, intent: LookupOpenIntent) -> VfsResult<ResolvedOpen> {
        let mut continuation = self;
        loop {
            let Self {
                mut session,
                entry,
                directory,
            } = continuation;
            let credentials = session.credentials;
            session.validate_searches()?;
            let (child, created) = match entry {
                FinalEntry::Resolved => {
                    if intent.node.create_new {
                        return Err(VfsError::AlreadyExists);
                    }
                    session.validate_result(&session.current)?;
                    return Ok(ResolvedOpen {
                        path: session.current,
                        created: false,
                    });
                }
                FinalEntry::Name(name) => {
                    session.check_search()?;
                    if directory && (intent.node.create || intent.node.create_new) {
                        return Err(VfsError::IsADirectory);
                    }
                    let result = if intent.node.create || intent.node.create_new {
                        // Missing is never creation authority. The actual slot
                        // owner observes and authorizes this fixed command.
                        session.validate_result(&session.current)?;
                        let mut node = intent.node.clone();
                        node.observation = session.observation;
                        let (location, created) = session
                            .current
                            .location()
                            .open_file_with_status(&name, &node, &credentials)?;
                        // The actual slot atomically checked the original
                        // naming observation before its own publication. Now
                        // verify the returned path against completed topology.
                        if session.observation.is_some() {
                            session.observation = Some(PathObservation::capture()?);
                        }
                        (session.bind(location)?, created)
                    } else {
                        (session.lookup(&name)?.ok_or(VfsError::NotFound)?, false)
                    };
                    session.remember_search();
                    result
                }
            };
            if !created && child.location().node_type() == NodeType::Symlink {
                if intent.follow || directory {
                    session.follow(child)?;
                    continuation = session.parent()?;
                    continue;
                }
                if !intent.path_only {
                    return Err(if intent.directory {
                        VfsError::NotADirectory
                    } else {
                        VfsError::FilesystemLoop
                    });
                }
            }
            if directory {
                child.location().check_is_dir()?;
            }
            session.validate_result(&child)?;
            return Ok(ResolvedOpen {
                path: child,
                created,
            });
        }
    }

    pub(crate) fn finish_parent(self) -> VfsResult<(PathHandle, String, Vec<PathHandle>)> {
        self.session.validate_searches()?;
        self.session.validate_result(&self.session.current)?;
        let (directory, name) = match self.entry {
            FinalEntry::Name(name) => {
                self.session.check_search()?;
                (self.session.current, name.into_owned())
            }
            FinalEntry::Resolved => super::FsView::resolved_parent_name(&self.session.current)?
                .ok_or(VfsError::InvalidInput)?,
        };
        Ok((directory, name, self.session.searched))
    }

    pub(crate) fn finish_lookup_with_trace(
        self,
        follow: bool,
    ) -> VfsResult<(PathHandle, Vec<PathHandle>)> {
        let mut continuation = self;
        loop {
            let Self {
                mut session,
                entry,
                directory,
            } = continuation;
            let FinalEntry::Name(name) = entry else {
                session.validate_searches()?;
                session.validate_result(&session.current)?;
                return Ok((session.current, session.searched));
            };
            let child = session.lookup(&name)?.ok_or(VfsError::NotFound)?;
            session.remember_search();
            if child.location().node_type() == NodeType::Symlink && (follow || directory) {
                session.follow(child)?;
                continuation = session.parent()?;
                continue;
            }
            if directory {
                child.location().check_is_dir()?;
            }
            session.validate_searches()?;
            session.validate_result(&child)?;
            return Ok((child, session.searched));
        }
    }
}

fn components(path: &str) -> VecDeque<Component<'_>> {
    let mut result = VecDeque::new();
    if path.starts_with('/') {
        result.push_back(Component::Root);
    }
    for name in path.split('/').filter(|name| !name.is_empty()) {
        result.push_back(match name {
            "." => Component::Current,
            ".." => Component::Parent,
            name => Component::Name(Cow::Borrowed(name)),
        });
    }
    if path.ends_with('/') {
        result.push_back(Component::Directory);
    }
    result
}

fn owned_components(path: &str) -> VecDeque<Component<'static>> {
    components(path)
        .into_iter()
        .map(|part| match part {
            Component::Name(name) => Component::Name(Cow::Owned(String::from(name))),
            Component::Root => Component::Root,
            Component::Current => Component::Current,
            Component::Parent => Component::Parent,
            Component::Directory => Component::Directory,
        })
        .collect()
}
