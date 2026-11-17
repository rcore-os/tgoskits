//! Proc self aliases select the saved caller in this mount's fixed PID view.

use alloc::{format, string::String, sync::Arc, vec};

use axfs_ng_vfs::{LinkCaller, LinkOps, LinkTarget, NodeIdentity, VfsError, VfsResult};

use super::{PidView, SimpleFileBacking, SimpleFsContext};
use crate::task::ProcLinkCaller;

#[derive(Clone, Copy)]
pub(super) enum SelfLinkKind {
    Process,
    Thread,
}

pub(super) fn node(
    fs: SimpleFsContext,
    view: PidView,
    kind: SelfLinkKind,
) -> VfsResult<Arc<SimpleFileBacking>> {
    let entry_kind = match kind {
        SelfLinkKind::Process => 20,
        SelfLinkKind::Thread => 21,
    };
    // The link belongs to this proc instance. Its target belongs to each
    // operation's caller, including reads through a link fd opened by another
    // task. Never bind the identity or recipe to the task materializing it.
    let identity = NodeIdentity::new(vec![fs.identity(), entry_kind]);
    SimpleFileBacking::new_link(fs, identity, SelfLink { view, kind })
}

struct SelfLink {
    view: PidView,
    kind: SelfLinkKind,
}

impl SelfLink {
    fn text(&self, caller: &dyn LinkCaller) -> VfsResult<String> {
        let caller = (caller as &dyn core::any::Any)
            .downcast_ref::<ProcLinkCaller>()
            .ok_or(VfsError::OperationNotSupported)?;
        let process = caller
            .proc_process_number(&self.view)
            .ok_or(VfsError::NotFound)?;
        Ok(match self.kind {
            SelfLinkKind::Process => format!("{}", process.get()),
            SelfLinkKind::Thread => {
                let thread = caller
                    .proc_thread_number(&self.view)
                    .ok_or(VfsError::NotFound)?;
                format!("{}/task/{}", process.get(), thread.get())
            }
        })
    }
}

impl LinkOps for SelfLink {
    fn target(&self, caller: &dyn LinkCaller) -> VfsResult<LinkTarget> {
        self.text(caller).map(LinkTarget::Text)
    }

    fn read_link(&self, caller: &dyn LinkCaller) -> VfsResult<String> {
        self.text(caller)
    }
}
