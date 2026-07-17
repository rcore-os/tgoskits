use core::sync::atomic::{AtomicU8, Ordering};

use ax_cpu_local::{
    CPU_AREA_BOOT_THREAD_OFFSET, CPU_LOCAL_ABI_VERSION, CpuBindingResultV1, CpuBindingV1, CpuIndex,
    CpuLocalPlatformV1, CpuLocalStatus, HostLevelV1, RegisterModeV1, image_register_mode,
};

const VALID: u8 = 0;
const MALFORMED_GEOMETRY: u8 = 1;
const WRONG_IMAGE_MODE: u8 = 2;

static RESPONSE: AtomicU8 = AtomicU8::new(VALID);

struct TestPlatform;

#[ax_cpu_local::abi::impl_extern_trait(name = "ax-cpu-local_0_1", abi = "rust")]
impl CpuLocalPlatformV1 for TestPlatform {
    fn current_cpu_binding() -> CpuBindingResultV1 {
        let area_base = match RESPONSE.load(Ordering::Relaxed) {
            MALFORMED_GEOMETRY => 0x1001,
            _ => 0x1000,
        };
        let register_mode = match RESPONSE.load(Ordering::Relaxed) {
            WRONG_IMAGE_MODE => match image_register_mode() {
                RegisterModeV1::LinuxCurrent => RegisterModeV1::UnikernelTls,
                RegisterModeV1::UnikernelTls => RegisterModeV1::LinuxCurrent,
            },
            _ => image_register_mode(),
        };
        CpuBindingResultV1::ok(CpuBindingV1 {
            abi_version: CPU_LOCAL_ABI_VERSION,
            register_mode: register_mode.as_u8(),
            host_level: HostLevelV1::Supervisor.as_u8(),
            cpu_index: CpuIndex::from_u32(0)
                .expect("CPU zero must be representable")
                .as_u32(),
            generation: 1,
            area_base,
            boot_thread: area_base + CPU_AREA_BOOT_THREAD_OFFSET,
            cookie: 0x55aa,
        })
    }

    fn get_tp() -> usize {
        0
    }

    unsafe fn set_tp(_value: usize) -> CpuLocalStatus {
        CpuLocalStatus::Unsupported
    }

    fn current_thread() -> usize {
        0
    }
}

#[test]
fn safe_platform_facade_revalidates_success_payload() {
    RESPONSE.store(MALFORMED_GEOMETRY, Ordering::Relaxed);
    assert_eq!(
        ax_cpu_local::platform::current_cpu_binding(),
        Err(CpuLocalStatus::InvalidBinding)
    );

    RESPONSE.store(WRONG_IMAGE_MODE, Ordering::Relaxed);
    assert_eq!(
        ax_cpu_local::platform::current_cpu_binding(),
        Err(CpuLocalStatus::AbiMismatch)
    );

    RESPONSE.store(VALID, Ordering::Relaxed);
    assert!(ax_cpu_local::platform::current_cpu_binding().is_ok());
}
