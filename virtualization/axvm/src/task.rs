//! Scheduler attachment exposing only execution identity and lower signals.

use std::{ptr, sync::Weak};

use crate::{
    host::task::{SchedulePolicy, SwitchReason, ThreadExtension, ThreadExtensionOps, ThreadId},
    identity::VcpuInstance,
    manager::ControlShared,
    runtime::vcpus::VcpuEvent,
};

#[derive(Clone)]
pub(crate) struct VcpuTaskContext {
    pub(crate) instance: VcpuInstance,
}

struct VcpuAttachment {
    context: VcpuTaskContext,
    exit: Weak<ControlShared>,
}

pub(crate) fn attach(context: VcpuTaskContext, exit: Weak<ControlShared>) -> ThreadExtension {
    let data = Box::into_raw(Box::new(VcpuAttachment { context, exit })) as usize;
    // SAFETY: one owned attachment is transferred to the scheduler. The
    // callback table reads that exact allocation and drops it exactly once.
    unsafe { ThreadExtension::new(data, &VCPU_TASK_OPS) }
}

pub(crate) fn current_task_context() -> Option<VcpuTaskContext> {
    let task = crate::host::task::current_thread();
    let extension = task.extension()?;
    if !ptr::eq(extension.ops(), &VCPU_TASK_OPS) {
        return None;
    }
    // SAFETY: callback-table identity proves the attachment's type. The strong
    // task handle retains its allocation while the narrow context is cloned.
    let attachment = unsafe { &*(extension.data() as *const VcpuAttachment) };
    Some(attachment.context.clone())
}

static VCPU_TASK_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: switch_in,
    on_switch_out: switch_out,
    on_exit: exited,
    on_deadline_overrun: deadline_overrun,
    drop: drop_attachment,
};

unsafe extern "Rust" fn switch_in(_: usize, _: ThreadId, _: SchedulePolicy, _: u64) {}
unsafe extern "Rust" fn switch_out(_: usize, _: ThreadId, _: SwitchReason) {}
unsafe extern "Rust" fn deadline_overrun(_: usize, _: ThreadId) {}

unsafe extern "Rust" fn exited(data: usize, _: ThreadId) {
    // SAFETY: scheduler ordinary task-work invokes this with the transferred
    // allocation alive, after the original execution stack is inactive.
    let attachment = unsafe { &*(data as *const VcpuAttachment) };
    if let Some(owner) = attachment.exit.upgrade() {
        owner.post_event(VcpuEvent::Retired {
            instance: attachment.context.instance,
        });
    }
}

unsafe extern "Rust" fn drop_attachment(data: usize) {
    // SAFETY: the scheduler is the sole destructor of this transferred box.
    drop(unsafe { Box::from_raw(data as *mut VcpuAttachment) });
}
