//! Private host-thread boundaries; callbacks own no topology objects implicitly.

use alloc::boxed::Box;
use core::cell::RefCell;

use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::mount) enum Stage {
    BeforeLock,
    Membership,
    Parent,
}

struct Hook {
    stage: Stage,
    callback: Box<dyn FnOnce()>,
}

std::thread_local! {
    static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

pub(in crate::mount) struct Armed(core::marker::PhantomData<std::rc::Rc<()>>);

impl Drop for Armed {
    fn drop(&mut self) {
        drop(HOOK.with(|slot| slot.borrow_mut().take()));
    }
}

pub(in crate::mount) fn arm(stage: Stage, callback: impl FnOnce() + 'static) -> Armed {
    HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_none());
        *slot = Some(Hook {
            stage,
            callback: Box::new(callback),
        });
    });
    Armed(core::marker::PhantomData)
}

fn run(stage: Stage) {
    let hook = HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|hook| hook.stage == stage) {
            slot.take()
        } else {
            None
        }
    });
    if let Some(hook) = hook {
        (hook.callback)();
    }
}

pub(super) fn before_lock(_: &Arc<Mountpoint>) {
    run(Stage::BeforeLock);
}

pub(super) fn after_membership(_: &Arc<Mountpoint>) {
    run(Stage::Membership);
}

pub(super) fn after_parent(_: &Location) {
    run(Stage::Parent);
}
