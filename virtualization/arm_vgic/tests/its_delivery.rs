use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};

use arm_vgic::{
    CpuInterfaceState, EventId, GicAffinity, GicV3Backend, GicV3BackendError, GicV3Config,
    GicV3Controller, GicV3MmioRegion, GicV3SpiOwnership, GicV3VcpuBinding, GicV3VcpuWake,
    GicVcpuId, GuestMemory, GuestMemoryError, IntId, InterruptState, ItsDeviceId,
    SoftwareGicV3Backend, VgicError, VgicResult,
};
use axdevice_base::ItsId;
use axvm_types::AccessWidth;

const GITS_CTLR: u64 = 0x0000;
const GITS_CBASER: u64 = 0x0080;
const GITS_CWRITER: u64 = 0x0088;
const GITS_CREADR: u64 = 0x0090;
const GITS_BASER: u64 = 0x0100;
const GITS_PIDR0: u64 = 0xffe0;
const GITS_PIDR2: u64 = 0xffe8;
const COMMAND_SIZE: u64 = 32;
const QUEUE_SIZE: usize = 0x1000;

mod support;

#[test]
fn clearing_a_loaded_lpi_preserves_its_lr_until_hardware_is_saved() {
    let backend = Arc::new(TrackingBackend::default());
    let (controller, binding, memory) = controller_with_its_backend(1, 32, backend.clone());
    enable_lpis(&controller, 0);
    initialize_its(&controller, &memory);

    memory.write_command(0, mapd(7, 8));
    memory.write_command(0x20, mapc(3, 0));
    memory.write_command(0x40, mapti(7, 5, 8192, 3));
    memory.write_command(0x60, command(0x03, 7, 5, 0, 0));
    controller
        .write_its(GITS_CWRITER, AccessWidth::Qword, 0x80)
        .unwrap();

    binding.load().unwrap();
    assert_eq!(backend.loaded_intids(), vec![IntId::new(8192).unwrap()]);

    memory.write_command(0x80, command(0x04, 7, 5, 0, 0));
    controller
        .write_its(GITS_CWRITER, AccessWidth::Qword, 0xa0)
        .unwrap();
    binding.save().unwrap();
    binding.load().unwrap();
    assert!(backend.loaded_intids().is_empty());
    binding.save().unwrap();
}

#[test]
fn two_its_instances_keep_device_event_namespaces_isolated() {
    let memory = Arc::new(TestGuestMemory::new(0x4100_0000, QUEUE_SIZE * 2));
    let config = GicV3Config::new(
        GicV3SpiOwnership::AllGuestOwned,
        GicV3MmioRegion::new(0x0800_0000, 0x1_0000).unwrap(),
        GicV3MmioRegion::new(0x080a_0000, 0x2_0000).unwrap(),
        0x2_0000,
        1,
    )
    .unwrap()
    .with_spi_count(32)
    .unwrap()
    .with_its_instances(vec![
        (
            ItsId::new(0),
            GicV3MmioRegion::new(0x0808_0000, 0x1_0000).unwrap(),
        ),
        (
            ItsId::new(1),
            GicV3MmioRegion::new(0x0809_0000, 0x1_0000).unwrap(),
        ),
    ])
    .unwrap();
    let controller = GicV3Controller::new_with_guest_memory(
        config,
        Arc::new(SoftwareGicV3Backend),
        Some(memory.clone()),
    )
    .unwrap();
    let binding = controller
        .attach_vcpu(
            GicVcpuId::new(0),
            GicAffinity::new(0, 0, 0, 0),
            Arc::new(NoopWake),
        )
        .unwrap();
    enable_lpis(&controller, 0);

    for (its, queue_offset, lpi) in [
        (ItsId::new(0), 0, 8192),
        (ItsId::new(1), QUEUE_SIZE as u64, 8193),
    ] {
        controller
            .write_its_for(
                its,
                GITS_CBASER,
                AccessWidth::Qword,
                memory.base() + queue_offset,
            )
            .unwrap();
        controller
            .write_its_for(its, GITS_CTLR, AccessWidth::Dword, 1)
            .unwrap();
        memory.write_command(queue_offset, mapd(7, 8));
        memory.write_command(queue_offset + 0x20, mapc(3, 0));
        memory.write_command(queue_offset + 0x40, mapti(7, 5, lpi, 3));
        controller
            .write_its_for(its, GITS_CWRITER, AccessWidth::Qword, 0x60)
            .unwrap();
        controller
            .configure_msi_input_for(
                its,
                ItsDeviceId::new(7),
                EventId::new(5),
                Some(arm_vgic::LpiId::new(lpi).unwrap()),
            )
            .unwrap();
        controller
            .signal_msi_for(its, ItsDeviceId::new(7), EventId::new(5))
            .unwrap();
    }

    binding.load().unwrap();
    let mut loaded = binding
        .cpu_interface_snapshot()
        .unwrap()
        .list_registers()
        .iter()
        .flatten()
        .map(|entry| entry.intid().raw())
        .collect::<Vec<_>>();
    loaded.sort_unstable();
    assert_eq!(loaded, vec![8192, 8193]);
}

#[test]
fn non_identity_ring_queue_translates_msi_to_target_lpi() {
    let (controller, binding, memory) = controller_with_its(1, 256);
    enable_lpis(&controller, 0);
    initialize_its(&controller, &memory);

    memory.write_command(0x00, mapd(7, 8));
    memory.write_command(0x20, mapc(3, 0));
    memory.write_command(0x40, mapti(7, 5, 8192, 3));
    controller
        .write_its(GITS_CWRITER, AccessWidth::Qword, 0x60)
        .unwrap();

    for offset in (0x60..0xfe0).step_by(COMMAND_SIZE as usize) {
        memory.write_command(offset, command(0x05, 0, 0, 0, 0));
    }
    controller
        .write_its(GITS_CWRITER, AccessWidth::Qword, 0xfe0)
        .unwrap();
    memory.write_command(0xfe0, command(0x03, 7, 5, 0, 0));
    controller
        .write_its(GITS_CWRITER, AccessWidth::Qword, 0)
        .unwrap();

    binding.load().unwrap();
    let loaded: Vec<_> = binding
        .cpu_interface_snapshot()
        .unwrap()
        .list_registers()
        .iter()
        .flatten()
        .map(|entry| entry.intid())
        .collect();
    assert_eq!(loaded, vec![IntId::new(8192).unwrap()]);
    assert_eq!(
        controller
            .read_its(GITS_CREADR, AccessWidth::Qword)
            .unwrap(),
        0
    );

    let (isolated, ..) = controller_with_its(1, 256);
    assert!(matches!(
        isolated.signal_msi(ItsDeviceId::new(7), EventId::new(5)),
        Err(VgicError::ResourceNotFound { .. })
    ));
}

#[test]
fn command_budget_rejects_unbounded_guest_work() {
    let (controller, _, memory) = controller_with_its(1, 1);
    initialize_its(&controller, &memory);
    memory.write_command(0, command(0x05, 0, 0, 0, 0));
    memory.write_command(0x20, command(0x05, 0, 0, 0, 0));

    assert_eq!(
        controller.write_its(GITS_CWRITER, AccessWidth::Qword, 0x40),
        Err(VgicError::ItsCommandBudgetExceeded {
            budget: 1,
            offset: 0,
        })
    );
}

/// The software ITS copies the guest command queue and reads whatever native
/// controller state the embedder needs while a command word is being copied.
///
/// That state is guarded by the native raw state lock. If command processing
/// still ran under that raw lock, the native read performed from inside
/// [`GuestMemory::read`] on a helper task would block until the bounded wait
/// fails the test. A passing run therefore proves the guest copy happens with
/// the native state lock released.
#[test]
fn guest_memory_read_reenters_native_controller_state() {
    let memory = Arc::new(ReentrantGuestMemory::new(0x4200_0000, QUEUE_SIZE));
    let config = GicV3Config::new(
        GicV3SpiOwnership::AllGuestOwned,
        GicV3MmioRegion::new(0x0800_0000, 0x1_0000).unwrap(),
        GicV3MmioRegion::new(0x080a_0000, 0x2_0000).unwrap(),
        0x2_0000,
        1,
    )
    .unwrap()
    .with_spi_count(32)
    .unwrap()
    .with_its(GicV3MmioRegion::new(0x0808_0000, 0x2_0000).unwrap())
    .unwrap()
    .with_its_command_budget(32)
    .unwrap();
    let controller = Arc::new(
        GicV3Controller::new_with_guest_memory(
            config,
            Arc::new(SoftwareGicV3Backend),
            Some(memory.clone()),
        )
        .unwrap(),
    );
    let _binding = controller
        .attach_vcpu(
            GicVcpuId::new(0),
            GicAffinity::new(0, 0, 0, 0),
            Arc::new(NoopWake),
        )
        .unwrap();

    // The probe is installed only after the vCPU exists so the re-entrant read
    // observes a fully attached native controller.
    memory.install_probe(Arc::downgrade(&controller), GicVcpuId::new(0));

    enable_lpis(&controller, 0);
    controller
        .write_its(GITS_CBASER, AccessWidth::Qword, memory.base())
        .unwrap();
    memory.write_command(0x00, mapd(7, 8));
    memory.write_command(0x20, mapc(3, 0));
    memory.write_command(0x40, mapti(7, 5, 8192, 3));
    memory.write_command(0x60, command(0x03, 7, 5, 0, 0));
    controller
        .write_its(GITS_CTLR, AccessWidth::Dword, 1)
        .unwrap();
    controller
        .write_its(GITS_CWRITER, AccessWidth::Qword, 0x80)
        .unwrap();

    assert!(
        memory.probe_completed(),
        "the guest-memory copy never re-entered the native controller state"
    );
    // The re-entrant read is only a probe: the INT command decoded while the
    // helper read native state still reached the Redistributor.
    assert_eq!(
        controller
            .interrupt_state(Some(GicVcpuId::new(0)), IntId::new(8192).unwrap())
            .unwrap(),
        InterruptState::Pending,
        "the INT command decoded during a re-entrant guest copy did not take effect"
    );
}

#[test]
fn disabled_its_records_the_writer_and_consumes_it_when_enabled() {
    let (controller, _, memory) = controller_with_its(1, 32);
    controller
        .write_its(GITS_CBASER, AccessWidth::Qword, memory.base())
        .unwrap();
    memory.write_command(0, command(0x05, 0, 0, 0, 0));

    controller
        .write_its(GITS_CWRITER, AccessWidth::Qword, COMMAND_SIZE)
        .unwrap();

    assert_eq!(
        controller
            .read_its(GITS_CWRITER, AccessWidth::Qword)
            .unwrap(),
        COMMAND_SIZE
    );
    assert_eq!(
        controller
            .read_its(GITS_CREADR, AccessWidth::Qword)
            .unwrap(),
        0
    );

    controller
        .write_its(GITS_CTLR, AccessWidth::Dword, 1)
        .unwrap();
    assert_eq!(
        controller
            .read_its(GITS_CREADR, AccessWidth::Qword)
            .unwrap(),
        COMMAND_SIZE
    );
}

/// A drain that consumes part of the command queue and then rejects a command
/// keeps both its CREADR progress and the delivery effects it already decoded.
///
/// The consumed INT command has already moved CREADR past itself, so dropping
/// its decoded LPI effect would lose the interrupt permanently: no later write
/// replays that command.
#[test]
fn consumed_commands_keep_their_delivery_effects_when_a_later_command_fails() {
    let (controller, _binding, memory) = controller_with_its(1, 32);
    let vcpu = GicVcpuId::new(0);
    enable_lpis(&controller, 0);
    initialize_its(&controller, &memory);

    memory.write_command(0x00, mapd(7, 8));
    memory.write_command(0x20, mapc(3, 0));
    memory.write_command(0x40, mapti(7, 5, 8192, 3));
    memory.write_command(0x60, command(0x03, 7, 5, 0, 0));
    // Processor 4 has no attached Redistributor, so this command is rejected
    // only after the INT command above was already consumed.
    memory.write_command(0x80, mapc(4, 4));

    assert!(matches!(
        controller.write_its(GITS_CWRITER, AccessWidth::Qword, 0xa0),
        Err(VgicError::InvalidItsCommand {
            opcode: 0x09,
            offset: 0x80,
            ..
        })
    ));
    // The four accepted commands advanced CREADR; the rejected one stays for a
    // later retry.
    assert_eq!(
        controller
            .read_its(GITS_CREADR, AccessWidth::Qword)
            .unwrap(),
        0x80
    );
    // The LPI decoded from the consumed INT command survived the failure.
    assert_eq!(
        controller
            .interrupt_state(Some(vcpu), IntId::new(8192).unwrap())
            .unwrap(),
        InterruptState::Pending,
        "a consumed INT command lost its decoded LPI delivery effect"
    );
}

#[test]
fn command_queue_accepts_the_full_cbaser_size_range() {
    let (controller, _, memory) = controller_with_its(1, 32);
    let one_mebibyte_queue = memory.base() | 0xff;
    controller
        .write_its(GITS_CBASER, AccessWidth::Qword, one_mebibyte_queue)
        .unwrap();
    controller
        .write_its(GITS_CWRITER, AccessWidth::Qword, 0x0f_ffe0)
        .unwrap();

    assert_eq!(
        controller
            .read_its(GITS_CWRITER, AccessWidth::Qword)
            .unwrap(),
        0x0f_ffe0
    );
}

#[test]
fn its_wide_registers_support_word_half_reads() {
    let (controller, _, memory) = controller_with_its(1, 32);
    controller
        .write_its(GITS_CBASER, AccessWidth::Qword, memory.base())
        .unwrap();

    for register in [0x0008, GITS_CBASER, GITS_CWRITER, GITS_CREADR, GITS_BASER] {
        let full = controller.read_its(register, AccessWidth::Qword).unwrap();
        assert_eq!(
            controller.read_its(register, AccessWidth::Dword).unwrap(),
            full & u64::from(u32::MAX)
        );
        assert_eq!(
            controller
                .read_its(register + 4, AccessWidth::Dword)
                .unwrap(),
            full >> 32
        );
    }
}

#[test]
fn its_identification_registers_describe_the_software_model() {
    let (controller, ..) = controller_with_its(1, 32);

    let typer = controller.read_its(0x0008, AccessWidth::Qword).unwrap();
    assert_ne!(typer & 1, 0, "Physical LPIs must be advertised");
    assert_eq!(((typer >> 4) & 0xf) + 1, 8, "ITE size must be 8 bytes");
    assert_eq!(((typer >> 8) & 0x1f) + 1, 24);
    assert_eq!(typer & (1 << 19), 0, "MAPC uses processor numbers");

    let device_baser = controller.read_its(GITS_BASER, AccessWidth::Qword).unwrap();
    let collection_baser = controller
        .read_its(GITS_BASER + 8, AccessWidth::Qword)
        .unwrap();
    assert_eq!((device_baser >> 56) & 0x7, 1);
    assert_eq!((collection_baser >> 56) & 0x7, 4);

    assert_ne!(
        controller.read_its(GITS_CTLR, AccessWidth::Dword).unwrap() & (1 << 31),
        0,
        "a disabled, idle ITS must be quiescent"
    );
    let memory = TestGuestMemory::new(0x5000_0000, QUEUE_SIZE);
    initialize_its(&controller, &memory);
    assert_ne!(
        controller.read_its(GITS_CTLR, AccessWidth::Dword).unwrap() & (1 << 31),
        0,
        "an enabled, idle software ITS must be quiescent"
    );
    assert_eq!(
        controller.read_its(GITS_PIDR0, AccessWidth::Dword).unwrap(),
        0x94
    );
    assert_eq!(
        controller.read_its(GITS_PIDR2, AccessWidth::Dword).unwrap(),
        0x3b
    );
}

#[test]
fn mapc_uses_redistributor_processor_number() {
    let (controller, _, memory) = controller_with_its(2, 32);
    let target = controller
        .attach_vcpu(
            GicVcpuId::new(1),
            GicAffinity::new(4, 3, 2, 1),
            Arc::new(NoopWake),
        )
        .unwrap();
    enable_lpis(&controller, 1);
    initialize_its(&controller, &memory);

    memory.write_command(0x00, mapd(7, 8));
    memory.write_command(0x20, mapc(3, 1));
    memory.write_command(0x40, mapti(7, 5, 8192, 3));
    memory.write_command(0x60, command(0x03, 7, 5, 0, 0));
    controller
        .write_its(GITS_CWRITER, AccessWidth::Qword, 0x80)
        .unwrap();

    target.load().unwrap();
    let loaded: Vec<_> = target
        .cpu_interface_snapshot()
        .unwrap()
        .list_registers()
        .iter()
        .flatten()
        .map(|entry| entry.intid())
        .collect();
    assert_eq!(loaded, vec![IntId::new(8192).unwrap()]);
}

#[test]
fn linux_command_set_maps_moves_clears_invalidates_and_discards() {
    let (controller, binding0, memory) = controller_with_its(2, 32);
    let binding1 = controller
        .attach_vcpu(
            GicVcpuId::new(1),
            GicAffinity::new(0, 0, 0, 1),
            Arc::new(NoopWake),
        )
        .unwrap();
    enable_lpis(&controller, 0);
    enable_lpis(&controller, 1);
    initialize_its(&controller, &memory);

    let commands = [
        mapd(9, 14),
        mapc(1, 0),
        mapc(2, 1),
        command(0x0b, 9, 8192, 1, 0),
        command(0x01, 9, 8192, 2, 0),
        command(0x03, 9, 8192, 0, 0),
        command(0x04, 9, 8192, 0, 0),
        command(0x0c, 9, 8192, 0, 0),
        command(0x0d, 0, 0, 2, 0),
        command(0x05, 0, 0, 0, 0),
        command(0x0f, 9, 8192, 0, 0),
    ];
    for (index, words) in commands.into_iter().enumerate() {
        memory.write_command(index as u64 * COMMAND_SIZE, words);
    }
    controller
        .write_its(GITS_CWRITER, AccessWidth::Qword, 11 * COMMAND_SIZE)
        .unwrap();

    binding0.load().unwrap();
    binding1.load().unwrap();
    assert!(
        binding0
            .cpu_interface_snapshot()
            .unwrap()
            .list_registers()
            .iter()
            .all(Option::is_none)
    );
    assert!(
        binding1
            .cpu_interface_snapshot()
            .unwrap()
            .list_registers()
            .iter()
            .all(Option::is_none)
    );
    assert!(matches!(
        controller.signal_msi(ItsDeviceId::new(9), EventId::new(8192)),
        Err(VgicError::ResourceNotFound { .. })
    ));
}

fn controller_with_its(
    vcpu_count: usize,
    budget: usize,
) -> (GicV3Controller, GicV3VcpuBinding, Arc<TestGuestMemory>) {
    controller_with_its_backend(vcpu_count, budget, Arc::new(SoftwareGicV3Backend))
}

fn controller_with_its_backend(
    vcpu_count: usize,
    budget: usize,
    backend: Arc<dyn GicV3Backend>,
) -> (GicV3Controller, GicV3VcpuBinding, Arc<TestGuestMemory>) {
    let memory = Arc::new(TestGuestMemory::new(0x4000_0000, QUEUE_SIZE));
    let config = GicV3Config::new(
        GicV3SpiOwnership::AllGuestOwned,
        GicV3MmioRegion::new(0x0800_0000, 0x1_0000).unwrap(),
        GicV3MmioRegion::new(0x080a_0000, 0x2_0000 * vcpu_count as u64).unwrap(),
        0x2_0000,
        vcpu_count,
    )
    .unwrap()
    .with_spi_count(32)
    .unwrap()
    .with_its(GicV3MmioRegion::new(0x0808_0000, 0x2_0000).unwrap())
    .unwrap()
    .with_its_command_budget(budget)
    .unwrap();
    let controller =
        GicV3Controller::new_with_guest_memory(config, backend, Some(memory.clone())).unwrap();
    let binding = controller
        .attach_vcpu(
            GicVcpuId::new(0),
            GicAffinity::new(0, 0, 0, 0),
            Arc::new(NoopWake),
        )
        .unwrap();
    (controller, binding, memory)
}

fn initialize_its(controller: &GicV3Controller, memory: &TestGuestMemory) {
    controller
        .write_its(GITS_CBASER, AccessWidth::Qword, memory.base())
        .unwrap();
    controller
        .write_its(GITS_CTLR, AccessWidth::Dword, 1)
        .unwrap();
}

fn enable_lpis(controller: &GicV3Controller, raw_vcpu: usize) {
    controller
        .write_redistributor(GicVcpuId::new(raw_vcpu), 0, AccessWidth::Dword, 1)
        .unwrap();
}

fn mapd(device: u32, event_bits: u8) -> [u64; 4] {
    command(0x08, device, u32::from(event_bits - 1), 0, 1 << 63)
}

fn mapc(collection: u16, processor: u16) -> [u64; 4] {
    [
        0x09,
        0,
        u64::from(collection) | (u64::from(processor) << 16) | (1 << 63),
        0,
    ]
}

fn mapti(device: u32, event: u32, lpi: u32, collection: u16) -> [u64; 4] {
    command(
        0x0a,
        device,
        event,
        u64::from(collection),
        u64::from(lpi) << 32,
    )
}

fn command(opcode: u8, device: u32, event: u32, word2_low: u64, extra: u64) -> [u64; 4] {
    [
        u64::from(opcode) | (u64::from(device) << 32),
        u64::from(event) | extra,
        word2_low | (extra & (1 << 63)),
        0,
    ]
}

struct NoopWake;

impl GicV3VcpuWake for NoopWake {
    fn wake(&self) -> VgicResult {
        Ok(())
    }
}

#[derive(Default)]
struct TrackingBackend {
    loaded: Mutex<Option<CpuInterfaceState>>,
}

impl TrackingBackend {
    fn loaded_intids(&self) -> Vec<IntId> {
        self.loaded
            .lock()
            .unwrap()
            .as_ref()
            .into_iter()
            .flat_map(CpuInterfaceState::list_registers)
            .flatten()
            .map(|entry| entry.intid())
            .collect()
    }
}

impl GicV3Backend for TrackingBackend {
    fn load_cpu_interface(
        &self,
        _vcpu: GicVcpuId,
        state: &CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        *self.loaded.lock().unwrap() = Some(state.clone());
        Ok(())
    }

    fn save_cpu_interface(
        &self,
        _vcpu: GicVcpuId,
        state: &mut CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        let mut loaded = self.loaded.lock().unwrap();
        let hardware = loaded
            .as_mut()
            .expect("the vCPU must be loaded before save");
        for (index, (expected, actual)) in state
            .list_registers()
            .iter()
            .zip(hardware.list_registers())
            .enumerate()
        {
            if expected.is_none() && actual.is_some() {
                return Err(GicV3BackendError::value(
                    "save CPU interface",
                    "a list register became live without a saved delivery",
                    index as u64,
                ));
            }
        }
        state
            .list_registers_mut()
            .copy_from_slice(hardware.list_registers());
        hardware.list_registers_mut().fill(None);
        Ok(())
    }
}

struct TestGuestMemory {
    base: u64,
    bytes: Mutex<Vec<u8>>,
}

impl TestGuestMemory {
    fn new(base: u64, size: usize) -> Self {
        Self {
            base,
            bytes: Mutex::new(vec![0; size]),
        }
    }

    const fn base(&self) -> u64 {
        self.base
    }

    fn write_command(&self, offset: u64, words: [u64; 4]) {
        let mut bytes = self.bytes.lock().unwrap();
        let start = offset as usize;
        for (index, word) in words.into_iter().enumerate() {
            let word_start = start + index * 8;
            bytes[word_start..word_start + 8].copy_from_slice(&word.to_le_bytes());
        }
    }
}

impl GuestMemory for TestGuestMemory {
    fn read(&self, address: u64, destination: &mut [u8]) -> Result<(), GuestMemoryError> {
        let offset = address.checked_sub(self.base).ok_or_else(|| {
            GuestMemoryError::new("read", format!("address {address:#x} is below guest RAM"))
        })? as usize;
        let bytes = self.bytes.lock().unwrap();
        let source = bytes
            .get(offset..offset + destination.len())
            .ok_or_else(|| {
                GuestMemoryError::new("read", format!("address {address:#x} is outside guest RAM"))
            })?;
        destination.copy_from_slice(source);
        Ok(())
    }
}

/// Guest memory that re-enters the native controller while the ITS copies a
/// command word.
struct ReentrantGuestMemory {
    base: u64,
    bytes: Mutex<Vec<u8>>,
    probe: Mutex<Option<ReentrantProbe>>,
    probe_completed: AtomicBool,
}

struct ReentrantProbe {
    controller: Weak<GicV3Controller>,
    vcpu: GicVcpuId,
}

impl ReentrantGuestMemory {
    fn new(base: u64, size: usize) -> Self {
        Self {
            base,
            bytes: Mutex::new(vec![0; size]),
            probe: Mutex::new(None),
            probe_completed: AtomicBool::new(false),
        }
    }

    const fn base(&self) -> u64 {
        self.base
    }

    fn install_probe(&self, controller: Weak<GicV3Controller>, vcpu: GicVcpuId) {
        *self.probe.lock().unwrap() = Some(ReentrantProbe { controller, vcpu });
    }

    fn probe_completed(&self) -> bool {
        self.probe_completed.load(Ordering::Acquire)
    }

    fn write_command(&self, offset: u64, words: [u64; 4]) {
        let mut bytes = self.bytes.lock().unwrap();
        let start = offset as usize;
        for (index, word) in words.into_iter().enumerate() {
            let word_start = start + index * 8;
            bytes[word_start..word_start + 8].copy_from_slice(&word.to_le_bytes());
        }
    }
}

impl GuestMemory for ReentrantGuestMemory {
    fn read(&self, address: u64, destination: &mut [u8]) -> Result<(), GuestMemoryError> {
        let offset = address.checked_sub(self.base).ok_or_else(|| {
            GuestMemoryError::new("read", format!("address {address:#x} is below guest RAM"))
        })? as usize;
        {
            let bytes = self.bytes.lock().unwrap();
            let source = bytes
                .get(offset..offset + destination.len())
                .ok_or_else(|| {
                    GuestMemoryError::new(
                        "read",
                        format!("address {address:#x} is outside guest RAM"),
                    )
                })?;
            destination.copy_from_slice(source);
        }
        if let Some(probe) = self.probe.lock().unwrap().as_ref() {
            // The native controller state is read from a helper task while this
            // guest copy holds the sleepable ITS state. If the native raw state
            // lock were still held across the copy, the read would block until
            // the timeout below fails the test.
            let controller = probe.controller.clone();
            let vcpu = probe.vcpu;
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let result = controller
                    .upgrade()
                    .ok_or_else(|| "the tested controller was dropped".to_string())
                    .and_then(|controller| {
                        controller
                            .has_pending_interrupt(vcpu)
                            .map(|_| ())
                            .map_err(|error| format!("{error}"))
                    });
                let _ = sender.send(result);
            });
            receiver
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("native controller state must be reachable during a guest copy")
                .expect("reading native controller state during a guest copy failed");
            self.probe_completed.store(true, Ordering::Release);
        }
        Ok(())
    }
}
