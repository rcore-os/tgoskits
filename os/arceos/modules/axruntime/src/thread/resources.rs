use alloc::boxed::Box;
use core::{
    alloc::Layout,
    ptr::{self, NonNull},
};

use ax_task::runtime::{
    RuntimeHandleResult, RuntimeStatus,
    resource::{StackHandle, StackRequest, TlsHandle},
};

#[cfg(feature = "paging")]
use super::PAGE_SIZE;

pub(super) struct RuntimeStack {
    pub(super) usable_top: usize,
    pub(super) backing: StackBacking,
}

pub(super) enum StackBacking {
    Heap {
        pointer: NonNull<u8>,
        layout: Layout,
    },
    #[cfg(feature = "paging")]
    VirtualPages(ax_mm::KernelVirtualAllocation),
}

#[cfg(kernel_tls)]
struct RuntimeTls {
    area: ax_hal::tls::TlsArea,
}

pub(super) fn allocate_runtime_stack(request: StackRequest) -> Result<StackHandle, RuntimeStatus> {
    #[cfg(feature = "fault-injection")]
    if super::creation_probe::record(super::creation_probe::CreationEvent::Stack) {
        return Err(RuntimeStatus::NoMemory);
    }
    if request.usable_size == 0 || request.alignment == 0 || !request.alignment.is_power_of_two() {
        return Err(RuntimeStatus::InvalidArgument);
    }

    if request.guard_size == 0 && !cfg!(feature = "vmap-task-stack") {
        return allocate_heap_stack(request);
    }

    #[cfg(feature = "paging")]
    {
        allocate_virtual_stack(request)
    }
    #[cfg(not(feature = "paging"))]
    {
        Err(RuntimeStatus::Unsupported)
    }
}

fn allocate_heap_stack(request: StackRequest) -> Result<StackHandle, RuntimeStatus> {
    let layout = Layout::from_size_align(request.usable_size, request.alignment)
        .map_err(|_| RuntimeStatus::InvalidArgument)?;
    let stack = super::allocation::try_box(core::mem::MaybeUninit::<RuntimeStack>::uninit())?;
    super::allocation::allocation_point()?;
    let pointer = ax_alloc::global_allocator()
        .alloc(layout)
        .map_err(map_alloc_status)?;
    let base = pointer.as_ptr() as usize;
    let Some(usable_top) = base.checked_add(request.usable_size) else {
        ax_alloc::global_allocator().dealloc(pointer, layout);
        return Err(RuntimeStatus::InvalidArgument);
    };
    let stack = Box::write(
        stack,
        RuntimeStack {
            usable_top,
            backing: StackBacking::Heap { pointer, layout },
        },
    );
    // SAFETY: Box::into_raw yields a non-null uniquely owned RuntimeStack that
    // stays live until deallocate_runtime_stack consumes this exact handle.
    Ok(unsafe { StackHandle::from_raw(Box::into_raw(stack).expose_provenance()) })
}

#[cfg(feature = "paging")]
fn allocate_virtual_stack(request: StackRequest) -> Result<StackHandle, RuntimeStatus> {
    if !request.guard_size.is_multiple_of(PAGE_SIZE) {
        return Err(RuntimeStatus::InvalidArgument);
    }
    let alignment = request.alignment.max(PAGE_SIZE);
    let usable_size = request
        .usable_size
        .checked_next_multiple_of(alignment)
        .ok_or(RuntimeStatus::InvalidArgument)?;
    let guard_size = request
        .guard_size
        .checked_next_multiple_of(alignment)
        .ok_or(RuntimeStatus::InvalidArgument)?;
    let layout = ax_mm::KernelVirtualAllocationLayout::new(
        usable_size,
        ax_hal::paging::MappingFlags::READ | ax_hal::paging::MappingFlags::WRITE,
        ax_alloc::UsageKind::TaskStack,
    )
    .and_then(|layout| layout.with_leading_guard_pages(guard_size / PAGE_SIZE))
    .and_then(|layout| layout.with_alignment(alignment))
    .map_err(|_| RuntimeStatus::InvalidArgument)?;
    // This is an ordinary resource-preparation path. A previous failed
    // shootdown may be retried here, before reserving another virtual range.
    ax_mm::retry_kernel_virtual_quarantines(8);
    let stack = super::allocation::try_box(core::mem::MaybeUninit::<RuntimeStack>::uninit())?;
    let allocation =
        ax_mm::KernelVirtualAllocation::allocate(layout).map_err(|error| match error {
            ax_mm::MmError::NoMemory => RuntimeStatus::NoMemory,
            _ => RuntimeStatus::Platform,
        })?;
    let stack = Box::write(
        stack,
        RuntimeStack {
            usable_top: allocation.usable_range().end.as_usize(),
            backing: StackBacking::VirtualPages(allocation),
        },
    );
    // SAFETY: the box uniquely owns the reservation and remains live until
    // the scheduler consumes this handle after the context has stopped.
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
        StackBacking::VirtualPages(allocation) => {
            if let Err(error) = allocation.release() {
                // The MM metadata retains the frames and VA across failure.
                // The consumed stack handle itself no longer owns resources.
                warn!("task stack retained in kernel virtual quarantine: {error}");
            }
        }
    }
    #[cfg(feature = "fault-injection")]
    super::creation_probe::record(super::creation_probe::CreationEvent::DropStack);
    RuntimeStatus::Success
}

pub(super) fn allocate_runtime_tls() -> RuntimeHandleResult {
    #[cfg(feature = "fault-injection")]
    if super::creation_probe::record(super::creation_probe::CreationEvent::Tls) {
        return RuntimeHandleResult::failure(RuntimeStatus::NoMemory);
    }
    #[cfg(kernel_tls)]
    {
        let allocated = (|| {
            super::allocation::allocation_point()?;
            let area = ax_hal::tls::TlsArea::try_alloc().ok_or(RuntimeStatus::NoMemory)?;
            super::allocation::try_box(RuntimeTls { area })
        })();
        match allocated {
            Ok(tls) => RuntimeHandleResult::success(Box::into_raw(tls).expose_provenance()),
            Err(status) => RuntimeHandleResult::failure(status),
        }
    }
    #[cfg(not(kernel_tls))]
    {
        RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
    }
}

pub(super) fn deallocate_runtime_tls(handle: TlsHandle) -> RuntimeStatus {
    #[cfg(feature = "fault-injection")]
    if !handle.is_none() {
        super::creation_probe::record(super::creation_probe::CreationEvent::DropTls);
    }
    if handle.is_none() {
        return RuntimeStatus::Success;
    }
    #[cfg(kernel_tls)]
    {
        // SAFETY: the scheduler consumes a live runtime TLS handle once.
        drop(unsafe {
            Box::from_raw(ptr::with_exposed_provenance_mut::<RuntimeTls>(
                handle.into_raw(),
            ))
        });
        RuntimeStatus::Success
    }
    #[cfg(not(kernel_tls))]
    {
        RuntimeStatus::Unsupported
    }
}

#[cfg(kernel_tls)]
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

#[cfg(not(kernel_tls))]
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
