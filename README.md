# x86_vcpu

[![CI](https://github.com/arceos-hypervisor/x86_vcpu/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/arceos-hypervisor/x86_vcpu/actions/workflows/ci.yml)

Definition of the vCPU structure and virtualization-related interface support for x86_64 architecture.

The crate user must implement the `PhysFrameIf` trait using
[`crate_interface::impl_interface`](https://crates.io/crates/crate_interface) to provide the low-level implementantion
of the allocation and dealloction of `PhysFrame`, relevant implementation can refer to [ArceOS](https://github.com/arceos-org/arceos/blob/main/modules/axhal/src/paging.rs).

## Example

```Rust
use x86_vcpu::PhysFrameIf;

struct PhysFrameIfImpl;

#[crate_interface::impl_interface]
impl axvm::PhysFrameIf for PhysFrameIfImpl {
    fn alloc_frame() -> Option<PhysAddr> {
        // Your implementation here
    }
    fn dealloc_frame(paddr: PhysAddr) {
        // Your implementation here
    }
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        // Your implementation here
    }
}
```