//! Immutable software-context to hardware-queue routing for v0.13 devices.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_kspin::PreemptGuard;
use ax_lazyinit::LazyInit;
use rdif_block::{
    BlkError, HardwareQueueLimits, InterruptQueueDesc, LogicalDeviceDesc, LogicalDeviceId,
    LogicalDeviceRoute, OwnedRequest, OwnershipDomainId, QueueExecution, QueueInfo, QueueKind,
    QueueLimits, validate_owned_request,
};

#[cfg(test)]
use super::gates::DispatchState;
use super::{
    gates::{
        AdmissionError, AdmissionFreezeProgress, AdmissionGate, DispatchCutoffProof, DispatchGate,
        DispatchGateError,
    },
    software_ctx::{FrozenSoftwareCtxMap, PendingSoftwareCtxPublication, SoftwareCtxIngress},
    table::{
        DomainRequestTable, REQUEST_TABLE_STAGING_FACTOR, RequestReservationFailure,
        RequestTableError, RequestToken,
    },
};
use crate::{
    block::BlockRuntimeConfig,
    maintenance::{
        DeviceMaintenanceHandle, MaintenanceCauses, MaintenanceState, MaintenanceSubmitError,
    },
};

pub(super) struct HardwareCredits {
    pub(super) limit: usize,
    in_use: AtomicUsize,
}

impl HardwareCredits {
    fn new(limit: usize) -> Result<Self, RequestRuntimeBuildError> {
        if limit == 0 {
            return Err(RequestRuntimeBuildError::ZeroQueueDepth);
        }
        Ok(Self {
            limit,
            in_use: AtomicUsize::new(0),
        })
    }

    pub(super) fn try_reserve(&self) -> Option<HardwareCreditReservation<'_>> {
        let mut observed = self.in_use.load(Ordering::Acquire);
        loop {
            if observed >= self.limit {
                return None;
            }
            match self.in_use.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(HardwareCreditReservation {
                        credits: self,
                        retained: false,
                    });
                }
                Err(actual) => observed = actual,
            }
        }
    }

    pub(super) fn release_inflight(&self) {
        let previous = self.in_use.fetch_sub(1, Ordering::AcqRel);
        assert!(
            previous != 0,
            "v0.13 hardware-credit accounting underflowed"
        );
    }

    #[cfg(test)]
    pub(super) fn in_use(&self) -> usize {
        self.in_use.load(Ordering::Acquire)
    }
}

pub(super) struct HardwareCreditReservation<'credits> {
    credits: &'credits HardwareCredits,
    retained: bool,
}

impl HardwareCreditReservation<'_> {
    pub(super) fn retain_for_inflight(mut self) {
        self.retained = true;
    }
}

impl Drop for HardwareCreditReservation<'_> {
    fn drop(&mut self) {
        if !self.retained {
            self.credits.release_inflight();
        }
    }
}

pub(super) struct DomainHctx {
    queue_id: usize,
    credits: HardwareCredits,
}

impl DomainHctx {
    pub(super) const fn queue_id(&self) -> usize {
        self.queue_id
    }

    pub(super) const fn credits(&self) -> &HardwareCredits {
        &self.credits
    }
}

/// Shared request state only. The portable driver owner never enters this Arc.
pub(in crate::block::activation_v13) struct DomainRequestRuntime {
    domain: OwnershipDomainId,
    queue_descs: Box<[InterruptQueueDesc]>,
    hctxs: Box<[DomainHctx]>,
    requests: DomainRequestTable,
    admission: AdmissionGate,
    dispatch: DispatchGate,
    software_contexts: LazyInit<FrozenSoftwareCtxMap>,
    watchdog_ns: u64,
}

impl DomainRequestRuntime {
    pub(in crate::block::activation_v13) fn new(
        domain: OwnershipDomainId,
        queues: &[InterruptQueueDesc],
        config: BlockRuntimeConfig,
    ) -> Result<Self, RequestRuntimeBuildError> {
        if queues.is_empty()
            || queues
                .iter()
                .any(|queue| queue.ownership_domain() != domain)
        {
            return Err(RequestRuntimeBuildError::InvalidDomainTopology);
        }
        let hardware_credit_capacity = queues.iter().try_fold(0_usize, |total, queue| {
            total
                .checked_add(effective_queue_depth(queue)?)
                .ok_or(RequestRuntimeBuildError::CapacityOverflow)
        })?;
        let request_capacity = hardware_credit_capacity
            .checked_mul(REQUEST_TABLE_STAGING_FACTOR)
            .ok_or(RequestRuntimeBuildError::CapacityOverflow)?;
        let requests = DomainRequestTable::new(request_capacity)
            .map_err(RequestRuntimeBuildError::RequestTable)?;
        let hctxs = queues
            .iter()
            .map(|queue| {
                let credit_limit = effective_queue_depth(queue)?;
                Ok(DomainHctx {
                    queue_id: queue.id(),
                    credits: HardwareCredits::new(credit_limit)?,
                })
            })
            .collect::<Result<Vec<_>, RequestRuntimeBuildError>>()?
            .into_boxed_slice();
        Ok(Self {
            domain,
            queue_descs: queues.to_vec().into_boxed_slice(),
            hctxs,
            requests,
            admission: AdmissionGate::new(),
            dispatch: DispatchGate::new(),
            software_contexts: LazyInit::new(),
            watchdog_ns: config.request_watchdog_ns(),
        })
    }

    pub(super) const fn domain(&self) -> OwnershipDomainId {
        self.domain
    }

    pub(super) fn hctxs(&self) -> &[DomainHctx] {
        &self.hctxs
    }

    pub(in crate::block::activation_v13) fn queue_descs(&self) -> &[InterruptQueueDesc] {
        &self.queue_descs
    }

    pub(super) fn hctx_index(&self, queue_id: usize) -> Option<usize> {
        self.hctxs.iter().position(|hctx| hctx.queue_id == queue_id)
    }

    pub(super) const fn requests(&self) -> &DomainRequestTable {
        &self.requests
    }

    pub(super) fn try_admit(&self) -> Result<super::gates::AdmissionPermit<'_>, AdmissionError> {
        self.admission.try_admit()
    }

    pub(super) fn begin_admission_freeze(&self) -> Result<AdmissionFreezeProgress, AdmissionError> {
        self.admission.begin_freeze()
    }

    pub(super) fn admission_is_frozen_and_idle(&self) -> bool {
        self.admission.is_frozen_and_idle()
    }

    pub(super) fn thaw_admission(&self) -> Result<(), AdmissionError> {
        self.admission.thaw()
    }

    pub(super) fn close_admission(&self) -> Result<(), AdmissionError> {
        self.admission.close()
    }

    #[cfg(test)]
    pub(super) fn dispatch_state(&self) -> DispatchState {
        self.dispatch.state()
    }

    pub(super) fn dispatch_allowed(&self) -> bool {
        self.dispatch.allows_dispatch()
    }

    pub(super) fn begin_dispatch_drain(&self) -> Result<(), DispatchGateError> {
        self.dispatch.begin_drain()
    }

    pub(super) fn commit_dispatch_quiesced(
        &self,
        proof: DispatchCutoffProof,
    ) -> Result<(), DispatchGateError> {
        self.dispatch.commit_quiesced(proof)
    }

    pub(super) fn resume_dispatch(&self) -> Result<(), DispatchGateError> {
        self.dispatch.resume()
    }

    pub(super) fn close_dispatch(&self) -> Result<(), DispatchGateError> {
        self.dispatch.close()
    }

    pub(super) const fn watchdog_ns(&self) -> u64 {
        self.watchdog_ns
    }

    fn install_software_contexts(
        &self,
        software_contexts: FrozenSoftwareCtxMap,
    ) -> Result<(), RequestRuntimeBuildError> {
        if software_contexts.hctx_count() != self.hctxs.len() {
            return Err(RequestRuntimeBuildError::InvalidDomainTopology);
        }
        if self
            .software_contexts
            .call_once(|| software_contexts)
            .is_none()
        {
            return Err(RequestRuntimeBuildError::SoftwareContextMapAlreadyFrozen);
        }
        Ok(())
    }

    pub(super) fn pop_software_ctx(
        &self,
        hctx_index: usize,
        cursor: &mut usize,
    ) -> Option<RequestToken> {
        self.software_contexts.get()?.pop(hctx_index, cursor)
    }

    pub(super) fn has_staged(&self) -> bool {
        self.software_contexts
            .get()
            .is_some_and(FrozenSoftwareCtxMap::has_pending)
    }
}

pub(in crate::block::activation_v13) struct DomainSubmitEndpoint {
    runtime: Arc<DomainRequestRuntime>,
    remote: DeviceMaintenanceHandle<super::super::V13MaintenanceEvent>,
}

impl DomainSubmitEndpoint {
    pub(in crate::block::activation_v13) fn new(
        runtime: Arc<DomainRequestRuntime>,
        remote: DeviceMaintenanceHandle<super::super::V13MaintenanceEvent>,
    ) -> Self {
        Self { runtime, remote }
    }

    fn wake_owner(&self) -> Result<(), MaintenanceSubmitError> {
        self.remote.publish_cause(MaintenanceCauses::SUBMIT)
    }

    fn owner_live(&self) -> bool {
        self.remote.state() == MaintenanceState::Live
    }
}

#[derive(Clone)]
pub struct V13BlockDeviceView {
    runtime: Arc<DeviceSubmissionRuntime>,
}

impl V13BlockDeviceView {
    pub(super) const fn new(runtime: Arc<DeviceSubmissionRuntime>) -> Self {
        Self { runtime }
    }

    pub fn id(&self) -> LogicalDeviceId {
        self.runtime.desc.id()
    }

    pub fn name(&self) -> &str {
        self.runtime.desc.name()
    }

    pub fn device_info(&self) -> rdif_block::DeviceInfo {
        self.runtime.desc.device()
    }

    pub fn hardware_limits(&self) -> HardwareQueueLimits {
        self.runtime.desc.limits()
    }

    pub fn queue_limits(&self) -> QueueLimits {
        self.runtime
            .contexts
            .first()
            .map(|context| context.queue_info.limits)
            .unwrap_or_else(|| unreachable!("a published v0.13 device has an online CPU route"))
    }

    pub(in crate::block) fn current_queue_info(&self) -> Result<QueueInfo, V13SubmitErrorKind> {
        let preempt = PreemptGuard::new();
        let cpu = ax_hal::percpu::this_cpu_id_pinned(preempt.cpu_pin());
        self.runtime
            .contexts
            .get(cpu)
            .map(|context| context.queue_info)
            .ok_or(V13SubmitErrorKind::InvalidCpu(cpu))
    }

    pub fn submit_owned(
        &self,
        request: OwnedRequest,
    ) -> Result<V13SubmittedRequest, V13SubmitError> {
        if ax_hal::irq::in_irq_context() {
            return Err(V13SubmitError::new(
                V13SubmitErrorKind::UnsafeContext,
                request,
            ));
        }
        let preempt = PreemptGuard::new();
        let cpu = ax_hal::percpu::this_cpu_id_pinned(preempt.cpu_pin());
        let Some(context) = self.runtime.contexts.get(cpu) else {
            return Err(V13SubmitError::new(
                V13SubmitErrorKind::InvalidCpu(cpu),
                request,
            ));
        };
        if let Err(error) = validate_owned_request(context.queue_info, &request) {
            return Err(V13SubmitError::new(
                V13SubmitErrorKind::Driver(error),
                request,
            ));
        }
        let admission = match context.target.runtime.try_admit() {
            Ok(admission) => admission,
            Err(error) => {
                return Err(V13SubmitError::new(error.into(), request));
            }
        };
        let token = match context.target.runtime.requests.reserve(
            context.queue_info.id,
            self.runtime.desc.driver_key(),
            request,
        ) {
            Ok(token) => token,
            Err(RequestReservationFailure { error, request }) => {
                return Err(V13SubmitError::new(error.into(), request));
            }
        };
        let publication = match context.publish(token) {
            Ok(publication) => publication,
            Err(error) => {
                let request = context
                    .target
                    .runtime
                    .requests
                    .abandon_staged(token)
                    .expect("a failed software-context publication retains its staged request");
                return Err(V13SubmitError::new(error, request));
            }
        };
        drop(preempt);

        let token = match publication.finish_after_wake(context.target.wake_owner()) {
            Ok(token) => token,
            Err(failure) => {
                let (error, token) = failure.into_parts();
                let request = context
                    .target
                    .runtime
                    .requests
                    .abandon_staged(token)
                    .expect("a retracted owner wake retains staged request ownership");
                return Err(V13SubmitError::new(error.into(), request));
            }
        };
        drop(admission);
        Ok(V13SubmittedRequest {
            target: Arc::clone(&context.target),
            token,
        })
    }
}

#[must_use = "an accepted v0.13 request must be waited or explicitly cancelled"]
pub struct V13SubmittedRequest {
    target: Arc<DomainSubmitEndpoint>,
    token: RequestToken,
}

impl V13SubmittedRequest {
    pub fn id(&self) -> rdif_block::RequestId {
        self.token.id()
    }

    pub fn wait(self) -> Result<rdif_block::CompletedRequest, V13SubmitErrorKind> {
        self.target
            .runtime
            .requests
            .wait_and_take(self.token, || self.target.owner_live())
            .map_err(Into::into)
    }
}

pub(super) struct DeviceSubmissionRuntime {
    desc: LogicalDeviceDesc,
    contexts: Box<[DeviceSoftwareCtx]>,
}

impl DeviceSubmissionRuntime {
    pub(super) fn build(
        desc: LogicalDeviceDesc,
        route: LogicalDeviceRoute,
        online_cpu_count: usize,
        domains: &[Arc<DomainSubmitEndpoint>],
        queue_descs: &[InterruptQueueDesc],
        config: BlockRuntimeConfig,
    ) -> Result<Arc<Self>, RequestRuntimeBuildError> {
        if online_cpu_count == 0 || online_cpu_count > crate::CPU_CAPACITY {
            return Err(RequestRuntimeBuildError::InvalidCpuCount);
        }
        let queue_ids = route.queues().iter().collect::<Vec<_>>();
        if queue_ids.is_empty() {
            return Err(RequestRuntimeBuildError::UnroutedDevice);
        }
        let contexts = (0..online_cpu_count)
            .map(|cpu| {
                let queue_id = mapped_queue_for_cpu(&queue_ids, cpu)
                    .ok_or(RequestRuntimeBuildError::UnroutedDevice)?;
                let queue = queue_descs
                    .iter()
                    .find(|queue| queue.id() == queue_id)
                    .ok_or(RequestRuntimeBuildError::MissingQueue(queue_id))?;
                let domain = domains
                    .iter()
                    .find(|domain| domain.runtime.domain() == queue.ownership_domain())
                    .ok_or(RequestRuntimeBuildError::MissingDomain(
                        queue.ownership_domain(),
                    ))?;
                let hctx_index = domain
                    .runtime
                    .hctx_index(queue_id)
                    .ok_or(RequestRuntimeBuildError::MissingQueue(queue_id))?;
                let capacity = effective_queue_depth(queue)?;
                Ok(DeviceSoftwareCtx {
                    hctx_index,
                    domain: queue.ownership_domain(),
                    target: Arc::clone(domain),
                    queue_info: queue_info(&desc, queue, config),
                    ingress: Arc::new(SoftwareCtxIngress::new(cpu, capacity)),
                })
            })
            .collect::<Result<Vec<_>, RequestRuntimeBuildError>>()?
            .into_boxed_slice();
        Ok(Arc::new(Self { desc, contexts }))
    }
}

fn mapped_queue_for_cpu(queue_ids: &[usize], cpu: usize) -> Option<usize> {
    if queue_ids.is_empty() {
        None
    } else {
        Some(queue_ids[cpu % queue_ids.len()])
    }
}

struct DeviceSoftwareCtx {
    hctx_index: usize,
    domain: OwnershipDomainId,
    target: Arc<DomainSubmitEndpoint>,
    queue_info: QueueInfo,
    ingress: Arc<SoftwareCtxIngress>,
}

impl DeviceSoftwareCtx {
    fn publish(
        &self,
        token: RequestToken,
    ) -> Result<PendingSoftwareCtxPublication<'_>, V13SubmitErrorKind> {
        self.ingress.publish(token)
    }
}

fn queue_info(
    device: &LogicalDeviceDesc,
    queue: &InterruptQueueDesc,
    config: BlockRuntimeConfig,
) -> QueueInfo {
    let limits = device.limits();
    QueueInfo {
        id: queue.id(),
        device: device.device(),
        limits: QueueLimits {
            dma_mask: limits.dma_mask,
            dma_domain: limits.dma_domain,
            dma_alignment: limits.dma_alignment,
            max_inflight: effective_queue_depth(queue)
                .expect("published v0.13 queue execution was validated"),
            max_blocks_per_request: limits.max_blocks_per_request,
            max_segments: limits.max_segments,
            max_segment_size: limits.max_segment_size,
            request_timeout_ns: config.request_watchdog_ns(),
            supported_flags: limits.supported_flags,
            supports_flush: limits.supports_flush,
            supports_discard: limits.supports_discard,
            supports_write_zeroes: limits.supports_write_zeroes,
        },
        kind: QueueKind::Interrupt {
            sources: queue.irq_sources(),
        },
        execution: queue.execution(),
    }
}

fn effective_queue_depth(queue: &InterruptQueueDesc) -> Result<usize, RequestRuntimeBuildError> {
    match queue.execution() {
        QueueExecution::Tagged => Ok(usize::from(queue.queue_depth().get())),
        QueueExecution::Serialized => Ok(1),
        QueueExecution::Inline => Err(RequestRuntimeBuildError::InlineExecutionInInterruptDomain),
    }
}

pub(in crate::block::activation_v13) fn build_published_devices(
    logical_devices: &[LogicalDeviceDesc],
    routes: &[LogicalDeviceRoute],
    domains: &[Arc<DomainSubmitEndpoint>],
    queue_descs: &[InterruptQueueDesc],
    online_cpu_count: usize,
    config: BlockRuntimeConfig,
) -> Result<Box<[V13BlockDeviceView]>, RequestRuntimeBuildError> {
    let mut devices = Vec::with_capacity(logical_devices.len());
    for desc in logical_devices {
        let route = routes
            .iter()
            .copied()
            .find(|route| route.runtime_id() == desc.id())
            .ok_or(RequestRuntimeBuildError::MissingDeviceRoute(desc.id()))?;
        let runtime = DeviceSubmissionRuntime::build(
            desc.clone(),
            route,
            online_cpu_count,
            domains,
            queue_descs,
            config,
        )?;
        devices.push(V13BlockDeviceView::new(runtime));
    }
    for domain in domains {
        let by_hctx = domain
            .runtime
            .hctxs()
            .iter()
            .enumerate()
            .map(|(hctx_index, _)| {
                devices
                    .iter()
                    .flat_map(|device| device.runtime.contexts.iter())
                    .filter(|context| {
                        context.domain == domain.runtime.domain()
                            && context.hctx_index == hctx_index
                    })
                    .map(|context| Arc::clone(&context.ingress))
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        domain
            .runtime
            .install_software_contexts(FrozenSoftwareCtxMap::new(by_hctx))?;
    }
    Ok(devices.into_boxed_slice())
}

#[derive(Debug, thiserror::Error)]
#[error("v0.13 block runtime rejected request: {kind}")]
pub struct V13SubmitError {
    kind: V13SubmitErrorKind,
    request: OwnedRequest,
}

impl V13SubmitError {
    fn new(kind: V13SubmitErrorKind, request: OwnedRequest) -> Self {
        Self { kind, request }
    }

    pub const fn kind(&self) -> V13SubmitErrorKind {
        self.kind
    }

    pub fn into_parts(self) -> (V13SubmitErrorKind, OwnedRequest) {
        (self.kind, self.request)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum V13SubmitErrorKind {
    #[error("v0.13 block submission requires ordinary task context")]
    UnsafeContext,
    #[error("CPU {0} is outside the frozen v0.13 software-context map")]
    InvalidCpu(usize),
    #[error("CPU {cpu} v0.13 software context is full")]
    SoftwareCtxFull { cpu: usize },
    #[error("v0.13 block request admission is frozen")]
    AdmissionFrozen,
    #[error("v0.13 block request admission is permanently closed")]
    AdmissionClosed,
    #[error("v0.13 block request admission has too many concurrent submitters")]
    AdmissionSaturated,
    #[error(transparent)]
    Driver(BlkError),
    #[error("v0.13 request-table ownership transition failed")]
    RequestTable,
    #[error(transparent)]
    Maintenance(MaintenanceSubmitError),
}

impl From<AdmissionError> for V13SubmitErrorKind {
    fn from(error: AdmissionError) -> Self {
        match error {
            AdmissionError::Frozen => Self::AdmissionFrozen,
            AdmissionError::Closed => Self::AdmissionClosed,
            AdmissionError::Saturated => Self::AdmissionSaturated,
            AdmissionError::StillOpen | AdmissionError::ActiveSubmitters(_) => {
                unreachable!("submit-side admission observed an owner-only transition error")
            }
        }
    }
}

impl From<RequestTableError> for V13SubmitErrorKind {
    fn from(_error: RequestTableError) -> Self {
        Self::RequestTable
    }
}

impl From<MaintenanceSubmitError> for V13SubmitErrorKind {
    fn from(error: MaintenanceSubmitError) -> Self {
        Self::Maintenance(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(in crate::block::activation_v13) enum RequestRuntimeBuildError {
    #[error("v0.13 hardware queue has zero depth")]
    ZeroQueueDepth,
    #[error("v0.13 ownership domain queue topology is invalid")]
    InvalidDomainTopology,
    #[error("v0.13 interrupt domain declared inline queue execution")]
    InlineExecutionInInterruptDomain,
    #[error("v0.13 ownership domain request capacity overflowed")]
    CapacityOverflow,
    #[error(transparent)]
    RequestTable(RequestTableError),
    #[error("v0.13 device runtime requires a valid frozen CPU count")]
    InvalidCpuCount,
    #[error("v0.13 logical device has no queue route")]
    UnroutedDevice,
    #[error("v0.13 route refers to missing hardware queue {0}")]
    MissingQueue(usize),
    #[error("v0.13 route refers to missing ownership domain {0:?}")]
    MissingDomain(OwnershipDomainId),
    #[error("v0.13 logical device {0:?} has no immutable route")]
    MissingDeviceRoute(LogicalDeviceId),
    #[error("v0.13 ownership domain software-context map was already frozen")]
    SoftwareContextMapAlreadyFrozen,
}

#[cfg(test)]
mod tests {
    use core::num::{NonZeroU16, NonZeroU64};

    use rdif_block::{
        DriverDeviceKey, IdList, InterruptQueueDesc, LogicalDeviceSelector, OwnedRequest,
        OwnershipDomainId, QueueExecution, RequestFlags, RequestOp,
    };

    use super::*;

    #[test]
    fn hardware_credit_one_prevents_a_second_driver_dispatch() {
        let credits = HardwareCredits::new(1).unwrap();
        credits.try_reserve().unwrap().retain_for_inflight();

        assert!(credits.try_reserve().is_none());
        assert_eq!(credits.in_use(), 1);

        credits.release_inflight();
        assert!(credits.try_reserve().is_some());
    }

    #[test]
    fn cpu_to_hctx_route_is_stable_for_the_activation_lifetime() {
        let queues = [7, 11];

        assert_eq!(mapped_queue_for_cpu(&queues, 0), Some(7));
        assert_eq!(mapped_queue_for_cpu(&queues, 1), Some(11));
        assert_eq!(mapped_queue_for_cpu(&queues, 2), Some(7));
        assert_eq!(mapped_queue_for_cpu(&queues, 3), Some(11));
        assert_eq!(mapped_queue_for_cpu(&queues, 2), Some(7));
        assert_eq!(mapped_queue_for_cpu(&[], 0), None);
    }

    #[test]
    fn request_table_can_stage_while_the_only_hardware_credit_is_inflight() {
        let domain = OwnershipDomainId::new(1).unwrap();
        let runtime = DomainRequestRuntime::new(
            domain,
            &[queue(domain, 1)],
            crate::block::BlockRuntimeConfig::default(),
        )
        .unwrap();
        let first = runtime
            .requests()
            .reserve(0, driver_device(), flush_request())
            .unwrap();
        let credit = runtime.hctxs()[0].credits().try_reserve().unwrap();
        let _inflight = runtime.requests().begin_dispatch(first, 100).unwrap();
        credit.retain_for_inflight();

        assert!(
            runtime
                .requests()
                .reserve(0, driver_device(), flush_request())
                .is_ok()
        );
        assert!(runtime.hctxs()[0].credits().try_reserve().is_none());
    }

    #[test]
    fn software_ctx_to_hctx_map_can_only_be_frozen_once() {
        let domain = OwnershipDomainId::new(1).unwrap();
        let runtime = DomainRequestRuntime::new(
            domain,
            &[queue(domain, 1)],
            crate::block::BlockRuntimeConfig::default(),
        )
        .unwrap();
        let empty_hctx = || {
            let contexts: Box<[Arc<SoftwareCtxIngress>]> = Box::new([]);
            FrozenSoftwareCtxMap::new(Box::new([contexts]))
        };

        assert!(runtime.install_software_contexts(empty_hctx()).is_ok());
        assert_eq!(
            runtime.install_software_contexts(empty_hctx()),
            Err(RequestRuntimeBuildError::SoftwareContextMapAlreadyFrozen)
        );
    }

    fn queue(domain: OwnershipDomainId, depth: u16) -> InterruptQueueDesc {
        let mut sources = IdList::none();
        sources.insert(1);
        InterruptQueueDesc::new(
            0,
            LogicalDeviceSelector::AllPublished,
            domain,
            QueueExecution::Tagged,
            NonZeroU16::new(depth).unwrap(),
            sources,
        )
        .unwrap()
    }

    fn driver_device() -> DriverDeviceKey {
        DriverDeviceKey::new(NonZeroU64::new(1).unwrap())
    }

    fn flush_request() -> OwnedRequest {
        OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        }
    }
}
