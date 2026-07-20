use std::sync::{Arc, Mutex};

use arm_vgic::{
    EventId, GicAffinity, GicV3Config, GicV3Controller, GicV3MmioRegion, GicV3SpiOwnership,
    GicV3VcpuBinding, GicV3VcpuWake, GicVcpuId, GuestMemory, GuestMemoryError, IntId, ItsDeviceId,
    SoftwareGicV3Backend, VgicError, VgicResult,
};
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
