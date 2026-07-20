use core::hint::spin_loop;

use log::debug;
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields, register_structs,
    registers::{ReadOnly, ReadWrite, WriteOnly},
};

register_structs! {
    pub(crate) NvmeReg {
        (0x000 => controller_capabilities: ReadOnly<u64, CAP::Register>),
        (0x008 => version: ReadOnly<u32, VS::Register>),
        (0x00c => interrupt_mask_set: WriteOnly<u32>),
        (0x010 => interrupt_mask_clear: WriteOnly<u32>),
        (0x014 => controller_configuration: ReadWrite<u32, CC::Register>),
        (0x018 => reserved_1),
        (0x01c => controller_status: ReadWrite<u32, CSTS::Register>),
        (0x020 => nvm_subsystem_reset: WriteOnly<u32>),
        (0x024 => admin_queue_attributes: ReadWrite<u32, AQA::Register>),
        (0x028 => admin_submission_queue_base_address: ReadWrite<u64>),
        (0x030 => admin_completion_queue_base_address: ReadWrite<u64>),
        (0x038 => controller_memory_buffer_location: ReadWrite<u32>),
        (0x03c => controller_memory_buffer_size: ReadWrite<u32>),
        (0x040 => boot_partition_information: ReadWrite<u32>),
        (0x044 => boot_partition_read_select: ReadWrite<u32>),
        (0x048 => boot_partition_memory_buffer_location: ReadWrite<u64>),
        (0x050 => controller_memory_buffer_memory_space_control: ReadWrite<u64>),
        (0x058 => controller_memory_buffer_status: ReadOnly<u32>),
        (0x05c => reserved_2),
        (0xe00 => persistent_memory_capabilities: ReadOnly<u32>),
        (0xe04 => persistent_memory_region_control: WriteOnly<u32>),
        (0xe08 => persistent_memory_region_status: ReadOnly<u32>),
        (0xe0c => persistent_memory_region_elasticity_buffer_size: ReadWrite<u32>),
        (0xe10 => persistent_memory_region_sustained_write_throughput: ReadOnly<u32>),
        (0xe14 => persistent_memory_region_controller_memory_space_control_lower: ReadWrite<u32>),
        (0xe18 => persistent_memory_region_controller_memory_space_control_upper: ReadWrite<u32>),
        (0xe1c => reserved_3),
        (0x1000 => submission_queue_0_tail_doorbell: WriteOnly<u32>),
        (0x1004 => @END),
    }
}

register_bitfields! [
    // First parameter is the register width. Can be u8, u16, u32, or u64.
    u32,
    VS [
        Major OFFSET(16) NUMBITS(8) [],
        Minor OFFSET(8) NUMBITS(8) [],
        Tertiary OFFSET(0) NUMBITS(8) []
    ],
    CC [
        Enable OFFSET(0) NUMBITS(1) [],
        IOCommandSetSelected OFFSET(4) NUMBITS(3) [
            NVMCommandSet = 0,
            AdminCommandSetOnly = 0b111,
        ],
        /// MPS: This field indicates the host memory page size. The
        /// memory page size is (2 ^ (12 + MPS)). Thus, the minimum host memory page
        /// size is 4 KiB and the maximum host memory page size is 128 MiB. The value
        /// set by host software shall be a supported value as indicated by the
        /// CAP.MPSMAX and CAP.MPSMIN fields. This field describes the value used for
        /// PRP entry size. This field shall only be modified when EN is cleared to ‘0’.
        MemoryPageSize OFFSET(7) NUMBITS(4) [],
        ArbitrationMechanismSelected OFFSET(11) NUMBITS(3) [
            RoundRobin = 0,
            WeightedRoundRobinWithUrgentPriorityClass=0b001,
            VendorSpecific = 0b111
        ],
        ShutdownNotification OFFSET(14) NUMBITS(2) [
            None = 0,
            Normal = 1,
            Abrupt = 0b10,
            Reserved = 0b11
        ],
        /// This field defines the I/O
        /// Submission Queue entry size that is used for the selected I/O Command Set.
        /// The required and maximum values for this field are specified in the SQES field
        /// in the Identify Controller data structure in Figure 251 for each I/O Command
        /// Set. The value is in bytes and is specified as a power of two (2^n).
        /// If any I/O Submission Queues exist, then write operations that change the value
        /// in this field produce undefined results.
        /// If the controller does not support I/O queues, then this field shall be read-only
        /// with a value of 0h.
        IOSubmissionQueueEntrySize OFFSET(16) NUMBITS(4) [],
        IOCompletionQueueEntrySize OFFSET(20) NUMBITS(4) []
    ],
    AQA [
        AdminSubmissionQueueSize OFFSET(0) NUMBITS(12) [],
        AdminCompletionQueueSize OFFSET(16) NUMBITS(12) []
    ],
    CSTS [
        /// Processing Paused
        PP  OFFSET(5) NUMBITS(1) [],
        /// NVM Subsystem Reset Occurred
        NSSRO OFFSET(4) NUMBITS(1) [],
        /// Shutdown Status
        SHST OFFSET(2) NUMBITS(2) [],
        /// Controller Fatal Status
        CFS OFFSET(1) NUMBITS(1) [],
        /// Ready
        RDY OFFSET(0) NUMBITS(1) []
    ]
];

register_bitfields! [
    // First parameter is the register width. Can be u8, u16, u32, or u64.
    u64,

    CAP [
        // Maximum Queue Entries Supported (MQES)
        MQES OFFSET(0) NUMBITS(16) [],

        // Contiguous Queues Required (CQR)
        CQR OFFSET(16) NUMBITS(1) [],

        // Arbitration Mechanism Supported (AMS)
        AMS OFFSET(17) NUMBITS(2) [],

        // Timeout (TO)
        TO OFFSET(24) NUMBITS(8) [],

        // Doorbell Stride (DSTRD)
        DSTRD OFFSET(32) NUMBITS(4) [],

        // NVM Subsystem Reset Supported (NSSRS)
        NSSRS OFFSET(36) NUMBITS(1) [],

        // Command Sets Supported (CSS)
        CSS OFFSET(37) NUMBITS(8) [],

        // Controller Memory Buffer Supported
        CMBS OFFSET(57) NUMBITS(1) [],
    ],

];

impl NvmeReg {
    const QUEUE_BASE_MASK: u64 = !0xfff;

    pub fn version(&self) -> (usize, usize, usize) {
        let major = self.version.read(VS::Major);
        let minor = self.version.read(VS::Minor);
        let tertiary = self.version.read(VS::Tertiary);

        (major as _, minor as _, tertiary as _)
    }

    pub fn set_admin_submission_queue_base_address(&self, addr: u64) {
        let addr = addr & Self::QUEUE_BASE_MASK;
        self.admin_submission_queue_base_address.set(addr);
    }

    pub fn set_admin_completion_queue_base_address(&self, addr: u64) {
        let addr = addr & Self::QUEUE_BASE_MASK;
        self.admin_completion_queue_base_address.set(addr);
    }

    pub fn set_admin_submission_and_completion_queue_size(
        &self,
        submission_size: usize,
        completion_size: usize,
    ) {
        self.admin_queue_attributes.write(
            AQA::AdminSubmissionQueueSize.val(submission_size as u32 - 1)
                + AQA::AdminCompletionQueueSize.val(completion_size as u32 - 1),
        );
    }

    pub fn reset(&self) {
        self.controller_configuration.write(CC::Enable::CLEAR);
        debug!("Waiting for reset...");
        spin_for_true(|| !self.controller_status.is_set(CSTS::RDY));
        debug!("Reset complete!")
    }

    pub fn setup_cc(&self, sqes: u32, cqes: u32) {
        self.controller_configuration.write(
            CC::Enable::SET
                + CC::IOCommandSetSelected::NVMCommandSet
                + CC::ArbitrationMechanismSelected::RoundRobin
                + CC::ShutdownNotification::None
                + CC::IOSubmissionQueueEntrySize.val(sqes)
                + CC::IOCompletionQueueEntrySize.val(cqes),
        );
        debug!("Waiting for ready...");
        spin_for_true(|| self.controller_status.is_set(CSTS::RDY));
        debug!("Ready!");
    }

    pub fn ready_for_read_controller_info(&self) {
        self.controller_configuration.write(
            CC::Enable::SET
                + CC::IOCommandSetSelected::NVMCommandSet
                + CC::ArbitrationMechanismSelected::RoundRobin
                + CC::ShutdownNotification::None,
        );
        debug!("Waiting for ready...");
        spin_for_true(|| self.controller_status.is_set(CSTS::RDY));
        debug!("Ready!");
    }

    pub fn mask_interrupt_vector(&self, vector: u32) {
        if vector < u32::BITS {
            self.interrupt_mask_set.set(1 << vector);
        }
    }

    pub fn unmask_interrupt_vector(&self, vector: u32) {
        if vector < u32::BITS {
            self.interrupt_mask_clear.set(1 << vector);
        }
    }

    // write submission queue doorbell to notify nvme device
    pub fn write_sq_y_tail_doolbell(&self, y: usize, tail: u32) {
        let dstrd = self.controller_capabilities.read(CAP::DSTRD) as usize;
        unsafe {
            let ptr = (self as *const NvmeReg as *const u8).add(0x1000 + (2 * y * (4 << dstrd)))
                as usize as *mut u32;
            ptr.write_volatile(tail);
        }
    }

    pub fn write_cq_y_head_doolbell(&self, y: usize, head: u32) {
        let dstrd = self.controller_capabilities.read(CAP::DSTRD) as usize;
        unsafe {
            let ptr = (self as *const NvmeReg as *const u8)
                .add(0x1000 + ((2 * y + 1) * (4 << dstrd))) as usize
                as *mut u32;
            ptr.write_volatile(head);
        }
    }
}

fn spin_for_true<F>(f: F)
where
    F: Fn() -> bool,
{
    while !f() {
        spin_loop();
    }
}
