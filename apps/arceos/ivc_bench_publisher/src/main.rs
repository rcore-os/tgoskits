#![cfg_attr(feature = "arceos", no_main)]
#![cfg_attr(feature = "arceos", no_std)]

#[cfg(feature = "arceos")]
use ax_std as _;

#[cfg_attr(feature = "arceos", unsafe(no_mangle))]
#[cfg(feature = "arceos")]
fn main() {
    bench_publisher::run();
}

#[cfg(any(feature = "arceos", test))]
mod bench_config {
    pub const CHANNEL_KEY: usize = 0x4956_4302;
    pub const CHANNEL_SIZE: usize = 0x100_0000;
    pub const TEST_TIMES: u32 = 100;
    pub const DATA_SIZES: [usize; 4] = [256 * 1024, 512 * 1024, 1024 * 1024, 10 * 1024 * 1024];
    pub const MAX_DATA_SIZE: usize = 10 * 1024 * 1024;
}

#[cfg(not(feature = "arceos"))]
fn main() {}

#[cfg(feature = "arceos")]
mod bench_publisher {
    use core::{cell::UnsafeCell, result::Result::Err};

    use ax_std::{
        os::arceos::modules::ax_hal::mem::{PhysAddr, VirtAddr, virt_to_phys},
        println,
        time::Instant,
    };
    use axhvc::ivc::{self, IvcGuestPhysAddr};
    use axivc::{
        IVC_DEFAULT_FALLBACK_POLL_ROUNDS, IVC_SLOT_PAYLOAD_SIZE, IvcMessageKind, IvcProducer,
        IvcRegion, fallback_poll,
    };

    use crate::bench_config;

    const SHMEM_FLAG_OFFSET: usize = 0;
    const SHMEM_LEN_OFFSET: usize = 1;
    const SHMEM_PAYLOAD_OFFSET: usize = SHMEM_LEN_OFFSET + core::mem::size_of::<u32>();
    const SHMEM_INVALID: u8 = 0;
    const SHMEM_VALID: u8 = 1;

    pub fn run() {
        let source_data = random_source_data();
        let shm_base_gpa = HyperCallOutputSlot::new(0);
        let shm_size = HyperCallOutputSlot::new(bench_config::CHANNEL_SIZE);

        if let Err(err) = ivc::publish_channel(
            bench_config::CHANNEL_KEY,
            shm_base_gpa.guest_phys_addr(),
            shm_size.guest_phys_addr(),
        ) {
            println!("ivc bench publish failed: {err}");
            return;
        }

        let shm_base_gpa = shm_base_gpa.read();
        let shm_size = shm_size.read();
        if shm_size < core::mem::size_of::<IvcRegion>() {
            println!(
                "ivc bench publish failed: shared page too small size={} need={}",
                shm_size,
                core::mem::size_of::<IvcRegion>()
            );
            return;
        }

        println!("ivc bench publish ok base={shm_base_gpa:#x} size={shm_size}");
        let Some((region, data_window)) = shared_mapping_mut(shm_base_gpa, shm_size) else {
            println!("ivc bench publish failed: map shared page base={shm_base_gpa:#x}");
            return;
        };
        region.initialize();
        initialize_payload_window(data_window);
        let region: &'static IvcRegion = region;
        // SAFETY: this benchmark is the only publisher endpoint for this VM.
        let (producer, consumer) = unsafe { region.publisher_endpoints() }.into_parts();
        run_send_bench(producer, consumer, data_window, source_data);
    }

    fn run_send_bench(
        mut producer: IvcProducer<'_>,
        mut consumer: axivc::IvcConsumer<'_>,
        data_window: &'static mut [u8],
        source_data: &'static [u8; bench_config::MAX_DATA_SIZE],
    ) {
        println!(
            "[test_output] IVC random block bench created, testTime = {}, windowSize = {}",
            bench_config::TEST_TIMES,
            data_window.len()
        );

        let mut sequence = 1u64;
        let mut ack_payload = [0u8; IVC_SLOT_PAYLOAD_SIZE];
        for (case_index, data_size) in bench_config::DATA_SIZES.iter().copied().enumerate() {
            if data_size + SHMEM_PAYLOAD_OFFSET > data_window.len() {
                println!(
                    "ivc bench publish failed: datasize {} is larger than shared window {}",
                    data_size,
                    data_window.len()
                );
                return;
            }

            let total_bytes = data_size as u64 * bench_config::TEST_TIMES as u64;
            let expected_checksum = checksum(&source_data[..data_size]);
            let mut send_bandwidth_x100_sum = 0u128;

            for iteration in 1..=bench_config::TEST_TIMES {
                let copy_started = Instant::now();
                send_payload(
                    &source_data[..data_size],
                    &mut data_window[..data_size + SHMEM_PAYLOAD_OFFSET],
                );
                send_bandwidth_x100_sum +=
                    throughput_mb_x100(data_size as u64, elapsed_micros(copy_started));
                let payload = BenchMessage::data(
                    case_index as u32,
                    iteration,
                    0,
                    data_size as u32,
                    data_size as u64,
                    expected_checksum,
                )
                .encode();
                send_message(&mut producer, sequence, &payload);
                if !wait_for_ack(&mut consumer, &mut ack_payload, sequence) {
                    return;
                }
                sequence += 1;
            }

            let average_mbps_x100 = send_bandwidth_x100_sum / bench_config::TEST_TIMES as u128;
            let payload =
                BenchMessage::stats(case_index as u32, 0, average_mbps_x100 as u64).encode();
            send_message(&mut producer, sequence, &payload);
            println!(
                "ivc bench publish case={} datasize={} testTime={} bytes={} throughput_mb_x100={}",
                case_index,
                data_size,
                bench_config::TEST_TIMES,
                total_bytes,
                average_mbps_x100
            );
            sequence += 1;
        }
        fallback_poll(IVC_DEFAULT_FALLBACK_POLL_ROUNDS * 1_000);
    }

    fn initialize_payload_window(data_window: &mut [u8]) {
        unsafe {
            core::ptr::write_volatile(
                data_window.as_mut_ptr().add(SHMEM_FLAG_OFFSET),
                SHMEM_INVALID,
            );
        }
    }

    fn send_message(producer: &mut IvcProducer<'_>, sequence: u64, payload: &[u8]) {
        loop {
            match producer.send(IvcMessageKind::Request, sequence, payload) {
                Ok(()) => break,
                Err(_) => fallback_poll(IVC_DEFAULT_FALLBACK_POLL_ROUNDS),
            }
        }
    }

    fn wait_for_ack(
        consumer: &mut axivc::IvcConsumer<'_>,
        payload: &mut [u8; IVC_SLOT_PAYLOAD_SIZE],
        expected_sequence: u64,
    ) -> bool {
        loop {
            match consumer.try_recv(payload) {
                Ok(Some(msg))
                    if msg.kind() == IvcMessageKind::Ack && msg.sequence() == expected_sequence =>
                {
                    return true;
                }
                Ok(Some(msg)) => {
                    println!(
                        "ivc bench publish failed: unexpected ack kind={:?} seq={} expected={}",
                        msg.kind(),
                        msg.sequence(),
                        expected_sequence
                    );
                    return false;
                }
                Ok(None) => fallback_poll(IVC_DEFAULT_FALLBACK_POLL_ROUNDS),
                Err(err) => {
                    println!("ivc bench publish failed: ack recv error err={err:?}");
                    return false;
                }
            }
        }
    }

    fn elapsed_micros(started: Instant) -> u128 {
        let elapsed = started.elapsed().as_micros();
        elapsed.max(1)
    }

    fn throughput_mb_x100(bytes: u64, elapsed_us: u128) -> u128 {
        (bytes as u128 * 100 * 1_000_000) / (1024 * 1024) / elapsed_us
    }

    fn random_source_data() -> &'static [u8; bench_config::MAX_DATA_SIZE] {
        let source_data = source_data_mut();
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for byte in source_data.iter_mut() {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            *byte = (state >> 32) as u8;
        }
        source_data
    }

    fn send_payload(source: &[u8], target: &mut [u8]) {
        unsafe {
            while core::ptr::read_volatile(target.as_ptr().add(SHMEM_FLAG_OFFSET)) != SHMEM_INVALID
            {
                core::hint::spin_loop();
            }

            let len = source.len() as u32;
            core::ptr::write_unaligned(
                target.as_mut_ptr().add(SHMEM_LEN_OFFSET).cast::<u32>(),
                len,
            );

            let payload = target.as_mut_ptr().add(SHMEM_PAYLOAD_OFFSET);
            let words = source.len() / core::mem::size_of::<u32>();
            let source_words = source.as_ptr().cast::<u32>();
            let target_words = payload.cast::<u32>();
            let mut index = 0;
            while index + 128 <= words {
                copy_u32_128(source_words.add(index), target_words.add(index));
                index += 128;
            }
            while index < words {
                core::ptr::write_unaligned(
                    target_words.add(index),
                    core::ptr::read_unaligned(source_words.add(index)),
                );
                index += 1;
            }

            let copied = words * core::mem::size_of::<u32>();
            for index in copied..source.len() {
                core::ptr::write(
                    payload.add(index),
                    core::ptr::read(source.as_ptr().add(index)),
                );
            }
            core::ptr::write_volatile(target.as_mut_ptr().add(SHMEM_FLAG_OFFSET), SHMEM_VALID);
        }
    }

    fn checksum(buf: &[u8]) -> u64 {
        let mut checksum = 0u64;
        let words = buf.len() / core::mem::size_of::<u32>();
        let word_ptr = buf.as_ptr().cast::<u32>();
        for index in 0..words {
            checksum = checksum
                .wrapping_add(unsafe { core::ptr::read_unaligned(word_ptr.add(index)) } as u64);
        }

        let consumed = words * core::mem::size_of::<u32>();
        for byte in &buf[consumed..] {
            checksum = checksum.wrapping_add(*byte as u64);
        }
        checksum
    }

    #[allow(unsafe_op_in_unsafe_fn)]
    unsafe fn copy_u32_128(source: *const u32, target: *mut u32) {
        macro_rules! copy_one {
            ($index:expr) => {
                core::ptr::write_unaligned(
                    target.add($index),
                    core::ptr::read_unaligned(source.add($index)),
                )
            };
        }
        copy_one!(0);
        copy_one!(1);
        copy_one!(2);
        copy_one!(3);
        copy_one!(4);
        copy_one!(5);
        copy_one!(6);
        copy_one!(7);
        copy_one!(8);
        copy_one!(9);
        copy_one!(10);
        copy_one!(11);
        copy_one!(12);
        copy_one!(13);
        copy_one!(14);
        copy_one!(15);
        copy_one!(16);
        copy_one!(17);
        copy_one!(18);
        copy_one!(19);
        copy_one!(20);
        copy_one!(21);
        copy_one!(22);
        copy_one!(23);
        copy_one!(24);
        copy_one!(25);
        copy_one!(26);
        copy_one!(27);
        copy_one!(28);
        copy_one!(29);
        copy_one!(30);
        copy_one!(31);
        copy_one!(32);
        copy_one!(33);
        copy_one!(34);
        copy_one!(35);
        copy_one!(36);
        copy_one!(37);
        copy_one!(38);
        copy_one!(39);
        copy_one!(40);
        copy_one!(41);
        copy_one!(42);
        copy_one!(43);
        copy_one!(44);
        copy_one!(45);
        copy_one!(46);
        copy_one!(47);
        copy_one!(48);
        copy_one!(49);
        copy_one!(50);
        copy_one!(51);
        copy_one!(52);
        copy_one!(53);
        copy_one!(54);
        copy_one!(55);
        copy_one!(56);
        copy_one!(57);
        copy_one!(58);
        copy_one!(59);
        copy_one!(60);
        copy_one!(61);
        copy_one!(62);
        copy_one!(63);
        copy_one!(64);
        copy_one!(65);
        copy_one!(66);
        copy_one!(67);
        copy_one!(68);
        copy_one!(69);
        copy_one!(70);
        copy_one!(71);
        copy_one!(72);
        copy_one!(73);
        copy_one!(74);
        copy_one!(75);
        copy_one!(76);
        copy_one!(77);
        copy_one!(78);
        copy_one!(79);
        copy_one!(80);
        copy_one!(81);
        copy_one!(82);
        copy_one!(83);
        copy_one!(84);
        copy_one!(85);
        copy_one!(86);
        copy_one!(87);
        copy_one!(88);
        copy_one!(89);
        copy_one!(90);
        copy_one!(91);
        copy_one!(92);
        copy_one!(93);
        copy_one!(94);
        copy_one!(95);
        copy_one!(96);
        copy_one!(97);
        copy_one!(98);
        copy_one!(99);
        copy_one!(100);
        copy_one!(101);
        copy_one!(102);
        copy_one!(103);
        copy_one!(104);
        copy_one!(105);
        copy_one!(106);
        copy_one!(107);
        copy_one!(108);
        copy_one!(109);
        copy_one!(110);
        copy_one!(111);
        copy_one!(112);
        copy_one!(113);
        copy_one!(114);
        copy_one!(115);
        copy_one!(116);
        copy_one!(117);
        copy_one!(118);
        copy_one!(119);
        copy_one!(120);
        copy_one!(121);
        copy_one!(122);
        copy_one!(123);
        copy_one!(124);
        copy_one!(125);
        copy_one!(126);
        copy_one!(127);
    }

    struct SourceData(UnsafeCell<[u8; bench_config::MAX_DATA_SIZE]>);

    unsafe impl Sync for SourceData {}

    static SOURCE_DATA: SourceData = SourceData(UnsafeCell::new([0; bench_config::MAX_DATA_SIZE]));

    fn source_data_mut() -> &'static mut [u8; bench_config::MAX_DATA_SIZE] {
        unsafe { &mut *SOURCE_DATA.0.get() }
    }

    struct BenchMessage {
        msg_type: u32,
        case_index: u32,
        iteration: u32,
        offset: u64,
        len: u32,
        total_size: u64,
        value: u64,
    }

    impl BenchMessage {
        const MAGIC: u32 = 0x4942_454e;
        const DATA: u32 = 1;
        const STATS: u32 = 2;

        const fn data(
            case_index: u32,
            iteration: u32,
            offset: u64,
            len: u32,
            total_size: u64,
            checksum: u64,
        ) -> Self {
            Self {
                msg_type: Self::DATA,
                case_index,
                iteration,
                offset,
                len,
                total_size,
                value: checksum,
            }
        }

        const fn stats(case_index: u32, elapsed_us: u64, throughput_mib_x100: u64) -> Self {
            Self {
                msg_type: Self::STATS,
                case_index,
                iteration: 0,
                offset: 0,
                len: 0,
                total_size: elapsed_us,
                value: throughput_mib_x100,
            }
        }

        fn encode(self) -> [u8; IVC_SLOT_PAYLOAD_SIZE] {
            let mut payload = [0u8; IVC_SLOT_PAYLOAD_SIZE];
            payload[0..4].copy_from_slice(&Self::MAGIC.to_le_bytes());
            payload[4..8].copy_from_slice(&self.msg_type.to_le_bytes());
            payload[8..12].copy_from_slice(&self.case_index.to_le_bytes());
            payload[12..16].copy_from_slice(&self.iteration.to_le_bytes());
            payload[16..24].copy_from_slice(&self.offset.to_le_bytes());
            payload[24..28].copy_from_slice(&self.len.to_le_bytes());
            payload[32..40].copy_from_slice(&self.total_size.to_le_bytes());
            payload[40..48].copy_from_slice(&self.value.to_le_bytes());
            payload
        }
    }

    struct HyperCallOutputSlot {
        value: UnsafeCell<usize>,
    }

    impl HyperCallOutputSlot {
        const fn new(value: usize) -> Self {
            Self {
                value: UnsafeCell::new(value),
            }
        }

        fn guest_phys_addr(&self) -> IvcGuestPhysAddr {
            let vaddr = VirtAddr::from_usize(self.value.get().addr());
            IvcGuestPhysAddr::new(virt_to_phys(vaddr).as_usize())
        }

        fn read(&self) -> usize {
            unsafe { core::ptr::read_volatile(self.value.get()) }
        }
    }

    fn shared_mapping_mut(
        shm_base_gpa: usize,
        shm_size: usize,
    ) -> Option<(&'static mut IvcRegion, &'static mut [u8])> {
        let vaddr = ax_mm::iomap_cacheable(PhysAddr::from_usize(shm_base_gpa), shm_size).ok()?;
        if shm_size <= core::mem::size_of::<IvcRegion>() {
            return None;
        }
        unsafe {
            let base = vaddr.as_mut_ptr();
            let region = &mut *(base as *mut IvcRegion);
            let data_offset = core::mem::size_of::<IvcRegion>();
            let data_len = shm_size - data_offset;
            let data_window = core::slice::from_raw_parts_mut(base.add(data_offset), data_len);
            Some((region, data_window))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_ivc_bench_uses_dedicated_channel() {
        assert_eq!(bench_config::CHANNEL_KEY, 0x4956_4302);
        assert_eq!(bench_config::CHANNEL_SIZE, 0x100_0000);
        assert_eq!(bench_config::TEST_TIMES, 100);
        assert_eq!(
            bench_config::DATA_SIZES,
            [256 * 1024, 512 * 1024, 1024 * 1024, 10 * 1024 * 1024]
        );
    }
}
