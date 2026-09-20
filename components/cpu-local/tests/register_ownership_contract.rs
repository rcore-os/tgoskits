//! Per-thread CPU binding installation.

#[cfg(feature = "host-test")]
use std::mem::MaybeUninit;

#[test]
#[cfg(feature = "host-test")]
fn each_host_thread_must_install_its_own_cpu_binding() {
    std::thread::spawn(|| {
        assert_eq!(
            unsafe { cpu_local::with_cpu_pin(|_| ()) },
            Err(cpu_local::CpuLocalError::AreaNotInstalled)
        );

        let storage = Box::leak(Box::new(MaybeUninit::<cpu_local::CpuAreaPrefix>::uninit()));
        let base = storage.as_mut_ptr() as usize;
        storage.write(
            cpu_local::CpuAreaPrefix::initialize(cpu_local::CpuIndex::try_from(1).unwrap(), base)
                .unwrap(),
        );
        let area = unsafe { cpu_local::CpuAreaRef::from_initialized_base(base) }.unwrap();
        // SAFETY: this thread explicitly owns the leaked CPU fixture and cannot
        // receive modeled traps while installing the completed area.
        unsafe { cpu_local::install_cpu_area(area) }.unwrap();
        assert_eq!(
            unsafe { cpu_local::with_cpu_pin(|pin| pin.area().base()) },
            Ok(base)
        );
    })
    .join()
    .unwrap();
}
