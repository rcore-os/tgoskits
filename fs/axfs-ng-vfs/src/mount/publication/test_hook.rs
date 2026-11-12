//! One host-thread publication boundary; registration owns only Weak identities.

use alloc::boxed::Box;
use core::{cell::RefCell, marker::PhantomData};

use super::*;

struct ReleasePoint {
    source: Weak<Mountpoint>,
    receiver: Weak<Mountpoint>,
    release: Box<dyn FnOnce()>,
}

std::thread_local! {
    static RELEASE_POINT: RefCell<Option<ReleasePoint>> = const { RefCell::new(None) };
}

pub(in crate::mount) struct ArmedRelease {
    _thread_bound: PhantomData<std::rc::Rc<()>>,
}

impl Drop for ArmedRelease {
    fn drop(&mut self) {
        let remaining = RELEASE_POINT.with(|slot| slot.borrow_mut().take());
        drop(remaining);
    }
}

pub(in crate::mount) fn arm(
    source: &Arc<Mountpoint>,
    receiver: Weak<Mountpoint>,
    release: impl FnOnce() + 'static,
) -> ArmedRelease {
    RELEASE_POINT.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_none(), "publication release point is already armed");
        *slot = Some(ReleasePoint {
            source: Arc::downgrade(source),
            receiver,
            release: Box::new(release),
        });
    });
    ArmedRelease {
        _thread_bound: PhantomData,
    }
}

pub(super) fn after_unlock(source: &Arc<Mountpoint>, retirement: &MountPublicationRetirement) {
    let point = RELEASE_POINT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let point = slot.as_ref()?;
        if point.source.as_ptr() != Arc::as_ptr(source)
            || !retirement.propagation.has_receiver(&point.receiver)
        {
            return None;
        }
        slot.take()
    });
    if let Some(point) = point {
        // No TLS borrow or internal gate survives into the host-thread handoff.
        (point.release)();
    }
}
