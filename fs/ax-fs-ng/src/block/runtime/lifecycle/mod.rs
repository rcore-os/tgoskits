mod controller;
mod device;
mod io;

use alloc::{
    boxed::Box,
    collections::VecDeque,
    format,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    time::Duration,
};

use ax_lazyinit::OnceLock;
use controller::{ControllerIrqToken, ControllerPort, run_controller};
use device::{CpuSubmissionChannel, DeviceInfoEpoch};
use irq_framework::IrqId;
#[cfg(any(feature = "ext4", feature = "fat"))]
use rdif_block::RequestFlags;
use rdif_block::{
    BatchSubmitError, BlkError, BlockController, BlockControllerGroup, BlockGroupMember,
    CompletedRequest, CompletionSink, ControllerEvent, ControllerState, ControllerUpdate,
    DeviceInfo, GroupControllerEvent, GroupControllerUpdate, HardwareQueue, IrqEndpoint,
    OwnedRequest, OwnedRequestBatch, QueueInfo, RequestOp, SharedIrqEndpoint, SubmitError,
    validate_owned_request,
};

use super::{
    channel::{BoundedChannel, SendError},
    completion::{CompletionGroup, CompletionSubscription},
    hctx::{
        ActivatedHctx, ControllerEventPort, Hctx, HctxIrqToken, HctxObserver, PreparedHctx,
        Submission, request_is_nowait,
    },
    irq::{
        BlockIrqAction, ControllerIrqLatch, ControllerIrqTarget, GroupIrqMemberTarget,
        LatchedControllerIrq,
    },
    waiters::TaskWaiters,
};
use crate::{
    BlockError, BlockResult,
    os::{
        BlockIrqRegistration, BlockNotification, BlockThread, register_block_irq, runtime_ops,
        sync::IrqMutex, wall_time,
    },
};

const CONTROLLER_CHANNEL_DEPTH: usize = 64;
const CONTROLLER_TRANSITION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RUNTIME_HCTX: usize = u64::BITS as usize;

const DEVICE_STARTING: u8 = 0;
const DEVICE_READY: u8 = 1;
const DEVICE_FAILED: u8 = 2;
const DEVICE_STOPPED: u8 = 3;

const GROUP_RUNNING: u8 = 0;
const GROUP_STOPPING: u8 = 1;
const GROUP_STOPPED: u8 = 2;

static BLOCK_RUNTIME: OnceLock<Arc<BlockRuntime>> = OnceLock::new();
static BLOCK_READS: AtomicU64 = AtomicU64::new(0);
static BLOCK_SECTORS_READ: AtomicU64 = AtomicU64::new(0);
static BLOCK_WRITES: AtomicU64 = AtomicU64::new(0);
static BLOCK_SECTORS_WRITTEN: AtomicU64 = AtomicU64::new(0);

/// Cumulative completed block I/O counters. Sector counters use 512-byte
/// sectors to retain the Linux `/proc/diskstats` convention.
pub fn block_io_stats() -> (u64, u64, u64, u64) {
    (
        BLOCK_READS.load(Ordering::Relaxed),
        BLOCK_SECTORS_READ.load(Ordering::Relaxed),
        BLOCK_WRITES.load(Ordering::Relaxed),
        BLOCK_SECTORS_WRITTEN.load(Ordering::Relaxed),
    )
}

/// One platform IRQ resolved before the portable controller enters runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockIrqSource {
    pub source_id: usize,
    pub irq: IrqId,
}

/// Portable controller plus platform IRQ metadata transferred from `ax-driver`.
pub struct RdifBlockDevice {
    name: String,
    irqs: Vec<BlockIrqSource>,
    controller: Box<dyn BlockController>,
}

/// Portable controller group plus its one-or-more platform IRQ bindings.
pub struct RdifBlockGroup {
    name: String,
    irqs: Vec<BlockIrqSource>,
    controller: Box<dyn BlockControllerGroup>,
}

impl RdifBlockDevice {
    pub fn new_with_irqs(
        name: impl Into<String>,
        irqs: impl IntoIterator<Item = BlockIrqSource>,
        controller: Box<dyn BlockController>,
    ) -> Self {
        Self {
            name: name.into(),
            irqs: irqs.into_iter().collect(),
            controller,
        }
    }
}

impl RdifBlockGroup {
    pub fn new_with_irqs(
        name: impl Into<String>,
        irqs: impl IntoIterator<Item = BlockIrqSource>,
        controller: Box<dyn BlockControllerGroup>,
    ) -> Self {
        Self {
            name: name.into(),
            irqs: irqs.into_iter().collect(),
            controller,
        }
    }
}

/// Installed IRQ-driven block runtime.
pub struct BlockRuntime {
    devices: Vec<Arc<BlockDeviceHandle>>,
    groups: Vec<Arc<BlockGroupHandle>>,
}

impl BlockRuntime {
    pub fn from_rdif_devices(devices: impl IntoIterator<Item = RdifBlockDevice>) -> Self {
        let mut registered = Vec::new();
        for device in devices {
            let name = device.name.clone();
            match BlockDeviceHandle::start(device) {
                Ok(handle) => registered.push(handle),
                Err(error) => {
                    warn!("failed to start IRQ-driven block controller {name}: {error:?}");
                }
            }
        }
        Self {
            devices: registered,
            groups: Vec::new(),
        }
    }

    pub fn from_rdif_sources(
        devices: impl IntoIterator<Item = RdifBlockDevice>,
        groups: impl IntoIterator<Item = RdifBlockGroup>,
    ) -> Self {
        let mut runtime = Self::from_rdif_devices(devices);
        for group in groups {
            match BlockGroupHandle::start(group) {
                Ok(group) => {
                    runtime.devices.extend(group.members.iter().cloned());
                    runtime.groups.push(group);
                }
                Err(error) => {
                    warn!("failed to start IRQ-shared block controller group: {error:?}");
                }
            }
        }
        runtime
    }

    pub fn install_from_rdif_devices(
        devices: impl IntoIterator<Item = RdifBlockDevice>,
    ) -> Arc<Self> {
        let runtime = Arc::new(Self::from_rdif_devices(devices));
        BLOCK_RUNTIME.call_once(|| Arc::clone(&runtime));
        runtime
    }

    pub fn install_from_rdif_sources(
        devices: impl IntoIterator<Item = RdifBlockDevice>,
        groups: impl IntoIterator<Item = RdifBlockGroup>,
    ) -> Arc<Self> {
        let runtime = Arc::new(Self::from_rdif_sources(devices, groups));
        BLOCK_RUNTIME.call_once(|| Arc::clone(&runtime));
        runtime
    }

    pub fn devices(&self) -> &[Arc<BlockDeviceHandle>] {
        &self.devices
    }

    fn online_smp(&self) -> Result<(), BlkError> {
        for device in &self.devices {
            device.online_smp()?;
        }
        Ok(())
    }

    fn release_irqs_for_passthrough(&self) -> BlockResult<usize> {
        let mut released = 0;
        let mut first_error = None;
        for group in &self.groups {
            match group.shutdown_result() {
                Ok(count) => released += count,
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        for device in &self.devices {
            if device.inner.group_owner.is_some() {
                continue;
            }
            match device.inner.shutdown_result() {
                Ok(count) => released += count,
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        first_error.map_or(Ok(released), Err)
    }
}

struct BlockGroupHandle {
    name: String,
    controller: IrqMutex<Option<Box<dyn BlockControllerGroup>>>,
    registrations: IrqMutex<Vec<InstalledGroupIrqRegistration>>,
    members: Vec<Arc<BlockDeviceHandle>>,
    teardown_state: AtomicU8,
    teardown_waiters: TaskWaiters,
}

struct GroupOwnerLink {
    state: IrqMutex<GroupOwnerState>,
}

enum GroupOwnerState {
    Provisional { terminal_seen: bool },
    Installed(Weak<BlockGroupHandle>),
}

impl GroupOwnerLink {
    const fn new() -> Self {
        Self {
            state: IrqMutex::new(GroupOwnerState::Provisional {
                terminal_seen: false,
            }),
        }
    }

    fn terminal_owner(&self) -> Option<Arc<BlockGroupHandle>> {
        let owner = {
            let mut state = self.state.lock();
            match &mut *state {
                GroupOwnerState::Provisional { terminal_seen } => {
                    *terminal_seen = true;
                    return None;
                }
                GroupOwnerState::Installed(owner) => owner.clone(),
            }
        };
        owner.upgrade()
    }

    fn provisional_terminal_seen(&self) -> bool {
        matches!(
            &*self.state.lock(),
            GroupOwnerState::Provisional {
                terminal_seen: true
            }
        )
    }

    fn install(&self, owner: &Arc<BlockGroupHandle>) -> bool {
        let mut state = self.state.lock();
        match &*state {
            GroupOwnerState::Provisional {
                terminal_seen: false,
            } => {
                *state = GroupOwnerState::Installed(Arc::downgrade(owner));
                true
            }
            GroupOwnerState::Provisional {
                terminal_seen: true,
            }
            | GroupOwnerState::Installed(_) => false,
        }
    }
}

struct InstalledGroupIrqRegistration {
    registration: Box<dyn BlockIrqRegistration>,
    hctx_tokens: Vec<HctxIrqToken>,
    controller_tokens: Vec<ControllerIrqToken>,
}

impl InstalledGroupIrqRegistration {
    fn enable(&mut self) -> BlockResult {
        for token in &mut self.hctx_tokens {
            token.commit();
        }
        for token in &mut self.controller_tokens {
            token.commit();
        }
        self.registration.enable()
    }

    fn disable_and_synchronize(&self) -> BlockResult {
        self.registration.disable_and_synchronize()
    }
}

struct StartedGroup {
    members: Vec<BlockGroupMember>,
    endpoints: Vec<SharedIrqEndpoint>,
}

impl BlockGroupHandle {
    fn start(group: RdifBlockGroup) -> Result<Arc<Self>, BlkError> {
        let RdifBlockGroup {
            name,
            irqs,
            mut controller,
        } = group;
        let group_owner = Arc::new(GroupOwnerLink::new());
        let StartedGroup { members, endpoints } =
            match start_group_controller(&mut *controller, CONTROLLER_TRANSITION_TIMEOUT) {
                Ok(started) => started,
                Err(error) => {
                    let _ = drive_group_transition(
                        &mut *controller,
                        GroupControllerEvent::Shutdown,
                        CONTROLLER_TRANSITION_TIMEOUT,
                    );
                    return Err(error);
                }
            };
        let mut bootstrapped = Vec::new();
        for member in members {
            let (member_id, member_controller) = member.into_parts();
            let member_name = member_controller.name().into();
            match BlockDeviceHandle::bootstrap_group_member(
                member_id,
                member_name,
                member_controller,
                Arc::clone(&group_owner),
            ) {
                Ok(handle) => bootstrapped.push((member_id, handle)),
                Err(error) => {
                    warn!("{name}: failed to bootstrap block member {member_id}: {error:?}");
                }
            }
        }
        if bootstrapped.is_empty() {
            let _ = drive_group_transition(
                &mut *controller,
                GroupControllerEvent::Shutdown,
                CONTROLLER_TRANSITION_TIMEOUT,
            );
            return Err(BlkError::NotSupported);
        }
        if group_owner.provisional_terminal_seen() {
            abort_group_start(controller, bootstrapped, Vec::new());
            return Err(BlkError::Io);
        }

        let mut registrations = Vec::new();
        let mut endpoint_sources = Vec::new();
        let setup_result = (|| {
            if endpoints.is_empty() {
                return Err(BlkError::NotSupported);
            }
            registrations
                .try_reserve(endpoints.len())
                .map_err(|_| BlkError::NoMemory)?;
            endpoint_sources
                .try_reserve(endpoints.len())
                .map_err(|_| BlkError::NoMemory)?;
            for (_, member) in &bootstrapped {
                member.inner.reserve_group_irq_targets(endpoints.len())?;
            }
            for endpoint in endpoints {
                let source_id = endpoint.source_id();
                let irq = irqs
                    .iter()
                    .find(|source| source.source_id == source_id)
                    .map(|source| source.irq)
                    .ok_or(BlkError::NotSupported)?;
                let mut targets = Vec::new();
                let mut hctx_tokens = Vec::new();
                let mut controller_tokens = Vec::new();
                targets
                    .try_reserve(bootstrapped.len())
                    .map_err(|_| BlkError::NoMemory)?;
                controller_tokens
                    .try_reserve(bootstrapped.len())
                    .map_err(|_| BlkError::NoMemory)?;
                let hctx_target_count = bootstrapped
                    .iter()
                    .try_fold(0usize, |count, (_, member)| {
                        count.checked_add(member.inner.hctxs.lock().len())
                    })
                    .ok_or(BlkError::InvalidRequest)?;
                hctx_tokens
                    .try_reserve(hctx_target_count)
                    .map_err(|_| BlkError::NoMemory)?;
                for (member_id, member) in &bootstrapped {
                    let (target, mut member_hctx_tokens, member_controller_token) =
                        member.inner.group_irq_target(*member_id, source_id)?;
                    targets.push(target);
                    hctx_tokens.append(&mut member_hctx_tokens);
                    controller_tokens.push(member_controller_token);
                }
                let cpu = bootstrapped
                    .first()
                    .and_then(|(_, member)| member.inner.first_hctx_cpu())
                    .unwrap_or(0);
                let registration = register_block_irq(
                    format!("{name}/irq-{source_id}"),
                    irq,
                    cpu,
                    BlockIrqAction::new_group(endpoint.into_handler(), None, targets),
                )
                .map_err(|_| BlkError::Io)?;
                registrations.push(InstalledGroupIrqRegistration {
                    registration,
                    hctx_tokens,
                    controller_tokens,
                });
                endpoint_sources.push(source_id);
            }

            for registration in &mut registrations {
                registration.enable().map_err(|_| BlkError::Io)?;
            }
            for source_id in &endpoint_sources {
                for (_, member) in &bootstrapped {
                    let state = member.inner.controller.call(ControllerEvent::Rearm {
                        source_id: *source_id,
                    })?;
                    if state == ControllerState::Shutdown {
                        return Err(BlkError::Io);
                    }
                }
                let state = drive_group_transition(
                    &mut *controller,
                    GroupControllerEvent::Rearm {
                        source_id: *source_id,
                    },
                    CONTROLLER_TRANSITION_TIMEOUT,
                )?;
                if state == ControllerState::Shutdown {
                    return Err(BlkError::Io);
                }
                if group_owner.provisional_terminal_seen() {
                    return Err(BlkError::Io);
                }
            }
            Ok(())
        })();
        if let Err(error) = setup_result {
            abort_group_start(controller, bootstrapped, registrations);
            return Err(error);
        }

        let ready_error = bootstrapped.iter().find_map(|(member_id, member)| {
            member
                .finish_group_start()
                .err()
                .map(|error| (*member_id, error))
        });
        if let Some((member_id, error)) = ready_error {
            warn!("{name}: block member {member_id} failed to become ready: {error:?}");
            abort_group_start(controller, bootstrapped, registrations);
            return Err(error);
        }
        let ready = bootstrapped.into_iter().map(|(_, member)| member).collect();
        let handle = Arc::new(Self {
            name,
            controller: IrqMutex::new(Some(controller)),
            registrations: IrqMutex::new(registrations),
            members: ready,
            teardown_state: AtomicU8::new(GROUP_RUNNING),
            teardown_waiters: TaskWaiters::new(),
        });
        if !group_owner.install(&handle) {
            let _ = handle.shutdown_result();
            return Err(BlkError::Io);
        }
        Ok(handle)
    }

    fn shutdown_result(&self) -> BlockResult<usize> {
        self.shutdown_internal(None)
    }

    fn shutdown_from_member(&self, member_id: usize) {
        let _ = self.shutdown_internal(Some(member_id));
    }

    fn shutdown_internal(&self, origin_member: Option<usize>) -> BlockResult<usize> {
        loop {
            match self.teardown_state.load(Ordering::Acquire) {
                GROUP_STOPPED => {
                    if origin_member.is_none() {
                        for member in &self.members {
                            member.inner.join_controller_thread();
                        }
                    }
                    return self
                        .members
                        .iter()
                        .find_map(|member| member.inner.terminal_teardown_error())
                        .map_or(Ok(0), Err);
                }
                GROUP_RUNNING => {
                    if self
                        .teardown_state
                        .compare_exchange(
                            GROUP_RUNNING,
                            GROUP_STOPPING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        break;
                    }
                }
                GROUP_STOPPING => self
                    .teardown_waiters
                    .wait_while(|| self.teardown_state.load(Ordering::Acquire) == GROUP_STOPPING)?,
                _ => unreachable!("invalid block group teardown state"),
            }
        }
        // Move the controller out so register retry waits never retain an
        // IRQ-save guard.
        let Some(mut controller) = self.controller.lock().take() else {
            return self.finish_shutdown(Err(BlockError::Io));
        };
        for member in &self.members {
            member.inner.prepare_group_shutdown_local();
        }
        for member in &self.members {
            if let Err(error) = quiesce_group_member(member) {
                *self.controller.lock() = Some(controller);
                return self.finish_shutdown(Err(error.into()));
            }
        }
        let group_quiesce = match drive_group_transition(
            &mut *controller,
            GroupControllerEvent::QuiesceIrqs,
            CONTROLLER_TRANSITION_TIMEOUT,
        ) {
            Ok(state) => state,
            Err(error) => {
                *self.controller.lock() = Some(controller);
                return self.finish_shutdown(Err(error.into()));
            }
        };
        let registrations = core::mem::take(&mut *self.registrations.lock());
        let count = registrations.len();
        if let Err(error) = disable_registrations(&registrations) {
            *self.registrations.lock() = registrations;
            for member in &self.members {
                member.inner.quiesce_hctxs_for_group();
            }
            *self.controller.lock() = Some(controller);
            return self.finish_shutdown(Err(error));
        }
        drop(registrations);
        for member in &self.members {
            member.inner.quiesce_hctxs_for_group();
        }
        for member in &self.members {
            if let Err(error) = shutdown_group_member(member) {
                *self.controller.lock() = Some(controller);
                return self.finish_shutdown(Err(error.into()));
            }
        }
        if group_quiesce != ControllerState::Shutdown {
            match drive_group_transition(
                &mut *controller,
                GroupControllerEvent::Shutdown,
                CONTROLLER_TRANSITION_TIMEOUT,
            ) {
                Ok(ControllerState::Shutdown) => {}
                Ok(state) => {
                    warn!("{}: group shutdown was not confirmed: {state:?}", self.name);
                    *self.controller.lock() = Some(controller);
                    return self.finish_shutdown(Err(BlockError::Io));
                }
                Err(error) => {
                    warn!("{}: block group shutdown failed: {error:?}", self.name);
                    *self.controller.lock() = Some(controller);
                    return self.finish_shutdown(Err(error.into()));
                }
            }
        }
        drop(controller);
        let mut first_error = None;
        for member in &self.members {
            if let Err(error) = member.inner.finish_group_member_shutdown()
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        let result = self.finish_terminal_shutdown(first_error.map_or(Ok(count), Err));
        for member in &self.members {
            if member.inner.member_id != origin_member {
                member.inner.join_controller_thread();
            }
        }
        result
    }

    fn finish_terminal_shutdown(&self, result: BlockResult<usize>) -> BlockResult<usize> {
        self.teardown_state.store(GROUP_STOPPED, Ordering::Release);
        self.teardown_waiters.notify_all();
        result
    }

    fn finish_shutdown(&self, result: BlockResult<usize>) -> BlockResult<usize> {
        let next = if result.is_ok() {
            GROUP_STOPPED
        } else {
            GROUP_RUNNING
        };
        self.teardown_state.store(next, Ordering::Release);
        self.teardown_waiters.notify_all();
        result
    }
}

fn quiesce_group_member(member: &BlockDeviceHandle) -> Result<(), BlkError> {
    if member.inner.controller.terminal_confirmed() {
        return Ok(());
    }
    match member.inner.controller.call(ControllerEvent::QuiesceIrqs) {
        Ok(_) => Ok(()),
        Err(_) if member.inner.controller.terminal_confirmed() => Ok(()),
        Err(error) => Err(error),
    }
}

fn shutdown_group_member(member: &BlockDeviceHandle) -> Result<(), BlkError> {
    if member.inner.controller.terminal_confirmed() {
        return Ok(());
    }
    match member.inner.controller.call(ControllerEvent::Shutdown) {
        Ok(ControllerState::Shutdown) => Ok(()),
        Ok(state) => {
            warn!(
                "{}: group member shutdown was not confirmed: {state:?}",
                member.name()
            );
            Err(BlkError::Io)
        }
        Err(_) if member.inner.controller.terminal_confirmed() => Ok(()),
        Err(error) => Err(error),
    }
}

fn abort_group_start(
    mut controller: Box<dyn BlockControllerGroup>,
    members: Vec<(usize, Arc<BlockDeviceHandle>)>,
    registrations: Vec<InstalledGroupIrqRegistration>,
) {
    for (_, member) in &members {
        member.inner.prepare_group_shutdown_local();
    }
    let mut quiesced = true;
    for (_, member) in &members {
        if quiesce_group_member(member).is_err() {
            quiesced = false;
        }
    }
    if drive_group_transition(
        &mut *controller,
        GroupControllerEvent::QuiesceIrqs,
        CONTROLLER_TRANSITION_TIMEOUT,
    )
    .is_err()
    {
        quiesced = false;
    }
    let irqs_synchronized = disable_registrations(&registrations).is_ok();
    for (_, member) in &members {
        member.inner.quiesce_hctxs_for_group();
    }
    if !quiesced || !irqs_synchronized {
        if irqs_synchronized {
            drop(registrations);
        } else {
            core::mem::forget(registrations);
        }
        core::mem::forget(members);
        core::mem::forget(controller);
        return;
    }
    drop(registrations);
    for (_, member) in &members {
        if shutdown_group_member(member).is_err() {
            core::mem::forget(members);
            core::mem::forget(controller);
            return;
        }
    }
    if !matches!(
        drive_group_transition(
            &mut *controller,
            GroupControllerEvent::Shutdown,
            CONTROLLER_TRANSITION_TIMEOUT,
        ),
        Ok(ControllerState::Shutdown)
    ) {
        core::mem::forget(members);
        core::mem::forget(controller);
        return;
    }
    drop(controller);
    for (_, member) in &members {
        let _ = member.inner.finish_group_member_shutdown();
        member.inner.join_controller_thread();
    }
}

impl Drop for BlockGroupHandle {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown_result()
            && self.teardown_state.load(Ordering::Acquire) != GROUP_STOPPED
        {
            warn!(
                "{}: quarantining block group because teardown did not reach a safe terminal \
                 state: {error:?}",
                self.name
            );
            let controller = self.controller.lock().take();
            let registrations = core::mem::take(&mut *self.registrations.lock());
            let members = core::mem::take(&mut self.members);
            core::mem::forget(controller);
            core::mem::forget(registrations);
            core::mem::forget(members);
        }
    }
}

fn start_group_controller(
    controller: &mut dyn BlockControllerGroup,
    timeout: Duration,
) -> Result<StartedGroup, BlkError> {
    let mut members = Vec::new();
    let mut endpoints = Vec::new();
    let mut event = GroupControllerEvent::Start;
    let deadline = wall_time().saturating_add(timeout);
    loop {
        let mut update = controller.advance(event)?;
        members.extend(update.take_members());
        endpoints.extend(update.take_irq_endpoints());
        match update.controller_state() {
            ControllerState::Ready => return Ok(StartedGroup { members, endpoints }),
            ControllerState::RegisterPending { retry_after } => {
                wait_for_group_retry(deadline, retry_after)?;
                event = GroupControllerEvent::RegisterRetry;
            }
            ControllerState::WaitingForIrq | ControllerState::Shutdown => {
                return Err(BlkError::Io);
            }
        }
    }
}

fn drive_group_transition(
    controller: &mut dyn BlockControllerGroup,
    mut event: GroupControllerEvent,
    timeout: Duration,
) -> Result<ControllerState, BlkError> {
    let deadline = wall_time().saturating_add(timeout);
    loop {
        let update: GroupControllerUpdate = controller.advance(event)?;
        match update.controller_state() {
            ControllerState::RegisterPending { retry_after } => {
                wait_for_group_retry(deadline, retry_after)?;
                event = GroupControllerEvent::RegisterRetry;
            }
            state => return Ok(state),
        }
    }
}

fn wait_for_group_retry(deadline: Duration, retry_after: Duration) -> Result<(), BlkError> {
    let now = wall_time();
    if now >= deadline {
        return Err(BlkError::TimedOut);
    }
    let delay = if retry_after.is_zero() {
        Duration::from_micros(1)
    } else {
        retry_after
    };
    let wait = delay.min(deadline - now);
    runtime_ops()
        .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?
        .notification()
        .wait_timeout(wait);
    if wall_time() >= deadline {
        Err(BlkError::TimedOut)
    } else {
        Ok(())
    }
}

/// Expands every installed controller after schedulers, IPIs, and local IRQs
/// are online on all CPUs.
///
/// # Errors
///
/// Returns the first controller transition failure. No controller falls back
/// to polling.
pub fn online_smp() -> Result<(), BlkError> {
    BLOCK_RUNTIME
        .get()
        .ok_or(BlkError::Other("block runtime is not installed"))?
        .online_smp()
}

/// Stops host block IRQ ownership before device passthrough.
///
/// # Errors
///
/// Returns an error if any IRQ registration cannot be disabled and
/// synchronized, the owning controller cannot confirm shutdown, or a hardware
/// queue cannot be quiesced completely.
pub fn release_block_irqs_for_passthrough() -> BlockResult<usize> {
    BLOCK_RUNTIME
        .get()
        .map_or(Ok(0), |runtime| runtime.release_irqs_for_passthrough())
}

/// Filesystem-facing device handle backed only by bounded channels.
pub struct BlockDeviceHandle {
    inner: Arc<DeviceInner>,
}

impl Drop for BlockDeviceHandle {
    fn drop(&mut self) {
        if let Err(error) = self.inner.shutdown_result() {
            let terminal = self.inner.lifecycle_gate.lock().phase == DevicePhase::Stopped;
            if !terminal {
                warn!(
                    "{}: quarantining block device because teardown did not reach a safe terminal \
                     state: {error:?}",
                    self.inner.name
                );
                self.inner.quarantine_resources();
            }
        }
    }
}

struct DeviceInner {
    name: String,
    device_info: IrqMutex<DeviceInfoEpoch>,
    max_io_queues: usize,
    irq_sources: Vec<BlockIrqSource>,
    hctxs: IrqMutex<Vec<Arc<Hctx>>>,
    detached_queues: IrqMutex<Vec<Box<dyn HardwareQueue>>>,
    cpu_channels: IrqMutex<Vec<CpuSubmissionChannel>>,
    irq_registrations: IrqMutex<Vec<InstalledIrqRegistration>>,
    controller: Arc<ControllerPort>,
    controller_thread: IrqMutex<Option<Box<dyn BlockThread>>>,
    state: AtomicU8,
    accepting: AtomicBool,
    data_gate_waiters: TaskWaiters,
    flush_gate_waiters: TaskWaiters,
    data_drain_waiters: TaskWaiters,
    state_notification: Arc<dyn BlockNotification>,
    lifecycle_gate: IrqMutex<LifecycleGateState>,
    shutdown_waiters: TaskWaiters,
    member_id: Option<usize>,
    group_owner: Option<Arc<GroupOwnerLink>>,
}

struct InstalledIrqRegistration {
    registration: Box<dyn BlockIrqRegistration>,
    hctx_tokens: Vec<HctxIrqToken>,
    controller_token: ControllerIrqToken,
}

impl InstalledIrqRegistration {
    fn enable(&mut self) -> BlockResult {
        for token in &mut self.hctx_tokens {
            token.commit();
        }
        self.controller_token.commit();
        self.registration.enable()
    }

    fn disable_and_synchronize(&self) -> BlockResult {
        self.registration.disable_and_synchronize()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DevicePhase {
    Starting,
    Ready,
    Failed,
    Stopping,
    Stopped,
}

struct LifecycleGateState {
    phase: DevicePhase,
    submission_ready_hctx_count: usize,
    active_data: usize,
    flush_active: bool,
    teardown_in_progress: bool,
    terminal_teardown_error: Option<BlockError>,
}

impl LifecycleGateState {
    const fn new() -> Self {
        Self {
            phase: DevicePhase::Starting,
            submission_ready_hctx_count: 0,
            active_data: 0,
            flush_active: false,
            teardown_in_progress: false,
            terminal_teardown_error: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SubmissionAdmission {
    Blocking,
    Nowait,
}

impl SubmissionAdmission {
    const fn is_nowait(self) -> bool {
        matches!(self, Self::Nowait)
    }

    fn cannot_wait(self) -> bool {
        self.is_nowait() || request_cannot_block()
    }
}

impl BlockDeviceHandle {
    fn start(device: RdifBlockDevice) -> Result<Arc<Self>, BlkError> {
        let RdifBlockDevice {
            name,
            irqs,
            controller,
        } = device;
        let handle = Self::bootstrap(name, irqs, controller, None, None)?;
        handle.finish_group_start()?;
        Ok(handle)
    }

    fn bootstrap_group_member(
        member_id: usize,
        name: String,
        controller: Box<dyn BlockController>,
        group_owner: Arc<GroupOwnerLink>,
    ) -> Result<Arc<Self>, BlkError> {
        Self::bootstrap(
            name,
            Vec::new(),
            controller,
            Some(member_id),
            Some(group_owner),
        )
    }

    fn bootstrap(
        name: String,
        irqs: Vec<BlockIrqSource>,
        controller: Box<dyn BlockController>,
        member_id: Option<usize>,
        group_owner: Option<Arc<GroupOwnerLink>>,
    ) -> Result<Arc<Self>, BlkError> {
        let info = controller.device_info();
        let max_io_queues = controller.max_io_queues().min(MAX_RUNTIME_HCTX);
        if max_io_queues == 0 {
            return Err(BlkError::NotSupported);
        }
        let ops =
            runtime_ops().map_err(|_| BlkError::Other("block runtime adapter is not installed"))?;
        let controller_notification = ops.notification();
        let controller_port = Arc::new(ControllerPort {
            commands: BoundedChannel::with_item_notification(
                CONTROLLER_CHANNEL_DEPTH,
                Arc::clone(&controller_notification),
            )
            .map_err(|_| BlkError::NoMemory)?,
            notification: controller_notification,
            irq_latches: IrqMutex::new(Vec::new()),
            terminal_confirmed: AtomicBool::new(false),
        });
        let inner = Arc::new(DeviceInner {
            name,
            device_info: IrqMutex::new(DeviceInfoEpoch::new(info)),
            max_io_queues,
            irq_sources: irqs,
            hctxs: IrqMutex::new(Vec::new()),
            detached_queues: IrqMutex::new(Vec::new()),
            cpu_channels: IrqMutex::new(Vec::new()),
            irq_registrations: IrqMutex::new(Vec::new()),
            controller: Arc::clone(&controller_port),
            controller_thread: IrqMutex::new(None),
            state: AtomicU8::new(DEVICE_STARTING),
            accepting: AtomicBool::new(false),
            data_gate_waiters: TaskWaiters::new(),
            flush_gate_waiters: TaskWaiters::new(),
            data_drain_waiters: TaskWaiters::new(),
            state_notification: ops.notification(),
            lifecycle_gate: IrqMutex::new(LifecycleGateState::new()),
            shutdown_waiters: TaskWaiters::new(),
            member_id,
            group_owner,
        });
        let weak = Arc::downgrade(&inner);
        let thread = ops
            .spawn_pinned(
                format!("blk-ctl/{}", inner.name),
                0,
                Box::new(move || run_controller(controller, controller_port, weak)),
            )
            .map_err(|_| BlkError::NoMemory)?;
        *inner.controller_thread.lock() = Some(thread);

        let handle = Arc::new(Self { inner });
        let state = match handle
            .inner
            .controller
            .call(ControllerEvent::Start { target_queues: 1 })
        {
            Ok(state) => state,
            Err(error) => {
                handle.inner.shutdown();
                return Err(error);
            }
        };
        if state == ControllerState::Ready && handle.inner.hctxs.lock().is_empty() {
            handle.shutdown();
            return Err(BlkError::Other(
                "controller reported ready without an I/O hardware queue",
            ));
        }
        if state == ControllerState::Ready && !handle.inner.mark_ready() {
            handle.inner.shutdown();
            return Err(BlkError::Io);
        }
        Ok(handle)
    }

    fn finish_group_start(&self) -> Result<(), BlkError> {
        if self.inner.state.load(Ordering::Acquire) != DEVICE_READY {
            self.inner.wait_until_ready(CONTROLLER_TRANSITION_TIMEOUT)?;
        }
        if self.inner.hctxs.lock().is_empty() || !self.inner.mark_ready() {
            return Err(BlkError::Io);
        }
        Ok(())
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn device_info(&self) -> DeviceInfo {
        self.inner.published_device_info()
    }

    #[cfg(feature = "ext4")]
    pub(crate) fn supports_flush(&self) -> bool {
        let gate = self.inner.lifecycle_gate.lock();
        let queues = self.inner.hctxs.lock();
        let ready = &queues[..gate.submission_ready_hctx_count.min(queues.len())];
        !ready.is_empty() && ready.iter().all(|queue| queue.info().limits.supports_flush)
    }

    #[cfg(feature = "ext4")]
    pub(crate) fn supports_fua(&self) -> bool {
        let gate = self.inner.lifecycle_gate.lock();
        let queues = self.inner.hctxs.lock();
        let ready = &queues[..gate.submission_ready_hctx_count.min(queues.len())];
        !ready.is_empty()
            && ready.iter().all(|queue| {
                queue
                    .info()
                    .limits
                    .supported_flags
                    .contains(RequestFlags::FUA)
            })
    }

    /// Enqueues one DMA-owning request on the current CPU software channel.
    ///
    /// `NOWAIT` affects only bounded channel admission and is removed before
    /// hardware validation.
    pub fn submit_owned(
        &self,
        request: OwnedRequest,
    ) -> Result<CompletionSubscription, SubmitError> {
        let batch = OwnedRequestBatch::from_iter([request]);
        match self.submit_batch_owned(batch) {
            Ok(group) => Ok(group
                .into_single()
                .expect("single-request submission returns one completion")),
            Err(error) => {
                let result = error.error;
                let mut requests = error.into_batch().into_iter();
                let request = requests
                    .next()
                    .expect("single-request submission error returns its request");
                Err(SubmitError::new(result, request))
            }
        }
    }

    /// Enqueues one ordered request group on the current CPU software channel.
    ///
    /// The runtime may split or combine groups when dispatching to hardware.
    /// A flush must be submitted alone so the device-level barrier can order it
    /// against every hardware queue.
    pub fn submit_batch_owned(
        &self,
        requests: OwnedRequestBatch,
    ) -> Result<CompletionGroup, BatchSubmitError> {
        if !self.inner.accepting.load(Ordering::Acquire) {
            return Err(BatchSubmitError::new(BlkError::Io, requests));
        }
        let count = requests.len();
        if count == 0 {
            return Err(BatchSubmitError::new(BlkError::InvalidRequest, requests));
        }
        let Some(cpu_channel) = self.inner.select_cpu_channel() else {
            return Err(BatchSubmitError::new(BlkError::Io, requests));
        };
        let mut info = cpu_channel.hctx.info();
        info.device = self.inner.published_device_info();
        let validation_error = requests
            .iter()
            .find_map(|request| validate_owned_request(info, request).err());
        if let Some(error) = validation_error {
            return Err(BatchSubmitError::new(error, requests));
        }

        let admission = if requests.iter().any(request_is_nowait) {
            SubmissionAdmission::Nowait
        } else {
            SubmissionAdmission::Blocking
        };
        let flush_count = requests
            .iter()
            .filter(|request| request.op == RequestOp::Flush)
            .count();
        if flush_count != 0 && (flush_count != 1 || count != 1) {
            return Err(BatchSubmitError::new(BlkError::InvalidRequest, requests));
        }
        let is_flush = flush_count == 1;
        if is_flush {
            if let Err(error) = self.inner.begin_flush_barrier(admission) {
                return Err(BatchSubmitError::new(error, requests));
            }
        } else if let Err(error) = self.inner.enter_data_submissions(count, admission) {
            return Err(BatchSubmitError::new(error, requests));
        }

        let (group, mut completions) = match CompletionGroup::pairs(count) {
            Ok(pair) => pair,
            Err(error) => {
                self.inner.undo_submission_admission(
                    if is_flush {
                        RequestOp::Flush
                    } else {
                        RequestOp::Read
                    },
                    count,
                );
                return Err(BatchSubmitError::new(error, requests));
            }
        };
        let mut submissions = VecDeque::new();
        if submissions.try_reserve_exact(count).is_err() {
            self.inner.undo_submission_admission(
                if is_flush {
                    RequestOp::Flush
                } else {
                    RequestOp::Read
                },
                count,
            );
            return Err(BatchSubmitError::new(BlkError::NoMemory, requests));
        }
        for request in requests {
            let completion = completions
                .pop_front()
                .expect("completion sender count matches request batch");
            submissions.push_back(Submission {
                request,
                completion,
            });
        }
        let send_result = cpu_channel
            .channel
            .send_many(submissions, admission.is_nowait());
        if let Err(send_error) = send_result {
            self.inner.undo_submission_admission(
                if is_flush {
                    RequestOp::Flush
                } else {
                    RequestOp::Read
                },
                count,
            );
            let terminal = {
                let gate = self.inner.lifecycle_gate.lock();
                gate.phase != DevicePhase::Ready
            };
            let error = if terminal {
                BlkError::Io
            } else {
                BlkError::Retry
            };
            let submissions = match send_error {
                SendError::Closed(submissions) | SendError::Full(submissions) => submissions,
            };
            let requests = submissions
                .into_iter()
                .map(|submission| submission.request)
                .collect();
            return Err(BatchSubmitError::new(error, requests));
        }
        Ok(group)
    }

    pub(crate) fn read_blocks(&self, block_id: u64, buf: &mut [u8]) -> BlockResult {
        io::read_blocks(self, block_id, buf)
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    pub(crate) fn write_blocks(&self, block_id: u64, buf: &[u8]) -> BlockResult {
        io::write_blocks(self, block_id, buf)
    }

    #[cfg(feature = "ext4")]
    pub(crate) fn write_blocks_fua(&self, block_id: u64, buf: &[u8]) -> BlockResult {
        if !self.supports_fua() {
            return Err(BlockError::Unsupported);
        }
        io::write_blocks_fua(self, block_id, buf)
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    pub(crate) fn flush_blocks(&self) -> BlockResult {
        let request = OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        };
        let completion = self
            .submit_owned(request)
            .map_err(|error| BlockError::from(error.error))?
            .recv()
            .map_err(BlockError::from)?;
        completion.result.map_err(BlockError::from)
    }

    fn online_smp(&self) -> Result<(), BlkError> {
        self.inner.online_smp()
    }

    fn shutdown(&self) -> usize {
        self.inner.shutdown()
    }
}

fn request_cannot_block() -> bool {
    match runtime_ops() {
        Ok(ops) => !ops.can_block(),
        Err(_) => true,
    }
}

fn sectors_for_blocks(logical_block_size: usize, block_count: u32) -> u64 {
    (logical_block_size as u64)
        .saturating_mul(block_count as u64)
        .div_ceil(512)
}

fn stop_hctxs(hctxs: &[Arc<Hctx>]) -> Result<(), BlkError> {
    let mut first_error = None;
    for hctx in hctxs {
        if let Err(error) = hctx.stop()
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn quiesce_hctxs(hctxs: &[Arc<Hctx>]) {
    for hctx in hctxs {
        hctx.quiesce();
    }
}

struct DetachedCompletionSink;

impl CompletionSink for DetachedCompletionSink {
    fn complete(&mut self, request: CompletedRequest) {
        drop(request.data);
    }
}

fn shutdown_detached_queues(queues: Vec<Box<dyn HardwareQueue>>) -> Result<(), BlkError> {
    let mut sink = DetachedCompletionSink;
    let mut first_error = None;
    for mut queue in queues {
        if let Err(error) = queue.shutdown(&mut sink) {
            if first_error.is_none() {
                first_error = Some(error);
            }
            warn!(
                "quarantining detached block queue {} after shutdown failed: {error:?}",
                queue.id()
            );
            core::mem::forget(queue);
        }
    }
    first_error.map_or(Ok(()), Err)
}

trait IrqRegistrationControl {
    fn disable_and_synchronize(&self) -> BlockResult;
}

impl IrqRegistrationControl for InstalledIrqRegistration {
    fn disable_and_synchronize(&self) -> BlockResult {
        self.disable_and_synchronize()
    }
}

impl IrqRegistrationControl for InstalledGroupIrqRegistration {
    fn disable_and_synchronize(&self) -> BlockResult {
        self.disable_and_synchronize()
    }
}

impl IrqRegistrationControl for Box<dyn BlockIrqRegistration> {
    fn disable_and_synchronize(&self) -> BlockResult {
        (**self).disable_and_synchronize()
    }
}

fn disable_registrations<T: IrqRegistrationControl>(registrations: &[T]) -> BlockResult {
    let mut first_error = None;
    for registration in registrations {
        if let Err(error) = registration.disable_and_synchronize()
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn block_io_error(
    stage: &'static str,
    operation: RequestOp,
    lba: u64,
    source: BlkError,
) -> BlockError {
    warn!("block {operation:?} at LBA {lba} failed during {stage}: {source:?}");
    BlockError::Device {
        stage,
        operation,
        lba,
        source,
    }
}

#[cfg(test)]
mod tests;
