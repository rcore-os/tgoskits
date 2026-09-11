use alloc::{sync::Weak, vec, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use axdevice_base::{
    ControllerInputId, DeviceAccess, InterruptControllerId, InterruptEndpoint, InterruptSharing,
    InterruptTrigger, IrqError, IrqResult, Resource, WiredIrqInput, WiredIrqSink,
};

use super::*;
use crate::{
    ConfigOffset, PciCapabilityEffectAccess, PciCapabilityEffectRegion, PciCapabilityId,
    PciCapabilitySpec, PciClass, PciConfigEffectId, PciEndpointIdentity, PciError, PciFunctionSpec,
    PciIntxPin, PciIntxRequirement, PciIntxRouter, PciTopologyBuilder, ResourceSlot,
};

static ORPHAN_QUEUE_TEST_LOCK: Mutex<()> = Mutex::new(());

struct StubFunction {
    fail_command: bool,
}

struct ReentrantLifecycleFunction {
    binding: Weak<PciRootBinding>,
}

struct ToggleCommandFunction {
    fail_command: AtomicBool,
}

struct FailingDeassertSink {
    fail_deassert: AtomicBool,
    asserted: AtomicBool,
}

struct BlockingWithdrawalFunction {
    started: AtomicBool,
    release: AtomicBool,
    withdrawals: AtomicUsize,
}

impl Device for BlockingWithdrawalFunction {
    fn name(&self) -> &str {
        "blocking-withdrawal-function"
    }

    fn resources(&self) -> &[Resource] {
        &[]
    }

    fn read(&self, _access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        Err(DeviceError::NotFound)
    }

    fn write(
        &self,
        _access: &DeviceAccess,
        _value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        Err(DeviceError::NotFound)
    }
}

impl PciFunction for BlockingWithdrawalFunction {
    fn read_bar(
        &self,
        _access: PciBarAccess,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult<u64> {
        Ok(0)
    }

    fn write_bar(
        &self,
        _access: PciBarAccess,
        _value: u64,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult {
        Ok(())
    }

    fn withdraw_irq(&self, _permit: &mut EndpointIrqTransitionPermit) -> DeviceResult {
        self.started.store(true, Ordering::Release);
        while !self.release.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        self.withdrawals.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

fn pending_withdrawal(device: u32, function: Arc<dyn PciFunction>) -> PendingIrqWithdrawal {
    let admission = Arc::new(EndpointAdmission::new(
        EndpointBindingGeneration(1),
        RoutedAdmissionEpoch(1),
    ));
    admission.close();
    PendingIrqWithdrawal {
        device: DeviceId::new(device),
        function,
        admission,
    }
}

impl WiredIrqSink for FailingDeassertSink {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        if !asserted && self.fail_deassert.load(Ordering::Relaxed) {
            return Err(IrqError::Backend {
                endpoint: InterruptEndpoint::Wired {
                    controller: InterruptControllerId::new(0),
                    input,
                },
                operation: "test deassert",
                detail: "injected test failure".into(),
            });
        }
        self.asserted.store(asserted, Ordering::Relaxed);
        Ok(())
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        Err(IrqError::Backend {
            endpoint: InterruptEndpoint::Wired {
                controller: InterruptControllerId::new(0),
                input,
            },
            operation: "test pulse",
            detail: "not used by this test".into(),
        })
    }
}

impl Device for StubFunction {
    fn name(&self) -> &str {
        "stub-pci-function"
    }
    fn resources(&self) -> &[Resource] {
        &[]
    }
    fn read(&self, _access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        Err(DeviceError::NotFound)
    }
    fn write(
        &self,
        _access: &DeviceAccess,
        _value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        Ok(())
    }
}

impl PciFunction for StubFunction {
    fn read_bar(
        &self,
        _access: PciBarAccess,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult<u64> {
        Ok(0)
    }
    fn write_bar(
        &self,
        _access: PciBarAccess,
        _value: u64,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult {
        Ok(())
    }

    fn command_changed(
        &self,
        _command: PciCommandState,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult {
        if self.fail_command {
            return Err(DeviceError::Unsupported {
                operation: "synchronize PCI command state",
                detail: "test endpoint rejected the command transition".into(),
            });
        }
        Ok(())
    }

    fn reset(&self, _command: PciCommandState) -> DeviceResult {
        Err(DeviceError::Unsupported {
            operation: "reset PCI endpoint",
            detail: "test endpoint does not implement reset".into(),
        })
    }
}

impl Device for ReentrantLifecycleFunction {
    fn name(&self) -> &str {
        "reentrant-lifecycle-function"
    }

    fn resources(&self) -> &[Resource] {
        &[]
    }

    fn read(&self, _access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        Err(DeviceError::NotFound)
    }

    fn write(
        &self,
        _access: &DeviceAccess,
        _value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        Ok(())
    }
}

impl PciFunction for ReentrantLifecycleFunction {
    fn read_bar(
        &self,
        _access: PciBarAccess,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult<u64> {
        Ok(0)
    }

    fn write_bar(
        &self,
        _access: PciBarAccess,
        _value: u64,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult {
        Ok(())
    }

    fn command_changed(
        &self,
        _command: PciCommandState,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult {
        let binding = self.binding.upgrade().ok_or(DeviceError::InvalidState {
            operation: "re-enter PCI lifecycle from command callback",
            detail: "test binding was dropped".into(),
        })?;
        assert!(binding.lifecycle.try_lock_irqsave().is_some());
        assert!(matches!(
            binding.reset_lifecycle(),
            Err(DeviceManagerError::InvalidState { .. })
        ));
        Ok(())
    }
}

impl Device for ToggleCommandFunction {
    fn name(&self) -> &str {
        "toggle-command-function"
    }

    fn resources(&self) -> &[Resource] {
        &[]
    }

    fn read(&self, _access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        Err(DeviceError::NotFound)
    }

    fn write(
        &self,
        _access: &DeviceAccess,
        _value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        Ok(())
    }
}

impl PciFunction for ToggleCommandFunction {
    fn read_bar(
        &self,
        _access: PciBarAccess,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult<u64> {
        Ok(0)
    }

    fn write_bar(
        &self,
        _access: PciBarAccess,
        _value: u64,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult {
        Ok(())
    }

    fn command_changed(
        &self,
        _command: PciCommandState,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult {
        if self.fail_command.load(Ordering::Acquire) {
            return Err(DeviceError::Unsupported {
                operation: "synchronize PCI command state",
                detail: "test endpoint rejected the command transition".into(),
            });
        }
        Ok(())
    }
}

struct RecordingFunction {
    root: Arc<PciRootState>,
    bdf: PciBdf,
    reads: SpinLock<Vec<(PciConfigReadEffect, DeviceId, u64)>>,
    writes: SpinLock<Vec<(PciConfigWriteEffect, DeviceId)>>,
    commands: SpinLock<Vec<(PciCommandState, DeviceId)>>,
    resets: SpinLock<Vec<PciCommandState>>,
    reset_failures: SpinLock<usize>,
    withdrawals: SpinLock<usize>,
    withdraw_failures: SpinLock<usize>,
    irq_line: Option<IrqLine>,
    supports_effects: bool,
    pending: bool,
}

impl Device for RecordingFunction {
    fn name(&self) -> &str {
        "recording-pci-function"
    }

    fn resources(&self) -> &[Resource] {
        &[]
    }

    fn read(&self, _access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        Err(DeviceError::NotFound)
    }

    fn write(
        &self,
        _access: &DeviceAccess,
        _value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        Ok(())
    }
}

impl PciFunction for RecordingFunction {
    fn intx_pending(&self) -> bool {
        // A dynamic status query must not retain the root state lock while
        // entering endpoint-owned behavior.
        let _ = self
            .root
            .read_config(self.bdf, ConfigOffset::new(0).unwrap(), AccessWidth::Dword);
        self.pending
    }

    fn supported_config_effects(&self) -> &[PciConfigEffectId] {
        const EFFECTS: &[PciConfigEffectId] = &[PciConfigEffectId::new(7)];
        if self.supports_effects { EFFECTS } else { &[] }
    }

    fn read_bar(
        &self,
        _access: PciBarAccess,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult<u64> {
        Ok(0)
    }

    fn write_bar(
        &self,
        _access: PciBarAccess,
        _value: u64,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult {
        Ok(())
    }

    fn read_config_effect(
        &self,
        effect: PciConfigReadEffect,
        context: &mut dyn PciEndpointContext,
    ) -> DeviceResult<u64> {
        // This nested root read proves that dispatch released the root
        // state lock before entering endpoint-owned behavior.
        let vendor_device = self
            .root
            .read_config(self.bdf, ConfigOffset::new(0).unwrap(), AccessWidth::Dword)
            .map_err(|error| DeviceError::InvalidInput {
                operation: "read recording PCI function",
                detail: alloc::format!("{error}"),
            })?;
        self.reads
            .lock_irqsave()
            .push((effect, context.device_id(), vendor_device));
        Ok(0x5a)
    }

    fn write_config_effect(
        &self,
        effect: PciConfigWriteEffect,
        context: &mut dyn PciEndpointContext,
    ) -> DeviceResult {
        self.writes
            .lock_irqsave()
            .push((effect, context.device_id()));
        Ok(())
    }

    fn command_changed(
        &self,
        command: PciCommandState,
        context: &mut dyn PciEndpointContext,
    ) -> DeviceResult {
        // Endpoint callbacks are allowed to re-enter root-owned config
        // reads. This also keeps the initial owner-side synchronization
        // independent from the root's publication state machine.
        self.root
            .read_config(self.bdf, ConfigOffset::new(0).unwrap(), AccessWidth::Dword)
            .map_err(|error| DeviceError::InvalidInput {
                operation: "re-enter recording PCI root from command callback",
                detail: alloc::format!("{error}"),
            })?;
        self.commands
            .lock_irqsave()
            .push((command, context.device_id()));
        Ok(())
    }

    fn reset(&self, command: PciCommandState) -> DeviceResult {
        self.resets.lock_irqsave().push(command);
        let mut failures = self.reset_failures.lock_irqsave();
        if *failures != 0 {
            *failures -= 1;
            return Err(DeviceError::Backend {
                operation: "reset test PCI endpoint",
                detail: "injected test failure".into(),
            });
        }
        Ok(())
    }

    fn withdraw_irq(&self, permit: &mut EndpointIrqTransitionPermit) -> DeviceResult {
        let mut failures = self.withdraw_failures.lock_irqsave();
        if *failures != 0 {
            *failures -= 1;
            return Err(DeviceError::Backend {
                operation: "withdraw test PCI endpoint IRQ",
                detail: "injected test failure".into(),
            });
        }
        if let Some(line) = &self.irq_line {
            permit.deassert(line)?;
        }
        *self.withdrawals.lock_irqsave() += 1;
        Ok(())
    }
}

fn router() -> EndpointRouter {
    EndpointRouter::new()
}

struct ConfigEffectRouteFixture {
    binding: Arc<PciRootBinding>,
    root: Arc<PciRootState>,
    bdf: PciBdf,
    effect_offset: ConfigOffset,
    recording: Arc<RecordingFunction>,
    lease: PciBindingLease,
}

fn config_effect_route_fixture(reset_failures: usize) -> ConfigEffectRouteFixture {
    let effect = PciCapabilityEffectRegion::new(
        PciConfigEffectId::new(7),
        8,
        6,
        PciCapabilityEffectAccess::ReadWrite,
    )
    .unwrap();
    let capability = PciCapabilitySpec::new(
        PciCapabilityId::new(9),
        vec![0, 0, 0x11, 0x22, 0x33, 0x44, 0, 0, 0, 0, 0, 0, 0, 0],
        vec![0, 0, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .unwrap()
    .with_effect(effect)
    .unwrap();
    let function_id = DeviceNodeId::new("reset-route-effect-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(
            PciFunctionSpec::new(
                function_id.clone(),
                PciEndpointIdentity::new(0x1af4, 0x1041, PciClass::new(0xff, 0, 0)),
            )
            .with_capability(capability)
            .with_intx(PciIntxRequirement::new(
                PciIntxPin::A,
                ResourceSlot::new("reset-route-intx").unwrap(),
            ))
            .unwrap(),
        )
        .unwrap();
    let route = PciIntxRouter::new(
        InterruptControllerId::new(0),
        [
            ControllerInputId::new(16),
            ControllerInputId::new(17),
            ControllerInputId::new(18),
            ControllerInputId::new(19),
        ],
        [16, 17, 18, 19],
        InterruptTrigger::LevelTriggered,
        InterruptSharing::Shared,
    )
    .resolve(&function_id, PciBdf::bus_zero(0), PciIntxPin::A)
    .unwrap();
    builder.set_intx_route(&function_id, route).unwrap();

    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let root = Arc::new(PciRootState::new(Arc::clone(&topology)));
    let bdf = topology.function(&function_id).unwrap().bdf();
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("reset-route-effect-host").unwrap(),
        Arc::clone(&root),
    ));
    let recording = Arc::new(RecordingFunction {
        root: Arc::clone(&root),
        bdf,
        reads: SpinLock::new(Vec::new()),
        writes: SpinLock::new(Vec::new()),
        commands: SpinLock::new(Vec::new()),
        resets: SpinLock::new(Vec::new()),
        reset_failures: SpinLock::new(reset_failures),
        withdrawals: SpinLock::new(0),
        withdraw_failures: SpinLock::new(0),
        irq_line: None,
        supports_effects: true,
        pending: false,
    });
    let mut grants = Vec::new();
    let lease = binding
        .bind_registered(
            &function_id,
            DeviceId::new(27),
            recording.clone(),
            &mut grants,
        )
        .unwrap();
    let capability_offset = topology
        .function(&function_id)
        .unwrap()
        .capabilities()
        .next()
        .unwrap()
        .offset()
        .value();
    let effect_offset = ConfigOffset::new(capability_offset + 8).unwrap();

    ConfigEffectRouteFixture {
        binding,
        root,
        bdf,
        effect_offset,
        recording,
        lease,
    }
}

fn config_effect_snapshot(
    root: &PciRootState,
    bdf: PciBdf,
    effect_offset: ConfigOffset,
) -> (EndpointRouteToken, PciCommandState, PciConfigReadEffect) {
    match root
        .prepare_read_config(bdf, effect_offset, AccessWidth::Dword)
        .unwrap()
    {
        crate::pci::root::PciConfigReadOutcome::Effect {
            token,
            command,
            effect,
        } => (token, command, *effect),
        _ => panic!("expected an admitted PCI config-effect snapshot"),
    }
}

mod cleanup;
mod dispatch;
mod interrupt;
mod lifecycle;
mod routing;
