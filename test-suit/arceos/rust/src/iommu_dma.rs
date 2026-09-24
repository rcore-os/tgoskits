use std::fs;

use ax_driver::pci::PciIommuFault;
use ax_hal::mem::{VirtAddr, virt_to_phys};
use dma_api::{DmaConstraints, DmaDomainId, DmaError};
use mmio_api::{MmioAddr, MmioRaw};
use rdif_iommu::{Iommu, IommuError, StreamId};

use crate::TestResult;

const NVME_FILE: &str = "/arceos-iommu-dma.bin";
const TESTDEV_BAR_SIZE: usize = 0x1000;
const DMA_TRIGGER: usize = 0x00;
const DMA_IOVA_LO: usize = 0x04;
const DMA_IOVA_HI: usize = 0x08;
const DMA_LEN: usize = 0x0c;
const DMA_RESULT: usize = 0x10;
const DMA_DOORBELL: usize = 0x14;
const DMA_PHYS_LO: usize = 0x1c;
const DMA_PHYS_HI: usize = 0x20;
const DMA_TX_FAIL: u32 = 0xdead_0002;
const DMA_PATTERN: u32 = 0x1234_5678;
const SMMU_TRANSLATION_FAULT: u8 = 0x10;

pub fn run() -> TestResult {
    verify_pci_bindings()?;
    verify_iova_limits_and_rebinding()?;
    ax_driver::pci::verify_iommu_dma_failure_paths()?;
    verify_nvme_io()?;
    verify_iommu_testdev()?;
    println!("IOMMU_DMA_TEST_OK");
    Ok(())
}

fn verify_iova_limits_and_rebinding() -> TestResult {
    let (address, _) = ax_driver::pci::iommu_testdev_endpoint()
        .ok_or("QEMU iommu-testdev was not bound to PCI")?;
    let requester_id = ((address.bus() as u32) << 8)
        | ((address.device() as u32) << 3)
        | address.function() as u32;
    let controller = rdrive::get_one::<Iommu>().ok_or("SMMU controller was not registered")?;
    let duplicate = controller
        .lock()
        .map_err(|_| "SMMU controller lock failed")?
        .bind(StreamId(requester_id));
    if !matches!(duplicate, Err(IommuError::AlreadyBound)) {
        return Err("SMMU accepted a duplicate StreamID binding");
    }

    let too_small = ax_driver::pci::bound_dma(address, 0xfff)
        .map_err(|_| "could not request a constrained DMA capability")?;
    if !matches!(
        too_small.coherent_array_zero_with_align::<u8>(4096, 4096),
        Err(DmaError::NoIova)
    ) {
        return Err("IOVA allocator ignored the device address mask");
    }

    let one_page = ax_driver::pci::bound_dma(address, 0x1fff)
        .map_err(|_| "could not request a one-page IOVA window")?;
    let first = one_page
        .coherent_array_zero_with_align::<u8>(4096, 4096)
        .map_err(|_| "one-page IOVA window allocation failed")?;
    let first_iova = first.dma_addr();
    if first_iova.as_u64() != 0x1000 {
        return Err("one-page IOVA window returned the wrong address");
    }
    if !matches!(
        one_page.coherent_array_zero_with_align::<u8>(4096, 4096),
        Err(DmaError::NoIova)
    ) {
        return Err("IOVA allocator exceeded the one-page window");
    }
    first
        .try_release()
        .map_err(|_| "one-page IOVA window release failed")?;
    let reused = one_page
        .coherent_array_zero_with_align::<u8>(4096, 4096)
        .map_err(|_| "one-page IOVA was not reusable after synchronized unmap")?;
    if reused.dma_addr() != first_iova {
        return Err("released IOVA was not reused within the constrained window");
    }
    reused
        .try_release()
        .map_err(|_| "reused IOVA release failed")?;

    let boundary = ax_driver::pci::bound_dma(address, 0x3fff)
        .map_err(|_| "could not request a bounded DMA capability")?
        .with_constraints(DmaConstraints::new(0x3fff).with_boundary(8192));
    let two_pages = boundary
        .coherent_array_zero_with_align::<u8>(8192, 4096)
        .map_err(|_| "two-page bounded IOVA allocation failed")?;
    if two_pages.dma_addr().as_u64() != 0x2000 {
        return Err("IOVA allocator crossed a device segment boundary");
    }
    two_pages
        .try_release()
        .map_err(|_| "bounded IOVA release failed")?;
    Ok(())
}

fn verify_pci_bindings() -> TestResult {
    let (testdev_address, _) = ax_driver::pci::iommu_testdev_endpoint()
        .ok_or("QEMU iommu-testdev was not bound to PCI")?;
    let mut nvme_address = None;
    let mut testdev_domain = None;
    for (address, domain) in ax_driver::pci::bound_pci_endpoints() {
        if !matches!(domain, DmaDomainId::Translated(_)) {
            return Err("PCI endpoint was left in a direct DMA domain");
        }
        if address == testdev_address {
            testdev_domain = Some(domain);
        } else if nvme_address.replace((address, domain)).is_some() {
            return Err("unexpected additional PCI endpoint in IOMMU QEMU fixture");
        }
    }
    let testdev_domain = testdev_domain.ok_or("iommu-testdev has no translated domain")?;
    let (address, nvme_domain) = nvme_address.ok_or("NVMe endpoint has no translated domain")?;
    if nvme_domain == testdev_domain {
        return Err("NVMe and iommu-testdev share one DMA domain");
    }

    let dma = ax_driver::pci::bound_dma(address, u64::MAX)
        .map_err(|_| "NVMe translated DMA capability is unavailable")?;
    if dma.info().domain() != nvme_domain {
        return Err("NVMe DMA capability does not own its translated domain");
    }
    let buffer = dma
        .coherent_array_zero_with_align::<u8>(TESTDEV_BAR_SIZE, TESTDEV_BAR_SIZE)
        .map_err(|_| "NVMe domain could not allocate a translated DMA buffer")?;
    let physical =
        virt_to_phys(VirtAddr::from_usize(buffer.as_ptr().as_ptr() as usize)).as_usize() as u64;
    if buffer.dma_addr().as_u64() == physical {
        return Err("NVMe DMA address equals its physical address");
    }
    buffer
        .try_release()
        .map_err(|_| "NVMe translated DMA buffer could not be released")?;
    Ok(())
}

fn verify_nvme_io() -> TestResult {
    let expected = (0..16 * 1024)
        .map(|index| (index as u8).wrapping_mul(17).wrapping_add(0x53))
        .collect::<Vec<_>>();

    fs::write(NVME_FILE, &expected).map_err(|_| "NVMe-backed write failed")?;
    let actual = fs::read(NVME_FILE).map_err(|_| "NVMe-backed read failed")?;
    fs::remove_file(NVME_FILE).map_err(|_| "NVMe-backed file cleanup failed")?;
    if actual != expected {
        return Err("NVMe-backed read returned different bytes");
    }
    Ok(())
}

fn verify_iommu_testdev() -> TestResult {
    let (address, bar0) = ax_driver::pci::iommu_testdev_endpoint()
        .ok_or("QEMU iommu-testdev was not bound to PCI")?;
    let requester_id = ((address.bus() as u32) << 8)
        | ((address.device() as u32) << 3)
        | address.function() as u32;
    let dma = ax_driver::pci::bound_dma(address, u64::MAX)
        .map_err(|_| "iommu-testdev has no bound DMA domain")?;
    if !matches!(dma.info().domain(), DmaDomainId::Translated(_)) {
        return Err("iommu-testdev DMA domain is not Translated");
    }

    let faults_before = ax_driver::pci::iommu_fault_count(address)
        .map_err(|_| "failed to inspect SMMU event queue")?;
    if faults_before != 0 {
        return Err("SMMU recorded an unexpected fault before iommu-testdev transfer");
    }

    let mmio = mmio_api::ioremap(MmioAddr::from(bar0), TESTDEV_BAR_SIZE)
        .map_err(|_| "failed to map iommu-testdev BAR0")?;
    let buffer = dma
        .coherent_array_zero_with_align::<u8>(TESTDEV_BAR_SIZE, TESTDEV_BAR_SIZE)
        .map_err(|_| "failed to allocate translated DMA test buffer")?;
    let iova = buffer.dma_addr().as_u64();
    let physical =
        virt_to_phys(VirtAddr::from_usize(buffer.as_ptr().as_ptr() as usize)).as_usize() as u64;
    if iova == physical {
        return Err("translated DMA address equals its physical address");
    }

    let mapped_result = trigger_dma(&mmio, iova, physical);
    if mapped_result != 0 {
        return Err("iommu-testdev could not access translated DMA buffer");
    }
    let bytes = buffer.read_with_cpu(4, |bytes| [bytes[0], bytes[1], bytes[2], bytes[3]]);
    if u32::from_le_bytes(bytes) != DMA_PATTERN {
        return Err("iommu-testdev DMA pattern was not written to translated buffer");
    }
    let faults_after_success = ax_driver::pci::iommu_fault_count(address)
        .map_err(|_| "failed to inspect SMMU event queue after valid DMA")?;
    if faults_after_success != faults_before {
        return Err("SMMU faulted on a valid translated DMA transfer");
    }

    let unmapped_iova = iova
        .checked_add(buffer.bytes_len() as u64)
        .ok_or("translated DMA range overflowed")?;
    if trigger_dma(&mmio, unmapped_iova, physical) != DMA_TX_FAIL {
        return Err("SMMU accepted an unmapped DMA address");
    }
    let unmapped_faults = ax_driver::pci::iommu_drain_faults(address)
        .map_err(|_| "failed to drain SMMU fault after unmapped DMA")?;
    verify_fault_records(&unmapped_faults, requester_id, unmapped_iova)?;
    let faults_after_unmapped = ax_driver::pci::iommu_fault_count(address)
        .map_err(|_| "failed to inspect SMMU fault after unmapped DMA")?;
    if faults_after_unmapped <= faults_after_success {
        return Err("SMMU EVTQ did not record an unmapped DMA fault");
    }

    buffer
        .try_release()
        .map_err(|_| "translated DMA unmap did not complete")?;
    if trigger_dma(&mmio, iova, physical) != DMA_TX_FAIL {
        return Err("SMMU accepted an IOVA after unmap completion");
    }
    let release_faults = ax_driver::pci::iommu_drain_faults(address)
        .map_err(|_| "failed to drain SMMU fault after unmap")?;
    verify_fault_records(&release_faults, requester_id, iova)?;
    let faults_after_release = ax_driver::pci::iommu_fault_count(address)
        .map_err(|_| "failed to inspect SMMU fault after unmap")?;
    if faults_after_release <= faults_after_unmapped {
        return Err("SMMU EVTQ did not record access after unmap");
    }
    Ok(())
}

fn verify_fault_records(faults: &[PciIommuFault], requester_id: u32, iova: u64) -> TestResult {
    if faults.is_empty() {
        return Err("SMMU EVTQ did not report the rejected DMA address");
    }
    if faults.iter().any(|fault| {
        fault.stream_id != requester_id
            || fault.address != iova
            || fault.event_id != SMMU_TRANSLATION_FAULT
    }) {
        return Err("SMMU EVTQ reported a different requester, address, or fault type");
    }
    Ok(())
}

fn trigger_dma(mmio: &MmioRaw, iova: u64, physical: u64) -> u32 {
    mmio.write::<u32>(DMA_IOVA_LO, iova as u32);
    mmio.write::<u32>(DMA_IOVA_HI, (iova >> 32) as u32);
    mmio.write::<u32>(DMA_PHYS_LO, physical as u32);
    mmio.write::<u32>(DMA_PHYS_HI, (physical >> 32) as u32);
    mmio.write::<u32>(DMA_LEN, 4);
    mmio.write::<u32>(DMA_DOORBELL, 1);
    let _ = mmio.read::<u32>(DMA_TRIGGER);
    mmio.read::<u32>(DMA_RESULT)
}
