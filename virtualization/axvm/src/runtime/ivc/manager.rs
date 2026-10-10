//! Manager-owned IVC channel table and explicit lifecycle tokens.
//!
//! The table is keyed by the publisher's [`VmKey`] and the guest channel key so
//! that a recycled numeric VM ID can never alias a stale channel. Publisher and
//! subscriber endpoints keep their own [`RunId`] and are only notified while the
//! bound run is still active.
//!
//! Every prepare/commit step is an explicit token transition. Guest-mapping
//! installation and retirement happen in the owning control task, so the
//! manager never calls back into a device port while holding its table or
//! channel lock.

use std::{
    collections::BTreeMap,
    format,
    sync::{Arc, Mutex},
    vec::Vec,
};

use ax_memory_addr::PAGE_SIZE_4K;
use axvm_types::GuestPhysAddr;

use super::{
    aperture::{IvcApertureAllocator, IvcNotifyEndpoint},
    backing::SharedBacking,
};
use crate::{
    AxVmError, AxVmResult, RunId, VmKey, ax_err_type,
    guest_memory::{GuestRange, MappingLease, MemoryRevision},
    host::paging::{HostPagingHandler, PagingHandler},
    services::RunSignals,
    sync::MutexExt,
};

/// Maximum size of one IVC channel's shared region.
///
/// Larger publisher requests are truncated. The guest ABI receives the exact
/// granted size from the control owner and must check that value.
pub(crate) const MAX_IVC_CHANNEL_SIZE: usize = 0x100_0000;
const IVC_NOTIFY_PEER: usize = usize::MAX;

/// The fixed identity and bound capabilities of one prepared endpoint.
///
/// The publisher or subscriber control task resolves its own controller, notify
/// port, and run signals, then passes this plan to the manager. Keeping the plan
/// explicit bounds the prepare signatures and lets the manager validate that
/// `run` really belongs to `owner`.
pub(crate) struct IvcEndpointPlan {
    pub(crate) owner: VmKey,
    pub(crate) run: RunId,
    pub(crate) aperture: Arc<dyn IvcApertureAllocator>,
    pub(crate) notify: Option<Arc<dyn IvcNotifyEndpoint>>,
    pub(crate) signals: Arc<RunSignals>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ChannelKey {
    publisher: VmKey,
    key: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EndpointRole {
    Publisher,
    Subscriber,
}

/// The stable, generation-exact identity of one installed IVC binding.
///
/// A binding is identified by its publisher instance and channel key; a
/// subscriber binding additionally records the subscriber instance so that the
/// exact peer can be detached later.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IvcBindingKey {
    pub(crate) publisher: VmKey,
    pub(crate) key: usize,
    pub(crate) subscriber: Option<VmKey>,
}

/// An installed endpoint identity retained by its VM control owner.
pub(crate) struct IvcBinding {
    key: IvcBindingKey,
}

impl IvcBinding {
    pub(crate) const fn key(&self) -> IvcBindingKey {
        self.key
    }
}

/// One endpoint's durable identity and locally bound capabilities.
#[derive(Clone)]
struct EndpointState {
    vm: VmKey,
    run: RunId,
    range: GuestRange,
    aperture: Arc<dyn IvcApertureAllocator>,
    notify: Option<Arc<dyn IvcNotifyEndpoint>>,
    signals: Arc<RunSignals>,
    /// Set once the control owner confirmed the guest mapping is installed.
    installed: bool,
    /// Set by a prepare step to block new operations on this endpoint.
    closing: bool,
    /// Set while one teardown token is releasing this endpoint, so a competing
    /// duplicate commit cannot issue a second aperture release.
    releasing: bool,
    /// The translation revision that installed this endpoint's mapping.
    installed_revision: Option<MemoryRevision>,
}

struct ChannelState<H: PagingHandler> {
    key: ChannelKey,
    /// True while a publisher endpoint is installed, i.e. the channel is
    /// published. Unpublishing clears this without touching shared pages.
    published: bool,
    /// True once both endpoints are gone and the shared backing may be dropped.
    retired: bool,
    publisher: Option<EndpointState>,
    subscriber: Option<EndpointState>,
    shared: SharedBacking<H>,
}

impl<H: PagingHandler + 'static> ChannelState<H> {
    fn endpoint(&self, role: EndpointRole) -> Option<&EndpointState> {
        match role {
            EndpointRole::Publisher => self.publisher.as_ref(),
            EndpointRole::Subscriber => self.subscriber.as_ref(),
        }
    }

    fn endpoint_mut(&mut self, role: EndpointRole) -> Option<&mut EndpointState> {
        match role {
            EndpointRole::Publisher => self.publisher.as_mut(),
            EndpointRole::Subscriber => self.subscriber.as_mut(),
        }
    }

    fn take_endpoint(&mut self, role: EndpointRole) -> Option<EndpointState> {
        match role {
            EndpointRole::Publisher => self.publisher.take(),
            EndpointRole::Subscriber => self.subscriber.take(),
        }
    }

    fn retire_if_empty(&mut self) {
        if self.publisher.is_none() && self.subscriber.is_none() {
            self.retired = true;
        }
    }
}

/// The cross-VM channel table owned by one VM manager instance.
pub(crate) struct IvcManager<H: PagingHandler = HostPagingHandler> {
    channels: Mutex<BTreeMap<ChannelKey, Arc<Mutex<ChannelState<H>>>>>,
}

impl<H: PagingHandler + 'static> IvcManager<H> {
    pub(crate) fn new() -> Self {
        Self {
            channels: Mutex::new(BTreeMap::new()),
        }
    }

    /// Reserves a publisher endpoint, its guest aperture range, and its shared
    /// backing. Nothing is visible to peers until [`IvcAttach::commit`].
    pub(crate) fn prepare_publish(
        self: &Arc<Self>,
        key: usize,
        size: usize,
        minimum_subscribers: usize,
        plan: IvcEndpointPlan,
    ) -> AxVmResult<IvcAttach<H>> {
        let IvcEndpointPlan {
            owner,
            run,
            aperture,
            notify,
            signals,
        } = plan;
        if run.vm() != owner {
            return Err(AxVmError::invalid_input(
                "publish IVC channel",
                "run does not belong to the publisher identity",
            ));
        }
        if minimum_subscribers > 1 {
            return Err(AxVmError::unsupported(
                "publish IVC channel",
                "the AXIVC SPSC protocol supports at most one subscriber",
            ));
        }

        let granted_size = granted_publisher_size(size)?;
        let channel_key = ChannelKey {
            publisher: owner,
            key,
        };

        // Reject a duplicate identity before allocating any backing.
        if self.channels.lock_unpoisoned().contains_key(&channel_key) {
            return Err(ax_err_type!(AlreadyExists, "IVC channel already exists"));
        }

        let guest = allocate_aperture(&aperture, granted_size, "publish IVC channel")?;
        let aperture_for_cleanup = aperture.clone();
        let shared = match SharedBacking::new(owner.vm_id(), key, granted_size) {
            Ok(shared) => shared,
            Err(error) => {
                return Err(cleanup_unused_aperture(
                    &aperture,
                    guest,
                    granted_size,
                    error,
                ));
            }
        };
        let mapping = match shared.lease(guest, granted_size) {
            Ok(mapping) => mapping,
            Err(error) => {
                return Err(cleanup_unused_aperture(
                    &aperture,
                    guest,
                    granted_size,
                    error,
                ));
            }
        };

        let endpoint = EndpointState {
            vm: owner,
            run,
            range: mapping.range,
            aperture,
            notify,
            signals,
            installed: false,
            closing: false,
            releasing: false,
            installed_revision: None,
        };
        let state = Arc::new(Mutex::new(ChannelState {
            key: channel_key,
            published: false,
            retired: false,
            publisher: Some(endpoint),
            subscriber: None,
            shared,
        }));

        {
            let mut channels = self.channels.lock_unpoisoned();
            if channels.contains_key(&channel_key) {
                drop(channels);
                drop(state);
                return Err(cleanup_unused_aperture(
                    &aperture_for_cleanup,
                    guest,
                    granted_size,
                    ax_err_type!(AlreadyExists, "IVC channel already exists"),
                ));
            }
            channels.insert(channel_key, state.clone());
        }

        Ok(IvcAttach {
            manager: self.clone(),
            channel_key,
            channel: state,
            role: EndpointRole::Publisher,
            vm: owner,
            run,
            mapping,
        })
    }

    /// Reserves a subscriber endpoint on an already-published channel.
    pub(crate) fn prepare_subscribe(
        self: &Arc<Self>,
        publisher_vm_id: usize,
        key: usize,
        requested_size: usize,
        plan: IvcEndpointPlan,
    ) -> AxVmResult<IvcAttach<H>> {
        let IvcEndpointPlan {
            owner: subscriber,
            run,
            aperture,
            notify,
            signals,
        } = plan;
        if run.vm() != subscriber {
            return Err(AxVmError::invalid_input(
                "subscribe IVC channel",
                "run does not belong to the subscriber identity",
            ));
        }
        if requested_size == 0 {
            return Err(AxVmError::invalid_input(
                "subscribe IVC channel",
                "requested size must be greater than zero",
            ));
        }
        let requested_pages = align_up_to_page(requested_size, "subscribe IVC channel")?;

        for channel in self.channels_for_numeric_publisher(publisher_vm_id, key) {
            let mut state = channel.lock_unpoisoned();
            if state.retired || !state.published {
                continue;
            }
            let Some(publisher) = state.publisher.as_ref() else {
                continue;
            };
            if !publisher.installed || publisher.closing {
                continue;
            }
            if let Some(endpoint) = state.subscriber.as_ref() {
                let reason = if endpoint.vm == subscriber {
                    "this VM already owns the IVC subscriber slot"
                } else {
                    "the SPSC IVC channel already has another subscriber"
                };
                return Err(ax_err_type!(AlreadyExists, reason));
            }

            let granted_size = requested_pages.min(state.shared.size());
            let guest = match allocate_aperture(&aperture, granted_size, "subscribe IVC channel") {
                Ok(guest) => guest,
                Err(error) => {
                    // Nothing was allocated, and no callback may run under the
                    // channel lock.
                    drop(state);
                    return Err(error);
                }
            };
            let mapping = match state.shared.lease(guest, granted_size) {
                Ok(mapping) => mapping,
                Err(error) => {
                    drop(state);
                    return Err(cleanup_unused_aperture(
                        &aperture,
                        guest,
                        granted_size,
                        error,
                    ));
                }
            };
            let range = match GuestRange::new(guest, granted_size) {
                Ok(range) => range,
                Err(error) => {
                    drop(state);
                    return Err(cleanup_unused_aperture(
                        &aperture,
                        guest,
                        granted_size,
                        error,
                    ));
                }
            };

            let channel_key = state.key;
            state.subscriber = Some(EndpointState {
                vm: subscriber,
                run,
                range,
                aperture: aperture.clone(),
                notify,
                signals,
                installed: false,
                closing: false,
                releasing: false,
                installed_revision: None,
            });
            drop(state);

            return Ok(IvcAttach {
                manager: self.clone(),
                channel_key,
                channel,
                role: EndpointRole::Subscriber,
                vm: subscriber,
                run,
                mapping,
            });
        }

        Err(ax_err_type!(
            NotFound,
            format!(
                "published IVC channel for publisher VM [{publisher_vm_id}] key {key:#x} not found"
            )
        ))
    }

    /// Closes the publisher endpoint to new operations and returns the token
    /// that releases its guest binding after the owner confirms retirement.
    ///
    /// The publisher endpoint may still be an uninstalled reservation: a failed
    /// install must remain reclaimable, and unpublishing never requires the peer
    /// to unmap first.
    pub(crate) fn prepare_unpublish(
        self: &Arc<Self>,
        owner: VmKey,
        key: usize,
    ) -> AxVmResult<IvcDetach<H>> {
        let channel_key = ChannelKey {
            publisher: owner,
            key,
        };
        let channel = self.channel_arc(channel_key)?;
        let mut state = channel.lock_unpoisoned();
        let Some(endpoint) = state
            .endpoint_mut(EndpointRole::Publisher)
            .filter(|endpoint| endpoint.vm == owner && !endpoint.releasing)
        else {
            return Err(ax_err_type!(
                NotFound,
                format!(
                    "VM[{}] has no reclaimable publisher endpoint VM[{}] key {key:#x}",
                    owner.vm_id(),
                    channel_key.publisher.vm_id()
                )
            ));
        };
        endpoint.closing = true;
        let detach = Self::detach_for(
            self,
            &channel,
            channel_key,
            EndpointRole::Publisher,
            endpoint,
        );
        drop(state);
        Ok(detach)
    }

    /// Closes one subscriber endpoint to new operations.
    pub(crate) fn prepare_unsubscribe(
        self: &Arc<Self>,
        publisher_vm_id: usize,
        key: usize,
        subscriber: VmKey,
    ) -> AxVmResult<IvcDetach<H>> {
        for channel in self.channels_for_numeric_publisher(publisher_vm_id, key) {
            let mut state = channel.lock_unpoisoned();
            let channel_key = state.key;
            let Some(endpoint) = state
                .endpoint_mut(EndpointRole::Subscriber)
                .filter(|endpoint| endpoint.vm == subscriber && !endpoint.releasing)
            else {
                continue;
            };
            endpoint.closing = true;
            let detach = Self::detach_for(
                self,
                &channel,
                channel_key,
                EndpointRole::Subscriber,
                endpoint,
            );
            drop(state);
            return Ok(detach);
        }
        Err(missing_subscriber_error(publisher_vm_id, key, subscriber))
    }

    /// Closes every endpoint owned by `owner`, in either role, whether or not its
    /// guest mapping was ever installed.
    ///
    /// The run owner has already stopped all vCPUs and invalidated both roots, so
    /// it alone decides per binding whether to unmap or whether no mapping
    /// exists; the manager only returns the tokens that must be committed after
    /// that decision. Uninstalled reservations are included so a failed install
    /// can always be reclaimed.
    pub(crate) fn prepare_teardown(
        self: &Arc<Self>,
        owner: VmKey,
    ) -> AxVmResult<Vec<IvcDetach<H>>> {
        let channels = {
            let table = self.channels.lock_unpoisoned();
            table
                .iter()
                .map(|(key, channel)| (*key, channel.clone()))
                .collect::<Vec<_>>()
        };

        let mut detachments = Vec::new();
        for (channel_key, channel) in channels {
            let mut state = channel.lock_unpoisoned();
            if state.retired {
                continue;
            }
            if let Some(endpoint) = state
                .publisher
                .as_mut()
                .filter(|endpoint| endpoint.vm == owner && !endpoint.releasing)
            {
                endpoint.closing = true;
                detachments.push(Self::detach_for(
                    self,
                    &channel,
                    channel_key,
                    EndpointRole::Publisher,
                    endpoint,
                ));
            }
            if let Some(endpoint) = state
                .subscriber
                .as_mut()
                .filter(|endpoint| endpoint.vm == owner && !endpoint.releasing)
            {
                endpoint.closing = true;
                detachments.push(Self::detach_for(
                    self,
                    &channel,
                    channel_key,
                    EndpointRole::Subscriber,
                    endpoint,
                ));
            }
        }
        Ok(detachments)
    }

    /// Pulses the target run's bound notify port and wakes its deferred worker.
    ///
    /// `source` must still be the live publisher or the live subscriber of the
    /// addressed channel, and both endpoints must belong to the run currently
    /// installed in the table. The notify endpoint and signal handle are cloned
    /// out of the table lock before any device call or wake.
    pub(crate) fn notify_channel(
        self: &Arc<Self>,
        publisher_vm_id: usize,
        key: usize,
        source: VmKey,
        target_vm_id: usize,
    ) -> AxVmResult<()> {
        let Some(notification) =
            self.resolve_notification(publisher_vm_id, key, source, target_vm_id)
        else {
            return Err(notification_not_found(
                publisher_vm_id,
                key,
                source,
                target_vm_id,
            ));
        };

        let Some(endpoint) = notification.target.notify.as_ref() else {
            return Err(AxVmError::resource_unavailable(
                "IVC notify endpoint",
                "the target run has no bound notify port",
            ));
        };
        endpoint.notify().map_err(|error| {
            AxVmError::resource_unavailable(
                "IVC notify endpoint",
                format!("the target notify port is closed: {error}"),
            )
        })?;
        notification.target.signals.notify_work().map_err(|error| {
            AxVmError::interrupt("wake IVC notify target", format!("{error:?}"))
        })?;
        Ok(())
    }

    fn detach_for(
        manager: &Arc<Self>,
        channel: &Arc<Mutex<ChannelState<H>>>,
        channel_key: ChannelKey,
        role: EndpointRole,
        endpoint: &EndpointState,
    ) -> IvcDetach<H> {
        IvcDetach {
            manager: manager.clone(),
            channel_key,
            channel: channel.clone(),
            role,
            vm: endpoint.vm,
            run: endpoint.run,
            range: endpoint.range,
            aperture: endpoint.aperture.clone(),
        }
    }

    fn channel_arc(&self, channel_key: ChannelKey) -> AxVmResult<Arc<Mutex<ChannelState<H>>>> {
        self.channels
            .lock_unpoisoned()
            .get(&channel_key)
            .cloned()
            .ok_or_else(|| {
                ax_err_type!(
                    NotFound,
                    format!(
                        "IVC channel for publisher VM [{}] key {:#x} not found",
                        channel_key.publisher.vm_id(),
                        channel_key.key
                    )
                )
            })
    }

    fn channels_for_numeric_publisher(
        &self,
        publisher_vm_id: usize,
        key: usize,
    ) -> Vec<Arc<Mutex<ChannelState<H>>>> {
        let table = self.channels.lock_unpoisoned();
        table
            .iter()
            .filter(|(channel_key, _)| {
                channel_key.publisher.vm_id() == publisher_vm_id && channel_key.key == key
            })
            .map(|(_, channel)| channel.clone())
            .collect()
    }

    fn resolve_notification(
        &self,
        publisher_vm_id: usize,
        key: usize,
        source: VmKey,
        target_vm_id: usize,
    ) -> Option<NotificationEndpoint> {
        for channel in self.channels_for_numeric_publisher(publisher_vm_id, key) {
            let state = channel.lock_unpoisoned();
            if state.retired || !state.published {
                continue;
            }
            let Some(publisher) = state.publisher.as_ref() else {
                continue;
            };
            let Some(subscriber) = state.subscriber.as_ref() else {
                continue;
            };
            if !publisher.installed
                || !subscriber.installed
                || publisher.closing
                || subscriber.closing
            {
                continue;
            }

            let (source_endpoint, target_endpoint) = if source == state.key.publisher {
                (publisher, subscriber)
            } else if source == subscriber.vm {
                (subscriber, publisher)
            } else {
                continue;
            };
            if !endpoint_is_active(source_endpoint) || !endpoint_is_active(target_endpoint) {
                continue;
            }

            let resolved_target = if target_vm_id == IVC_NOTIFY_PEER {
                target_endpoint.vm.vm_id()
            } else {
                target_vm_id
            };
            if resolved_target != target_endpoint.vm.vm_id() {
                continue;
            }

            return Some(NotificationEndpoint {
                target: target_endpoint.clone(),
            });
        }
        None
    }

    fn remove_channel_if_retired(&self, key: ChannelKey, channel: &Arc<Mutex<ChannelState<H>>>) {
        let retired = {
            let state = channel.lock_unpoisoned();
            state.retired
        };
        if !retired {
            return;
        }

        let removed = {
            let mut table = self.channels.lock_unpoisoned();
            match table.get(&key) {
                Some(published) if Arc::ptr_eq(published, channel) => table.remove(&key),
                _ => None,
            }
        };
        // Drop the retired channel, and therefore any shared backing, with no
        // channel or table lock held so the frame owner runs outside the lock.
        drop(removed);
    }
}

struct NotificationEndpoint {
    target: EndpointState,
}

/// A run is only a valid notify peer while its bound signals report the same
/// run identity and at least one live vCPU.
fn endpoint_is_active(endpoint: &EndpointState) -> bool {
    endpoint.signals.run_id() == endpoint.run && endpoint.signals.active_mask() != 0
}

/// A mapping prepared by the manager and not yet acknowledged as installed.
pub(crate) struct IvcAttach<H: PagingHandler> {
    manager: Arc<IvcManager<H>>,
    channel_key: ChannelKey,
    channel: Arc<Mutex<ChannelState<H>>>,
    role: EndpointRole,
    vm: VmKey,
    run: RunId,
    mapping: MappingLease,
}

impl<H: PagingHandler + 'static> IvcAttach<H> {
    pub(crate) fn mapping(&self) -> MappingLease {
        self.mapping.clone()
    }

    pub(crate) fn guest_addr(&self) -> GuestPhysAddr {
        self.mapping.range.start
    }

    pub(crate) fn size(&self) -> usize {
        self.mapping.range.length
    }

    /// Acknowledges that the owning run installed the returned mapping at the
    /// given translation revision, publishing the endpoint.
    ///
    /// On failure the token is consumed but the reservation stays in the table,
    /// so the run owner can still reclaim it through `prepare_teardown`.
    pub(crate) fn commit(self, revision: MemoryRevision) -> AxVmResult<IvcBinding> {
        if revision.run != self.run {
            return Err(AxVmError::invalid_input(
                "install IVC mapping",
                "revision belongs to a different run",
            ));
        }

        let mut state = self.channel.lock_unpoisoned();
        if state.key != self.channel_key || state.retired {
            return Err(AxVmError::invalid_state(
                "install IVC mapping",
                "reserved IVC channel was retired",
            ));
        }
        let publisher_installed = state
            .publisher
            .as_ref()
            .is_some_and(|publisher| publisher.installed);
        if self.role == EndpointRole::Subscriber && (!state.published || !publisher_installed) {
            return Err(AxVmError::invalid_state(
                "install IVC mapping",
                "publisher endpoint is no longer installed",
            ));
        }

        {
            let Some(endpoint) = state.endpoint_mut(self.role) else {
                return Err(AxVmError::invalid_state(
                    "install IVC mapping",
                    "IVC attachment reservation is no longer cancellable",
                ));
            };
            if endpoint.vm != self.vm
                || endpoint.run != self.run
                || endpoint.installed
                || endpoint.closing
            {
                return Err(AxVmError::invalid_state(
                    "install IVC mapping",
                    "IVC attachment reservation is no longer cancellable",
                ));
            }
            endpoint.installed = true;
            endpoint.installed_revision = Some(revision);
        }
        if self.role == EndpointRole::Publisher {
            state.published = true;
        }

        Ok(IvcBinding {
            key: IvcBindingKey {
                publisher: state.key.publisher,
                key: state.key.key,
                subscriber: (self.role == EndpointRole::Subscriber).then_some(self.vm),
            },
        })
    }

    /// Explicit rollback for a reservation that is known not to be installed.
    ///
    /// The aperture is released outside the state lock. If that release fails,
    /// the closing reservation stays in the table so it can be retried through
    /// [`IvcManager::prepare_teardown`]; no path leaves the endpoint owned by
    /// nobody.
    pub(crate) fn cancel_uninstalled(self) -> AxVmResult<()> {
        let aperture = {
            let mut state = self.channel.lock_unpoisoned();
            if state.key != self.channel_key || state.retired {
                return Err(AxVmError::invalid_state(
                    "cancel IVC attachment",
                    "reserved IVC channel was retired",
                ));
            }
            let Some(endpoint) = state.endpoint_mut(self.role) else {
                return Err(AxVmError::invalid_state(
                    "cancel IVC attachment",
                    "attachment was already installed, closed, or retired",
                ));
            };
            if endpoint.vm != self.vm
                || endpoint.run != self.run
                || endpoint.installed
                || endpoint.closing
                || endpoint.releasing
            {
                return Err(AxVmError::invalid_state(
                    "cancel IVC attachment",
                    "attachment was already installed, closed, or retired",
                ));
            }
            endpoint.closing = true;
            endpoint.releasing = true;
            endpoint.aperture.clone()
        };

        if let Err(error) = aperture.release(self.mapping.range.start, self.mapping.range.length) {
            let mut state = self.channel.lock_unpoisoned();
            if let Some(endpoint) = state.endpoint_mut(self.role)
                && endpoint.vm == self.vm
                && endpoint.run == self.run
                && endpoint.closing
            {
                endpoint.releasing = false;
            }
            drop(state);
            return Err(AxVmError::device("cancel IVC attachment", error));
        }

        let removed = {
            let mut state = self.channel.lock_unpoisoned();
            let exact = state.endpoint(self.role).is_some_and(|endpoint| {
                endpoint.vm == self.vm
                    && endpoint.run == self.run
                    && !endpoint.installed
                    && endpoint.closing
                    && endpoint.releasing
            });
            if exact {
                let removed = state.take_endpoint(self.role);
                if self.role == EndpointRole::Publisher {
                    state.published = false;
                }
                state.retire_if_empty();
                removed
            } else {
                None
            }
        };
        drop(removed);

        self.manager
            .remove_channel_if_retired(self.channel_key, &self.channel);
        Ok(())
    }
}

/// An explicit teardown token for one guest binding, installed or uninstalled.
pub(crate) struct IvcDetach<H: PagingHandler> {
    manager: Arc<IvcManager<H>>,
    channel_key: ChannelKey,
    channel: Arc<Mutex<ChannelState<H>>>,
    role: EndpointRole,
    vm: VmKey,
    run: RunId,
    range: GuestRange,
    aperture: Arc<dyn IvcApertureAllocator>,
}

impl<H: PagingHandler + 'static> Clone for IvcDetach<H> {
    fn clone(&self) -> Self {
        Self {
            manager: self.manager.clone(),
            channel_key: self.channel_key,
            channel: self.channel.clone(),
            role: self.role,
            vm: self.vm,
            run: self.run,
            range: self.range,
            aperture: self.aperture.clone(),
        }
    }
}

impl<H: PagingHandler + 'static> IvcDetach<H> {
    pub(crate) const fn range(&self) -> GuestRange {
        self.range
    }

    pub(crate) const fn vm(&self) -> VmKey {
        self.vm
    }

    pub(crate) const fn run(&self) -> RunId {
        self.run
    }

    /// Releases the endpoint's guest aperture binding after the owning run has
    /// confirmed the retirement of `retired`.
    ///
    /// The release is reserved under the state lock and performed outside it, so
    /// no device callback runs while the channel lock is held. A release failure
    /// clears the reservation and keeps the endpoint reclaimable; a competing
    /// duplicate commit observes the reservation and cannot issue a second
    /// release. The final removal re-checks the exact identity and drops the
    /// endpoint outside the lock.
    pub(crate) fn commit(self, retired: MemoryRevision) -> AxVmResult<()> {
        if retired.run != self.run {
            return Err(AxVmError::invalid_input(
                "commit IVC teardown",
                "retirement belongs to a different run",
            ));
        }

        let aperture = {
            let mut state = self.channel.lock_unpoisoned();
            if state.key != self.channel_key {
                return Err(AxVmError::invalid_state(
                    "commit IVC teardown",
                    "IVC channel identity changed",
                ));
            }
            let Some(endpoint) = state.endpoint_mut(self.role) else {
                return Err(AxVmError::invalid_state(
                    "commit IVC teardown",
                    "IVC endpoint was already retired",
                ));
            };
            if endpoint.vm != self.vm || endpoint.run != self.run || !endpoint.closing {
                return Err(AxVmError::invalid_state(
                    "commit IVC teardown",
                    "IVC endpoint was already retired",
                ));
            }
            if endpoint.releasing {
                return Err(AxVmError::invalid_state(
                    "commit IVC teardown",
                    "IVC teardown is already in progress",
                ));
            }
            if let Some(installed) = endpoint.installed_revision
                && retired.sequence <= installed.sequence
            {
                return Err(AxVmError::invalid_input(
                    "commit IVC teardown",
                    "retirement revision did not advance past the installed mapping",
                ));
            }
            endpoint.releasing = true;
            endpoint.aperture.clone()
        };

        if let Err(error) = aperture.release(self.range.start, self.range.length) {
            let mut state = self.channel.lock_unpoisoned();
            if let Some(endpoint) = state.endpoint_mut(self.role)
                && endpoint.vm == self.vm
                && endpoint.run == self.run
                && endpoint.closing
            {
                endpoint.releasing = false;
            }
            drop(state);
            return Err(AxVmError::device("release IVC aperture", error));
        }

        let removed = {
            let mut state = self.channel.lock_unpoisoned();
            let exact = state.endpoint(self.role).is_some_and(|endpoint| {
                endpoint.vm == self.vm && endpoint.run == self.run && endpoint.closing
            });
            if exact {
                let removed = state.take_endpoint(self.role);
                if self.role == EndpointRole::Publisher {
                    // Unpublishing never requires a peer to unmap first; the
                    // shared pages and any peer binding are retained until the
                    // peer detaches.
                    state.published = false;
                }
                state.retire_if_empty();
                removed
            } else {
                None
            }
        };
        drop(removed);

        self.manager
            .remove_channel_if_retired(self.channel_key, &self.channel);
        Ok(())
    }
}

fn allocate_aperture(
    aperture: &Arc<dyn IvcApertureAllocator>,
    size: usize,
    operation: &'static str,
) -> AxVmResult<GuestPhysAddr> {
    aperture.allocate(size).map_err(|error| {
        let detail = format!("{error}");
        AxVmError::resource_unavailable("IVC aperture", format_args!("{operation}: {detail}"))
    })
}

/// Releases a range that was allocated for a reservation that never installed.
fn cleanup_unused_aperture(
    aperture: &Arc<dyn IvcApertureAllocator>,
    guest: GuestPhysAddr,
    size: usize,
    primary: AxVmError,
) -> AxVmError {
    match aperture.release(guest, size) {
        Ok(()) => primary,
        Err(rollback) => AxVmError::lifecycle_rollback("prepare IVC attachment", primary, rollback),
    }
}

fn granted_publisher_size(requested: usize) -> AxVmResult<usize> {
    if requested == 0 {
        return Err(AxVmError::invalid_input(
            "publish IVC channel",
            "size must be greater than zero",
        ));
    }
    let aligned = align_up_to_page(requested, "publish IVC channel")?;
    Ok(aligned.min(MAX_IVC_CHANNEL_SIZE))
}

fn align_up_to_page(size: usize, operation: &'static str) -> AxVmResult<usize> {
    size.checked_add(PAGE_SIZE_4K - 1)
        .map(|value| value - (value % PAGE_SIZE_4K))
        .ok_or_else(|| AxVmError::invalid_input(operation, "aligned size overflows"))
}

fn missing_subscriber_error(publisher_vm_id: usize, key: usize, subscriber: VmKey) -> AxVmError {
    ax_err_type!(
        NotFound,
        format!(
            "VM[{}] is not subscribed to publisher VM[{publisher_vm_id}] key {key:#x}",
            subscriber.vm_id()
        )
    )
}

fn notification_not_found(
    publisher_vm_id: usize,
    key: usize,
    source: VmKey,
    target_vm_id: usize,
) -> AxVmError {
    ax_err_type!(
        InvalidInput,
        format!(
            "VM[{}] cannot notify VM[{target_vm_id}] on publisher VM[{publisher_vm_id}] key \
             {key:#x}",
            source.vm_id()
        )
    )
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use ax_memory_addr::PAGE_SIZE_4K;
    use axdevice::{DeviceManagerError, DeviceManagerResult};
    use axvm_types::GuestPhysAddr;

    use super::{IvcApertureAllocator, IvcEndpointPlan, IvcManager, IvcNotifyEndpoint};
    use crate::{
        RunId, VmKey, guest_memory::MemoryRevision, host::paging::test_frames::TestFrames,
        services::RunSignals,
    };

    /// Records every aperture allocation so a test can prove each range is
    /// released exactly once and never leaked.
    struct TrackingAperture {
        inner: Mutex<TrackingState>,
    }

    struct TrackingState {
        next: usize,
        outstanding: Vec<(usize, usize)>,
        released: Vec<(usize, usize)>,
        fail_next_release: bool,
    }

    impl TrackingAperture {
        fn new(base: usize) -> Arc<Self> {
            Arc::new(Self {
                inner: Mutex::new(TrackingState {
                    next: base,
                    outstanding: Vec::new(),
                    released: Vec::new(),
                    fail_next_release: false,
                }),
            })
        }

        fn fail_next_release(&self) {
            self.inner.lock().unwrap().fail_next_release = true;
        }

        fn outstanding(&self) -> usize {
            self.inner.lock().unwrap().outstanding.len()
        }

        fn release_count(&self) -> usize {
            self.inner.lock().unwrap().released.len()
        }
    }

    impl IvcApertureAllocator for TrackingAperture {
        fn allocate(&self, size: usize) -> DeviceManagerResult<GuestPhysAddr> {
            let mut state = self.inner.lock().unwrap();
            let base = state.next;
            state.next = state
                .next
                .checked_add(size)
                .expect("test aperture overflow");
            state.outstanding.push((base, size));
            Ok(GuestPhysAddr::from_usize(base))
        }

        fn release(&self, addr: GuestPhysAddr, size: usize) -> DeviceManagerResult {
            let mut state = self.inner.lock().unwrap();
            if state.fail_next_release {
                state.fail_next_release = false;
                return Err(DeviceManagerError::InvalidState {
                    operation: "release test aperture range",
                    detail: "injected release failure".into(),
                });
            }
            let entry = (addr.as_usize(), size);
            let index = state
                .outstanding
                .iter()
                .position(|candidate| *candidate == entry)
                .ok_or_else(|| DeviceManagerError::InvalidInput {
                    operation: "release test aperture range",
                    detail: "range is not outstanding".into(),
                })?;
            state.outstanding.remove(index);
            state.released.push(entry);
            Ok(())
        }
    }

    /// A notify endpoint that counts pulses; a silent run never pulses it.
    struct CountingNotify {
        pulses: AtomicUsize,
    }

    impl CountingNotify {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                pulses: AtomicUsize::new(0),
            })
        }

        fn pulses(&self) -> usize {
            self.pulses.load(Ordering::Relaxed)
        }
    }

    impl IvcNotifyEndpoint for CountingNotify {
        fn notify(&self) -> DeviceManagerResult {
            self.pulses.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn plan(
        owner: VmKey,
        run: RunId,
        aperture: &Arc<TrackingAperture>,
        notify: &Arc<CountingNotify>,
    ) -> IvcEndpointPlan {
        let notify_dyn: Option<Arc<dyn IvcNotifyEndpoint>> = Some(notify.clone());
        IvcEndpointPlan {
            owner,
            run,
            aperture: aperture.clone(),
            notify: notify_dyn,
            signals: RunSignals::new(run, 1).unwrap(),
        }
    }

    fn revision(run: RunId, sequence: u64) -> MemoryRevision {
        MemoryRevision { run, sequence }
    }

    #[test]
    fn publish_subscribe_commit_and_teardown_release_each_range_once() {
        let manager = Arc::new(IvcManager::<TestFrames>::new());
        let publisher = VmKey::new(1, 1);
        let subscriber = VmKey::new(2, 1);
        let publisher_run = RunId::new(publisher, 1);
        let subscriber_run = RunId::new(subscriber, 1);
        let publisher_aperture = TrackingAperture::new(0x1000_0000);
        let subscriber_aperture = TrackingAperture::new(0x2000_0000);
        let publisher_notify = CountingNotify::new();
        let subscriber_notify = CountingNotify::new();

        let publish = manager
            .prepare_publish(
                0x51,
                PAGE_SIZE_4K,
                1,
                plan(
                    publisher,
                    publisher_run,
                    &publisher_aperture,
                    &publisher_notify,
                ),
            )
            .unwrap();
        assert_eq!(publish.guest_addr(), GuestPhysAddr::from_usize(0x1000_0000));
        assert_eq!(publish.size(), PAGE_SIZE_4K);
        let published = publish.commit(revision(publisher_run, 1)).unwrap();
        assert_eq!(published.key().publisher, publisher);
        assert_eq!(published.key().key, 0x51);
        assert_eq!(published.key().subscriber, None);

        let subscribe = manager
            .prepare_subscribe(
                1,
                0x51,
                PAGE_SIZE_4K,
                plan(
                    subscriber,
                    subscriber_run,
                    &subscriber_aperture,
                    &subscriber_notify,
                ),
            )
            .unwrap();
        let subscribed = subscribe.commit(revision(subscriber_run, 1)).unwrap();
        assert_eq!(subscribed.key().subscriber, Some(subscriber));

        // A silent run has no active vCPU, so the notify request is refused and
        // the target port is never pulsed.
        assert!(
            manager
                .notify_channel(1, 0x51, publisher, usize::MAX)
                .is_err()
        );
        assert_eq!(publisher_notify.pulses(), 0);
        assert_eq!(subscriber_notify.pulses(), 0);

        // Unpublishing the publisher keeps the subscriber binding alive.
        let unpublish = manager.prepare_unpublish(publisher, 0x51).unwrap();
        unpublish.commit(revision(publisher_run, 2)).unwrap();
        assert_eq!(publisher_aperture.outstanding(), 0);
        assert_eq!(publisher_aperture.release_count(), 1);
        assert_eq!(subscriber_aperture.outstanding(), 1);

        // The last peer retires the channel and releases its own range.
        let mut teardown = manager.prepare_teardown(subscriber).unwrap();
        assert_eq!(teardown.len(), 1);
        let detach = teardown.pop().unwrap();
        assert_eq!(detach.vm(), subscriber);
        detach.commit(revision(subscriber_run, 2)).unwrap();
        assert_eq!(subscriber_aperture.outstanding(), 0);
        assert_eq!(subscriber_aperture.release_count(), 1);

        // The retired channel is gone, so another unpublish cannot find it.
        assert!(manager.prepare_unpublish(publisher, 0x51).is_err());
    }

    #[test]
    fn teardown_reclaims_a_reservation_left_by_a_failed_commit() {
        let manager = Arc::new(IvcManager::<TestFrames>::new());
        let owner = VmKey::new(7, 1);
        let owner_run = RunId::new(owner, 1);
        let aperture = TrackingAperture::new(0x3000_0000);
        let notify = CountingNotify::new();

        let attach = manager
            .prepare_publish(
                0x77,
                PAGE_SIZE_4K,
                1,
                plan(owner, owner_run, &aperture, &notify),
            )
            .unwrap();
        let foreign_run = RunId::new(VmKey::new(9, 1), 1);
        // The commit fails after a (simulated) successful root update, consumes
        // the token, and leaves the reservation in the table.
        assert!(attach.commit(revision(foreign_run, 1)).is_err());
        assert_eq!(aperture.outstanding(), 1);

        // Teardown reclaims the never-installed reservation.
        let mut teardown = manager.prepare_teardown(owner).unwrap();
        assert_eq!(teardown.len(), 1);
        let detach = teardown.pop().unwrap();
        assert_eq!(detach.vm(), owner);
        assert_eq!(detach.range().start, GuestPhysAddr::from_usize(0x3000_0000));
        detach.commit(revision(owner_run, 1)).unwrap();
        assert_eq!(aperture.outstanding(), 0);
        assert_eq!(aperture.release_count(), 1);
    }

    #[test]
    fn failed_cancel_uninstalled_stays_reclaimable_through_teardown() {
        let manager = Arc::new(IvcManager::<TestFrames>::new());
        let owner = VmKey::new(8, 1);
        let owner_run = RunId::new(owner, 1);
        let aperture = TrackingAperture::new(0xa000_0000);
        let notify = CountingNotify::new();

        let attach = manager
            .prepare_publish(
                0x88,
                PAGE_SIZE_4K,
                1,
                plan(owner, owner_run, &aperture, &notify),
            )
            .unwrap();
        aperture.fail_next_release();
        assert!(attach.cancel_uninstalled().is_err());
        assert_eq!(aperture.outstanding(), 1);

        // The abandoned reservation is still owned by the VM and can be
        // reclaimed after the cancel failed.
        let mut teardown = manager.prepare_teardown(owner).unwrap();
        assert_eq!(teardown.len(), 1);
        teardown
            .pop()
            .unwrap()
            .commit(revision(owner_run, 1))
            .unwrap();
        assert_eq!(aperture.outstanding(), 0);
        assert_eq!(aperture.release_count(), 1);
    }

    #[test]
    fn failed_aperture_release_keeps_the_detach_retryable() {
        let manager = Arc::new(IvcManager::<TestFrames>::new());
        let owner = VmKey::new(3, 1);
        let owner_run = RunId::new(owner, 1);
        let aperture = TrackingAperture::new(0x4000_0000);
        let notify = CountingNotify::new();

        let attach = manager
            .prepare_publish(
                0x33,
                PAGE_SIZE_4K,
                1,
                plan(owner, owner_run, &aperture, &notify),
            )
            .unwrap();
        attach.commit(revision(owner_run, 1)).unwrap();

        let detach = manager.prepare_unpublish(owner, 0x33).unwrap();
        let duplicate = detach.clone();
        aperture.fail_next_release();
        assert!(detach.commit(revision(owner_run, 2)).is_err());
        assert_eq!(aperture.outstanding(), 1);
        assert_eq!(aperture.release_count(), 0);

        // A retry succeeds and releases exactly once.
        let retry = manager.prepare_unpublish(owner, 0x33).unwrap();
        retry.commit(revision(owner_run, 3)).unwrap();
        assert_eq!(aperture.release_count(), 1);

        // The duplicate token cannot release the retired endpoint a second time.
        assert!(duplicate.commit(revision(owner_run, 3)).is_err());
        assert_eq!(aperture.release_count(), 1);
        assert_eq!(aperture.outstanding(), 0);
    }

    #[test]
    fn detach_commit_rejects_stale_or_foreign_revisions() {
        let manager = Arc::new(IvcManager::<TestFrames>::new());
        let owner = VmKey::new(4, 1);
        let owner_run = RunId::new(owner, 1);
        let aperture = TrackingAperture::new(0x5000_0000);
        let notify = CountingNotify::new();

        let attach = manager
            .prepare_publish(
                0x44,
                PAGE_SIZE_4K,
                1,
                plan(owner, owner_run, &aperture, &notify),
            )
            .unwrap();
        attach.commit(revision(owner_run, 5)).unwrap();

        let foreign_run = RunId::new(VmKey::new(6, 1), 1);
        assert!(
            manager
                .prepare_unpublish(owner, 0x44)
                .unwrap()
                .commit(revision(foreign_run, 6))
                .is_err()
        );

        // A revision older than the installed mapping is not valid evidence.
        assert!(
            manager
                .prepare_unpublish(owner, 0x44)
                .unwrap()
                .commit(revision(owner_run, 4))
                .is_err()
        );
        assert_eq!(aperture.release_count(), 0);

        // The installed revision itself cannot prove that its mapping retired.
        assert!(
            manager
                .prepare_unpublish(owner, 0x44)
                .unwrap()
                .commit(revision(owner_run, 5))
                .is_err()
        );
        assert_eq!(aperture.release_count(), 0);

        // A newer revision for the same run is accepted.
        manager
            .prepare_unpublish(owner, 0x44)
            .unwrap()
            .commit(revision(owner_run, 6))
            .unwrap();
        assert_eq!(aperture.release_count(), 1);
    }

    #[test]
    fn teardown_returns_one_token_per_role_owned_by_the_vm() {
        let manager = Arc::new(IvcManager::<TestFrames>::new());
        let publisher = VmKey::new(10, 1);
        let owner = VmKey::new(11, 1);
        let publisher_run = RunId::new(publisher, 1);
        let owner_run = RunId::new(owner, 1);
        let publisher_aperture = TrackingAperture::new(0x6000_0000);
        let owner_aperture = TrackingAperture::new(0x7000_0000);
        let publisher_notify = CountingNotify::new();
        let owner_notify = CountingNotify::new();

        let publish = manager
            .prepare_publish(
                0x60,
                PAGE_SIZE_4K,
                1,
                plan(
                    publisher,
                    publisher_run,
                    &publisher_aperture,
                    &publisher_notify,
                ),
            )
            .unwrap();
        publish.commit(revision(publisher_run, 1)).unwrap();
        let subscribe = manager
            .prepare_subscribe(
                10,
                0x60,
                PAGE_SIZE_4K,
                plan(owner, owner_run, &owner_aperture, &owner_notify),
            )
            .unwrap();
        subscribe.commit(revision(owner_run, 1)).unwrap();

        // `owner` also publishes its own channel.
        let publish = manager
            .prepare_publish(
                0x61,
                PAGE_SIZE_4K,
                1,
                plan(owner, owner_run, &owner_aperture, &owner_notify),
            )
            .unwrap();
        publish.commit(revision(owner_run, 2)).unwrap();

        let teardown = manager.prepare_teardown(owner).unwrap();
        assert_eq!(teardown.len(), 2, "both roles owned by the VM are returned");
        for token in teardown {
            token.commit(revision(owner_run, 3)).unwrap();
        }
        assert_eq!(owner_aperture.outstanding(), 0);
        assert_eq!(owner_aperture.release_count(), 2);

        // The other VM's publisher endpoint is untouched by `owner` teardown.
        assert_eq!(manager.prepare_teardown(publisher).unwrap().len(), 1);
        assert_eq!(publisher_aperture.outstanding(), 1);
    }

    #[test]
    fn channel_key_reuse_isolates_the_previous_generation() {
        let manager = Arc::new(IvcManager::<TestFrames>::new());
        let first = VmKey::new(20, 1);
        let second = VmKey::new(20, 2);
        let first_run = RunId::new(first, 1);
        let second_run = RunId::new(second, 1);
        let first_aperture = TrackingAperture::new(0x8000_0000);
        let second_aperture = TrackingAperture::new(0x9000_0000);
        let first_notify = CountingNotify::new();
        let second_notify = CountingNotify::new();

        let attach = manager
            .prepare_publish(
                0x70,
                PAGE_SIZE_4K,
                1,
                plan(first, first_run, &first_aperture, &first_notify),
            )
            .unwrap();
        attach.commit(revision(first_run, 1)).unwrap();
        let token = manager.prepare_unpublish(first, 0x70).unwrap();
        let stale = token.clone();
        token.commit(revision(first_run, 2)).unwrap();

        // The same numeric id and key can be reused by a new generation.
        let attach = manager
            .prepare_publish(
                0x70,
                PAGE_SIZE_4K,
                1,
                plan(second, second_run, &second_aperture, &second_notify),
            )
            .unwrap();
        attach.commit(revision(second_run, 1)).unwrap();

        // A stale token from the previous generation must not touch the new
        // channel or release its range.
        assert!(stale.commit(revision(first_run, 3)).is_err());
        assert_eq!(second_aperture.release_count(), 0);
        assert_eq!(second_aperture.outstanding(), 1);

        // The new generation is still usable.
        manager
            .prepare_unpublish(second, 0x70)
            .unwrap()
            .commit(revision(second_run, 2))
            .unwrap();
        assert_eq!(second_aperture.release_count(), 1);
    }
}
