//! End-to-end proof through the ordinary page allocator, not memory-map logs.
use ax_alloc::UsageKind;
use someboot::{boot_resources::PhysicalRange, handoff};

pub fn exercise() {
    let report = handoff::reclaimed_memory();
    assert!(handoff::takeover_completed());
    assert!(report.iter().map(|r| r.resource_bytes).sum::<u64>() > 256 * 1024 * 1024);
    let archive = handoff::reclaimed_initramfs().expect("archive was consumed and returned");
    let previous_image = handoff::previous_image().expect("inherited caller image");
    let mut required: Vec<PhysicalRange> = report
        .iter()
        .filter(|r| r.range.len >= 64 * 1024)
        .map(|r| r.range)
        .collect();
    required.push(archive);
    required.push(previous_image);
    let mut hit = vec![false; required.len()];
    // Preallocate all bookkeeping before applying memory pressure. Large blocks
    // drain the bulk of RAM quickly; single pages then reach small sections.
    let page_budget = ax_hal::mem::memory_regions()
        .filter(|r| r.flags.contains(ax_hal::mem::MemRegionFlags::FREE))
        .map(|r| r.size / 4096)
        .sum::<usize>();
    let mut held = Vec::with_capacity(page_budget);
    let allocator = ax_alloc::global_allocator();
    for count in [512, 1] {
        while !hit.iter().all(|v| *v) {
            let Ok(address) = allocator.alloc_pages(count, 4096, UsageKind::Global) else {
                break;
            };
            assert!(held.len() < held.capacity());
            held.push((address, count));
            let physical = ax_hal::mem::virt_to_phys(address.into()).as_usize() as u64;
            let allocation = PhysicalRange::new(physical, (count * 4096) as u64);
            for (i, required) in required.iter().enumerate() {
                let start = physical.max(required.start).next_multiple_of(4096);
                let end = allocation.end().unwrap().min(required.end().unwrap());
                if !hit[i] && start.checked_add(4096).is_some_and(|v| v <= end) {
                    let pointer = (address + (start - physical) as usize) as *mut u64;
                    // SAFETY: this ordinary allocation exclusively owns the
                    // selected complete page. It lies in a reclaimed range.
                    unsafe {
                        for word in 0..512 {
                            pointer
                                .add(word)
                                .write_volatile(start ^ word as u64 ^ 0xa5a5_5a5a);
                        }
                        for word in 0..512 {
                            assert_eq!(
                                pointer.add(word).read_volatile(),
                                start ^ word as u64 ^ 0xa5a5_5a5a
                            );
                        }
                    }
                    hit[i] = true;
                }
            }
        }
    }
    for (address, count) in held.drain(..) {
        allocator.dealloc_pages(address, count, UsageKind::Global);
    }
    for (range, hit) in required.iter().zip(&hit) {
        assert!(
            *hit,
            "reclaimed range was unavailable to ordinary pages: {range:?}"
        );
        println!(
            "HANDOFF_RECLAIM_WRITE_READ {:#x}..{:#x}",
            range.start,
            range.end().unwrap()
        );
    }
    println!("HANDOFF_INITRAMFS_RECLAIM_PASSED bytes={}", archive.len);
    println!("HANDOFF_MEMORY_RECLAIM_PASSED ranges={}", required.len());
}
