use alloc::boxed::Box;
use core::{
    alloc::Layout,
    ptr::{self, NonNull},
};

use ax_task::runtime::{
    RuntimeHandleResult, RuntimeStatus, StackHandle, StackRequest, TlsHandle, TlsRequest,
};

use super::PAGE_SIZE;

pub(super) struct RuntimeStack {
    #[cfg(feature = "paging")]
    pub(super) base: usize,
    pub(super) usable_top: usize,
    pub(super) backing: StackBacking,
}

pub(super) enum StackBacking {
    Heap {
        pointer: NonNull<u8>,
        layout: Layout,
    },
    #[cfg(feature = "paging")]
    GuardedPages { pages: usize, guard_size: usize },
}

#[cfg(feature = "tls")]
struct RuntimeTls {
    area: ax_hal::tls::TlsArea,
}

pub(super) fn allocate_runtime_stack(request: StackRequest) -> Result<StackHandle, RuntimeStatus> {
    if request.usable_size == 0 || request.alignment == 0 || !request.alignment.is_power_of_two() {
        return Err(RuntimeStatus::InvalidArgument);
    }

    if request.guard_size == 0 {
        return allocate_heap_stack(request);
    }

    #[cfg(feature = "paging")]
    {
        allocate_guarded_stack(request)
    }
    #[cfg(not(feature = "paging"))]
    {
        Err(RuntimeStatus::Unsupported)
    }
}

fn allocate_heap_stack(request: StackRequest) -> Result<StackHandle, RuntimeStatus> {
    let layout = Layout::from_size_align(request.usable_size, request.alignment)
        .map_err(|_| RuntimeStatus::InvalidArgument)?;
    let pointer = ax_alloc::global_allocator()
        .alloc(layout)
        .map_err(map_alloc_status)?;
    let base = pointer.as_ptr() as usize;
    let usable_top = base
        .checked_add(request.usable_size)
        .ok_or(RuntimeStatus::InvalidArgument)?;
    let stack = Box::new(RuntimeStack {
        #[cfg(feature = "paging")]
        base,
        usable_top,
        backing: StackBacking::Heap { pointer, layout },
    });
    // SAFETY: Box::into_raw yields a non-null uniquely owned RuntimeStack that
    // stays live until deallocate_runtime_stack consumes this exact handle.
    Ok(unsafe { StackHandle::from_raw(Box::into_raw(stack).expose_provenance()) })
}

#[cfg(feature = "paging")]
fn allocate_guarded_stack(request: StackRequest) -> Result<StackHandle, RuntimeStatus> {
    if !request.guard_size.is_multiple_of(PAGE_SIZE) {
        return Err(RuntimeStatus::InvalidArgument);
    }
    let usable_size = request
        .usable_size
        .checked_add(PAGE_SIZE - 1)
        .ok_or(RuntimeStatus::InvalidArgument)?
        / PAGE_SIZE
        * PAGE_SIZE;
    let total_size = request
        .guard_size
        .checked_add(usable_size)
        .ok_or(RuntimeStatus::InvalidArgument)?;
    let pages = total_size / PAGE_SIZE;
    let base = ax_alloc::global_allocator()
        .alloc_pages(
            pages,
            request.alignment.max(PAGE_SIZE),
            ax_alloc::UsageKind::Global,
        )
        .map_err(map_alloc_status)?;
    let guard = ax_memory_addr::VirtAddr::from(base);
    if crate::kernel_mapping::protect_kernel_range(
        guard,
        request.guard_size,
        ax_hal::paging::MappingFlags::empty(),
    )
    .is_err()
    {
        ax_alloc::global_allocator().dealloc_pages(base, pages, ax_alloc::UsageKind::Global);
        return Err(RuntimeStatus::Platform);
    }
    let stack = Box::new(RuntimeStack {
        base,
        usable_top: base + total_size,
        backing: StackBacking::GuardedPages {
            pages,
            guard_size: request.guard_size,
        },
    });
    // SAFETY: Box::into_raw yields a non-null uniquely owned RuntimeStack that
    // stays live until deallocate_runtime_stack consumes this exact handle.
    Ok(unsafe { StackHandle::from_raw(Box::into_raw(stack).expose_provenance()) })
}

pub(super) fn deallocate_runtime_stack(handle: StackHandle) -> RuntimeStatus {
    if handle.is_none() {
        return RuntimeStatus::InvalidHandle;
    }
    // SAFETY: ax-task passes only a live handle returned by
    // `allocate_runtime_stack`, and consumes it exactly once during reaping.
    let stack = unsafe {
        Box::from_raw(ptr::with_exposed_provenance_mut::<RuntimeStack>(
            handle.into_raw(),
        ))
    };
    match stack.backing {
        StackBacking::Heap { pointer, layout } => {
            ax_alloc::global_allocator().dealloc(pointer, layout);
        }
        #[cfg(feature = "paging")]
        StackBacking::GuardedPages { pages, guard_size } => {
            let guard = ax_memory_addr::VirtAddr::from(stack.base);
            let restore = ax_hal::paging::MappingFlags::READ | ax_hal::paging::MappingFlags::WRITE;
            if crate::kernel_mapping::protect_kernel_range(guard, guard_size, restore).is_err() {
                core::mem::forget(stack);
                return RuntimeStatus::Platform;
            }
            ax_alloc::global_allocator().dealloc_pages(
                stack.base,
                pages,
                ax_alloc::UsageKind::Global,
            );
        }
    }
    RuntimeStatus::Success
}

pub(super) fn allocate_runtime_tls(_request: TlsRequest) -> RuntimeHandleResult {
    #[cfg(feature = "tls")]
    {
        let tls = Box::new(RuntimeTls {
            area: ax_hal::tls::TlsArea::alloc(),
        });
        RuntimeHandleResult::success(Box::into_raw(tls).expose_provenance())
    }
    #[cfg(not(feature = "tls"))]
    {
        RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
    }
}

pub(super) fn deallocate_runtime_tls(handle: TlsHandle) -> RuntimeStatus {
    if handle.is_none() {
        return RuntimeStatus::Success;
    }
    #[cfg(feature = "tls")]
    {
        // SAFETY: the scheduler consumes a live runtime TLS handle once.
        drop(unsafe {
            Box::from_raw(ptr::with_exposed_provenance_mut::<RuntimeTls>(
                handle.into_raw(),
            ))
        });
        RuntimeStatus::Success
    }
    #[cfg(not(feature = "tls"))]
    {
        RuntimeStatus::Unsupported
    }
}

#[cfg(feature = "tls")]
pub(super) fn runtime_tls_pointer(handle: TlsHandle) -> usize {
    if handle.is_none() {
        return 0;
    }
    // SAFETY: context creation borrows a live runtime TLS handle.
    unsafe {
        (&*ptr::with_exposed_provenance::<RuntimeTls>(handle.into_raw()))
            .area
            .tls_ptr()
            .addr()
    }
}

#[cfg(not(feature = "tls"))]
pub(super) fn runtime_tls_pointer(_handle: TlsHandle) -> usize {
    0
}

fn map_alloc_status(error: ax_alloc::AllocError) -> RuntimeStatus {
    match error {
        ax_alloc::AllocError::NoMemory => RuntimeStatus::NoMemory,
        ax_alloc::AllocError::InvalidParam => RuntimeStatus::InvalidArgument,
        _ => RuntimeStatus::Platform,
    }
}
