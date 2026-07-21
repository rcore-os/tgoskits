mod area;
mod thread;

pub use area::*;
pub use thread::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CpuIndex, HostLevelV1, image_register_mode};

    fn init(cpu: usize, area_base: usize) -> CpuAreaInitV2 {
        CpuAreaInitV2::new(
            image_register_mode(),
            HostLevelV1::Supervisor,
            CpuIndex::try_from(cpu).unwrap(),
            11,
            area_base,
            area_base + CPU_AREA_BOOT_THREAD_OFFSET,
            0x55aa,
        )
    }

    #[test]
    fn prefix_v2_uses_three_cache_lines() {
        assert_eq!(size_of::<CpuAreaPrefixV2>(), 192);
        assert_eq!(CPU_AREA_RUNTIME_ANCHOR_OFFSET, 64);
        assert_eq!(CPU_AREA_BOOT_THREAD_OFFSET, 128);
        assert_eq!(CPU_AREA_CURRENT_THREAD_OFFSET, 64);
        assert_eq!(
            CPU_AREA_KERNEL_STACK_POINTER_OFFSET,
            64 + size_of::<usize>()
        );
    }

    #[test]
    fn final_prefix_publishes_the_permanent_boot_header() {
        let prefix = CpuAreaPrefixV2::initialize(init(3, 0x8000)).unwrap();
        assert_eq!(prefix.runtime_anchor().current_thread_raw(), 0x8080);
        let binding = prefix.boot_thread().header().cpu_binding().unwrap();
        assert_eq!(binding.area_base(), 0x8000);
        assert_eq!(binding.cpu_index().as_u32(), 3);
        assert_eq!(
            prefix.boot_thread().header().context_identity().as_usize(),
            0,
            "the permanent boot header must not impersonate a runtime-owned task",
        );
    }

    #[test]
    fn pinned_identity_and_four_phase_cpu_binding_round_trip() {
        let context = ContextIdentity::from_raw(7).unwrap();
        let thread = ThreadIdentity::from_parts(4, 2).unwrap();
        let header = Box::pin(CurrentThreadHeader::new(context));
        header.as_ref().bind_thread(thread).unwrap();
        let binding = init(1, 0x8000).binding();
        // SAFETY: this fixture is the only scheduler owner.
        let epoch = unsafe { header.as_ref().bind_cpu(binding) }.unwrap();
        assert_eq!(header.cpu_binding().unwrap().epoch(), epoch);
        // SAFETY: no CPU current slot publishes this fixture.
        unsafe { header.as_ref().unbind_cpu(epoch) }.unwrap();
        assert!(header.cpu_binding().is_none());
        // SAFETY: rebind occurs only after the previous unbind Release.
        let next = unsafe { header.as_ref().bind_cpu(binding) }.unwrap();
        assert_ne!(epoch, next);
    }

    #[test]
    fn failed_second_thread_bind_does_not_modify_identity() {
        let first = ThreadIdentity::from_parts(4, 2).unwrap();
        let second = ThreadIdentity::from_parts(5, 9).unwrap();
        let header = Box::pin(CurrentThreadHeader::new(
            ContextIdentity::from_raw(1).unwrap(),
        ));
        header.as_ref().bind_thread(first).unwrap();
        assert_eq!(
            header.as_ref().bind_thread(second),
            Err(CurrentThreadError::ThreadAlreadyBound)
        );
        assert_eq!(header.thread_identity(), Some(first));
    }
}
