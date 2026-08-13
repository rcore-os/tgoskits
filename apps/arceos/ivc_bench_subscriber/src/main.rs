#![cfg_attr(feature = "arceos", no_main)]
#![cfg_attr(feature = "arceos", no_std)]

#[cfg(feature = "arceos")]
use ax_std as _;

#[cfg_attr(feature = "arceos", unsafe(no_mangle))]
#[cfg(feature = "arceos")]
fn main() {
    bench_subscriber::run();
}

#[cfg(any(feature = "arceos", test))]
mod bench_config {
    pub const CHANNEL_KEY: usize = 0x4956_4302;
    pub const TEST_TIMES: u32 = 100;
    pub const DATA_SIZES: [usize; 4] = [256 * 1024, 512 * 1024, 1024 * 1024, 10 * 1024 * 1024];
    pub const PUBLISHER_VM_ID: usize = 1;
}

#[cfg(not(feature = "arceos"))]
fn main() {}

#[cfg(feature = "arceos")]
mod bench_subscriber {
    use core::{cell::UnsafeCell, result::Result::Err};

    use ax_std::{
        os::arceos::modules::ax_hal::mem::{PhysAddr, VirtAddr, virt_to_phys},
        println,
        time::Instant,
    };
    use axhvc::ivc::{self, IvcGuestPhysAddr};
    use axivc::{
        IVC_DEFAULT_FALLBACK_POLL_ROUNDS, IVC_SLOT_PAYLOAD_SIZE, IvcConsumer, IvcMessageKind,
        IvcProducer, IvcRegion, fallback_poll,
    };

    use crate::bench_config;

    const MAX_SUBSCRIBE_ATTEMPTS: usize = 200_000;
    const MAX_PROTOCOL_HEADER_ATTEMPTS: usize = 200_000;
    const SHMEM_FLAG_OFFSET: usize = 0;
    const SHMEM_LEN_OFFSET: usize = 1;
    const SHMEM_PAYLOAD_OFFSET: usize = SHMEM_LEN_OFFSET + core::mem::size_of::<u32>();
    const SHMEM_INVALID: u8 = 0;
    const SHMEM_VALID: u8 = 1;

    pub fn run() {
        let Some((shm_base_gpa, shm_size)) = subscribe_with_retry() else {
            println!("AXVISOR_IVC_BENCH_RESULT=FAIL reason=subscribe-timeout");
            return;
        };

        if shm_size < core::mem::size_of::<IvcRegion>() {
            println!(
                "AXVISOR_IVC_BENCH_RESULT=FAIL reason=shared-page-too-small size={} need={}",
                shm_size,
                core::mem::size_of::<IvcRegion>()
            );
            return;
        }

        println!("ivc bench subscribe ok base={shm_base_gpa:#x} size={shm_size}");
        let Some((region, data_window)) = shared_mapping(shm_base_gpa, shm_size) else {
            println!("AXVISOR_IVC_BENCH_RESULT=FAIL reason=map-shared-page base={shm_base_gpa:#x}");
            return;
        };
        if !wait_for_protocol_header(region) {
            println!("AXVISOR_IVC_BENCH_RESULT=FAIL reason=protocol-header-timeout");
            return;
        }

        let region: &'static IvcRegion = region;
        // SAFETY: this benchmark is the only subscriber endpoint for this VM.
        let (producer, consumer) = unsafe { region.subscriber_endpoints() }.into_parts();
        run_receive_bench(producer, consumer, data_window);
    }

    fn subscribe_with_retry() -> Option<(usize, usize)> {
        for attempt in 1..=MAX_SUBSCRIBE_ATTEMPTS {
            let shm_base_gpa = HyperCallOutputSlot::new(0);
            let shm_size = HyperCallOutputSlot::new(0);

            match ivc::subscribe_channel(
                bench_config::PUBLISHER_VM_ID,
                bench_config::CHANNEL_KEY,
                shm_base_gpa.guest_phys_addr(),
                shm_size.guest_phys_addr(),
            ) {
                Ok(()) => return Some((shm_base_gpa.read(), shm_size.read())),
                Err(err) => {
                    if attempt == 1 || attempt % 20_000 == 0 {
                        println!("ivc bench subscribe retry attempt={attempt} err={err}");
                    }
                    fallback_poll(IVC_DEFAULT_FALLBACK_POLL_ROUNDS);
                }
            }
        }
        None
    }

    fn wait_for_protocol_header(region: &IvcRegion) -> bool {
        for _ in 0..MAX_PROTOCOL_HEADER_ATTEMPTS {
            if region.protocol_header_matches() {
                return true;
            }
            fallback_poll(IVC_DEFAULT_FALLBACK_POLL_ROUNDS);
        }
        false
    }

    fn run_receive_bench(
        mut producer: IvcProducer<'_>,
        mut consumer: IvcConsumer<'_>,
        data_window: &'static [u8],
    ) {
        let mut payload = [0u8; IVC_SLOT_PAYLOAD_SIZE];
        let mut case_bytes = 0u64;
        let mut total_bytes = 0u64;
        let mut chunks = 0u64;
        let mut completed_cases = 0usize;
        let mut receive_bandwidth_x100_sum = 0u128;

        println!("[test_output] =========================================");
        while completed_cases < bench_config::DATA_SIZES.len() {
            match consumer.try_recv(&mut payload) {
                Ok(Some(msg)) => {
                    if msg.kind() != IvcMessageKind::Request {
                        println!(
                            "AXVISOR_IVC_BENCH_RESULT=FAIL reason=unexpected-message-kind seq={}",
                            msg.sequence()
                        );
                        return;
                    }

                    let Some(command) = BenchMessage::decode(&payload) else {
                        println!(
                            "AXVISOR_IVC_BENCH_RESULT=FAIL reason=bad-message seq={}",
                            msg.sequence()
                        );
                        return;
                    };

                    match command.msg_type {
                        BenchMessage::DATA => {
                            if case_bytes == 0 {
                                case_bytes = 0;
                                receive_bandwidth_x100_sum = 0;
                            }

                            let len = command.len as usize;
                            if len + SHMEM_PAYLOAD_OFFSET > data_window.len() {
                                println!(
                                    "AXVISOR_IVC_BENCH_RESULT=FAIL reason=data-len case={} \
                                     iteration={} offset={} len={}",
                                    command.case_index, command.iteration, command.offset, len
                                );
                                return;
                            }

                            let read_started = Instant::now();
                            let Some((actual_len, checksum)) =
                                receive_payload(&data_window[..len + SHMEM_PAYLOAD_OFFSET])
                            else {
                                println!(
                                    "AXVISOR_IVC_BENCH_RESULT=FAIL reason=data-invalid case={} \
                                     iteration={} offset={} len={}",
                                    command.case_index, command.iteration, command.offset, len
                                );
                                return;
                            };
                            receive_bandwidth_x100_sum +=
                                throughput_mb_x100(actual_len as u64, elapsed_micros(read_started));
                            if actual_len != len || checksum != command.value {
                                println!(
                                    "AXVISOR_IVC_BENCH_RESULT=FAIL reason=data-checksum case={} \
                                     iteration={} offset={} len={}",
                                    command.case_index, command.iteration, command.offset, len
                                );
                                return;
                            }

                            case_bytes += len as u64;
                            total_bytes += len as u64;
                            chunks += 1;
                            send_ack(&mut producer, msg.sequence());
                        }

                        BenchMessage::STATS => {
                            let case_index = command.case_index as usize;
                            if case_index >= bench_config::DATA_SIZES.len() {
                                println!(
                                    "AXVISOR_IVC_BENCH_RESULT=FAIL reason=bad-case-index case={}",
                                    command.case_index
                                );
                                return;
                            }

                            let expected_bytes = bench_config::DATA_SIZES[case_index] as u64
                                * bench_config::TEST_TIMES as u64;
                            if case_bytes != expected_bytes {
                                println!(
                                    "AXVISOR_IVC_BENCH_RESULT=FAIL reason=bad-case-bytes case={} \
                                     bytes={} expected={}",
                                    case_index, case_bytes, expected_bytes
                                );
                                return;
                            }

                            let receive_mb_x100 =
                                receive_bandwidth_x100_sum / bench_config::TEST_TIMES as u128;
                            println!(
                                "[test_output] average sendBandwidth = {}.{:02} MB/s, average \
                                 receiveBandwidth = {}.{:02} MB/s, testTime = {}, datasize = {}",
                                command.value / 100,
                                command.value % 100,
                                receive_mb_x100 / 100,
                                receive_mb_x100 % 100,
                                bench_config::TEST_TIMES,
                                bench_config::DATA_SIZES[case_index]
                            );
                            println!("[test_output] =========================================");
                            completed_cases += 1;
                            case_bytes = 0;
                            receive_bandwidth_x100_sum = 0;
                        }

                        _ => {
                            println!(
                                "AXVISOR_IVC_BENCH_RESULT=FAIL reason=unknown-command type={}",
                                command.msg_type
                            );
                            return;
                        }
                    }
                }
                Ok(None) => fallback_poll(IVC_DEFAULT_FALLBACK_POLL_ROUNDS),
                Err(err) => {
                    println!("AXVISOR_IVC_BENCH_RESULT=FAIL reason=recv-error err={err:?}");
                    return;
                }
            }
        }

        println!(
            "AXVISOR_IVC_BENCH_RESULT=PASS cases={} testTime={} bytes={} chunks={}",
            completed_cases,
            bench_config::TEST_TIMES,
            total_bytes,
            chunks
        );
    }

    fn send_ack(producer: &mut IvcProducer<'_>, sequence: u64) {
        let payload = [0u8; IVC_SLOT_PAYLOAD_SIZE];
        loop {
            match producer.send(IvcMessageKind::Ack, sequence, &payload) {
                Ok(()) => break,
                Err(_) => fallback_poll(IVC_DEFAULT_FALLBACK_POLL_ROUNDS),
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

    fn receive_payload(buf: &[u8]) -> Option<(usize, u64)> {
        unsafe {
            if core::ptr::read_volatile(buf.as_ptr().add(SHMEM_FLAG_OFFSET)) != SHMEM_VALID {
                return None;
            }

            let len = core::ptr::read_unaligned(buf.as_ptr().add(SHMEM_LEN_OFFSET).cast::<u32>())
                as usize;
            if len + SHMEM_PAYLOAD_OFFSET > buf.len() {
                return None;
            }

            let payload = buf.as_ptr().add(SHMEM_PAYLOAD_OFFSET);
            let words = len / core::mem::size_of::<u32>();
            let word_ptr = payload.cast::<u32>();
            let mut checksum = 0u64;
            let mut index = 0;
            while index + 128 <= words {
                checksum = checksum.wrapping_add(sum_u32_128(word_ptr.add(index)));
                index += 128;
            }
            while index < words {
                checksum =
                    checksum.wrapping_add(core::ptr::read_unaligned(word_ptr.add(index)) as u64);
                index += 1;
            }

            let consumed = words * core::mem::size_of::<u32>();
            for index in consumed..len {
                checksum = checksum.wrapping_add(core::ptr::read(payload.add(index)) as u64);
            }
            core::ptr::write_volatile(
                buf.as_ptr().add(SHMEM_FLAG_OFFSET).cast_mut(),
                SHMEM_INVALID,
            );
            Some((len, checksum))
        }
    }

    #[allow(unsafe_op_in_unsafe_fn)]
    unsafe fn sum_u32_128(source: *const u32) -> u64 {
        macro_rules! read_one {
            ($index:expr) => {
                core::ptr::read_unaligned(source.add($index)) as u64
            };
        }
        0u64.wrapping_add(read_one!(0))
            .wrapping_add(read_one!(1))
            .wrapping_add(read_one!(2))
            .wrapping_add(read_one!(3))
            .wrapping_add(read_one!(4))
            .wrapping_add(read_one!(5))
            .wrapping_add(read_one!(6))
            .wrapping_add(read_one!(7))
            .wrapping_add(read_one!(8))
            .wrapping_add(read_one!(9))
            .wrapping_add(read_one!(10))
            .wrapping_add(read_one!(11))
            .wrapping_add(read_one!(12))
            .wrapping_add(read_one!(13))
            .wrapping_add(read_one!(14))
            .wrapping_add(read_one!(15))
            .wrapping_add(read_one!(16))
            .wrapping_add(read_one!(17))
            .wrapping_add(read_one!(18))
            .wrapping_add(read_one!(19))
            .wrapping_add(read_one!(20))
            .wrapping_add(read_one!(21))
            .wrapping_add(read_one!(22))
            .wrapping_add(read_one!(23))
            .wrapping_add(read_one!(24))
            .wrapping_add(read_one!(25))
            .wrapping_add(read_one!(26))
            .wrapping_add(read_one!(27))
            .wrapping_add(read_one!(28))
            .wrapping_add(read_one!(29))
            .wrapping_add(read_one!(30))
            .wrapping_add(read_one!(31))
            .wrapping_add(read_one!(32))
            .wrapping_add(read_one!(33))
            .wrapping_add(read_one!(34))
            .wrapping_add(read_one!(35))
            .wrapping_add(read_one!(36))
            .wrapping_add(read_one!(37))
            .wrapping_add(read_one!(38))
            .wrapping_add(read_one!(39))
            .wrapping_add(read_one!(40))
            .wrapping_add(read_one!(41))
            .wrapping_add(read_one!(42))
            .wrapping_add(read_one!(43))
            .wrapping_add(read_one!(44))
            .wrapping_add(read_one!(45))
            .wrapping_add(read_one!(46))
            .wrapping_add(read_one!(47))
            .wrapping_add(read_one!(48))
            .wrapping_add(read_one!(49))
            .wrapping_add(read_one!(50))
            .wrapping_add(read_one!(51))
            .wrapping_add(read_one!(52))
            .wrapping_add(read_one!(53))
            .wrapping_add(read_one!(54))
            .wrapping_add(read_one!(55))
            .wrapping_add(read_one!(56))
            .wrapping_add(read_one!(57))
            .wrapping_add(read_one!(58))
            .wrapping_add(read_one!(59))
            .wrapping_add(read_one!(60))
            .wrapping_add(read_one!(61))
            .wrapping_add(read_one!(62))
            .wrapping_add(read_one!(63))
            .wrapping_add(read_one!(64))
            .wrapping_add(read_one!(65))
            .wrapping_add(read_one!(66))
            .wrapping_add(read_one!(67))
            .wrapping_add(read_one!(68))
            .wrapping_add(read_one!(69))
            .wrapping_add(read_one!(70))
            .wrapping_add(read_one!(71))
            .wrapping_add(read_one!(72))
            .wrapping_add(read_one!(73))
            .wrapping_add(read_one!(74))
            .wrapping_add(read_one!(75))
            .wrapping_add(read_one!(76))
            .wrapping_add(read_one!(77))
            .wrapping_add(read_one!(78))
            .wrapping_add(read_one!(79))
            .wrapping_add(read_one!(80))
            .wrapping_add(read_one!(81))
            .wrapping_add(read_one!(82))
            .wrapping_add(read_one!(83))
            .wrapping_add(read_one!(84))
            .wrapping_add(read_one!(85))
            .wrapping_add(read_one!(86))
            .wrapping_add(read_one!(87))
            .wrapping_add(read_one!(88))
            .wrapping_add(read_one!(89))
            .wrapping_add(read_one!(90))
            .wrapping_add(read_one!(91))
            .wrapping_add(read_one!(92))
            .wrapping_add(read_one!(93))
            .wrapping_add(read_one!(94))
            .wrapping_add(read_one!(95))
            .wrapping_add(read_one!(96))
            .wrapping_add(read_one!(97))
            .wrapping_add(read_one!(98))
            .wrapping_add(read_one!(99))
            .wrapping_add(read_one!(100))
            .wrapping_add(read_one!(101))
            .wrapping_add(read_one!(102))
            .wrapping_add(read_one!(103))
            .wrapping_add(read_one!(104))
            .wrapping_add(read_one!(105))
            .wrapping_add(read_one!(106))
            .wrapping_add(read_one!(107))
            .wrapping_add(read_one!(108))
            .wrapping_add(read_one!(109))
            .wrapping_add(read_one!(110))
            .wrapping_add(read_one!(111))
            .wrapping_add(read_one!(112))
            .wrapping_add(read_one!(113))
            .wrapping_add(read_one!(114))
            .wrapping_add(read_one!(115))
            .wrapping_add(read_one!(116))
            .wrapping_add(read_one!(117))
            .wrapping_add(read_one!(118))
            .wrapping_add(read_one!(119))
            .wrapping_add(read_one!(120))
            .wrapping_add(read_one!(121))
            .wrapping_add(read_one!(122))
            .wrapping_add(read_one!(123))
            .wrapping_add(read_one!(124))
            .wrapping_add(read_one!(125))
            .wrapping_add(read_one!(126))
            .wrapping_add(read_one!(127))
    }

    struct BenchMessage {
        msg_type: u32,
        case_index: u32,
        iteration: u32,
        offset: u64,
        len: u32,
        value: u64,
    }

    impl BenchMessage {
        const MAGIC: u32 = 0x4942_454e;
        const DATA: u32 = 1;
        const STATS: u32 = 2;

        fn decode(payload: &[u8; IVC_SLOT_PAYLOAD_SIZE]) -> Option<Self> {
            if read_u32(payload, 0) != Self::MAGIC {
                return None;
            }
            Some(Self {
                msg_type: read_u32(payload, 4),
                case_index: read_u32(payload, 8),
                iteration: read_u32(payload, 12),
                offset: read_u64(payload, 16),
                len: read_u32(payload, 24),
                value: read_u64(payload, 40),
            })
        }
    }

    fn read_u32(payload: &[u8; IVC_SLOT_PAYLOAD_SIZE], offset: usize) -> u32 {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&payload[offset..offset + 4]);
        u32::from_le_bytes(bytes)
    }

    fn read_u64(payload: &[u8; IVC_SLOT_PAYLOAD_SIZE], offset: usize) -> u64 {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&payload[offset..offset + 8]);
        u64::from_le_bytes(bytes)
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

    fn shared_mapping(
        shm_base_gpa: usize,
        shm_size: usize,
    ) -> Option<(&'static IvcRegion, &'static [u8])> {
        let vaddr = ax_mm::iomap_cacheable(PhysAddr::from_usize(shm_base_gpa), shm_size).ok()?;
        if shm_size <= core::mem::size_of::<IvcRegion>() {
            return None;
        }
        unsafe {
            let base = vaddr.as_ptr();
            let region = &*(base as *const IvcRegion);
            let data_offset = core::mem::size_of::<IvcRegion>();
            let data_len = shm_size - data_offset;
            let data_window = core::slice::from_raw_parts(base.add(data_offset), data_len);
            Some((region, data_window))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_ivc_bench_matches_publisher_config() {
        assert_eq!(bench_config::CHANNEL_KEY, 0x4956_4302);
        assert_eq!(bench_config::TEST_TIMES, 100);
        assert_eq!(
            bench_config::DATA_SIZES,
            [256 * 1024, 512 * 1024, 1024 * 1024, 10 * 1024 * 1024]
        );
        assert_eq!(bench_config::PUBLISHER_VM_ID, 1);
    }
}
