//! Deterministic concurrency hooks for the `/dev/net/tun` attach/close TOCTOU
//! windows.
//!
//! The userspace `bugfix-bug-tun-tap-abi` test drives the same two races with a
//! barrier + a 16-round loop, so it only *probabilistically* lands on the exact
//! interleaving. These hooks pin the interleaving deterministically: a two-arrival
//! barrier parks both racers at the contended point before either proceeds, so the
//! claim/commit and the detach/destroy windows are exercised on every run rather
//! than by luck. Each hook flips red when its production guard is removed (see the
//! per-hook old-red/new-green notes).

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_net::TunShared;

use crate::sync::IrqMutex;

/// Two-arrival rendezvous shared by a hook's pair of racer tasks. Enabling it
/// makes [`rendezvous`] block until both racers have arrived, forcing them to
/// contend on the attach-state slot at the same instant. It is inert (a no-op)
/// whenever a hook has not armed it, so it never perturbs other tests.
static RACE_BARRIER_ENABLED: AtomicBool = AtomicBool::new(false);
static RACE_BARRIER_ARRIVALS: AtomicUsize = AtomicUsize::new(0);

fn arm_barrier() {
    RACE_BARRIER_ARRIVALS.store(0, Ordering::Release);
    RACE_BARRIER_ENABLED.store(true, Ordering::Release);
}

fn disarm_barrier() {
    RACE_BARRIER_ENABLED.store(false, Ordering::Release);
}

/// Blocks until both racer tasks have arrived. Called with no lock held: it
/// yields (never spins under the `attach_state` `SpinNoIrq`), so the paired task
/// can make progress and release the rendezvous.
fn rendezvous() {
    if !RACE_BARRIER_ENABLED.load(Ordering::Acquire) {
        return;
    }
    RACE_BARRIER_ARRIVALS.fetch_add(1, Ordering::AcqRel);
    while RACE_BARRIER_ARRIVALS.load(Ordering::Acquire) < 2 {
        ax_task::yield_now();
    }
}

/// Deterministic claim/commit interleaving: two concurrent `TUNSETIFF`-style
/// creators race to build the *same* interface (mirroring two empty-name
/// creates that resolve to one device). Both reach the single-queue claim at the
/// same moment via the barrier; exactly one must win the claim (record the
/// attachment) and the other must observe `EBUSY` and never a second attachment.
///
/// This isolates the invariant the production `set_iff` path relies on: the
/// `try_attach()` slot is the serialization point, so at most one fd ever holds
/// the device (Linux `tun_attach` returns `EBUSY` for a second queue on a
/// non-multi-queue device). No orphan and no double-attach can result.
///
/// Old-red / new-green: with `try_attach()` gating on the `Free` state exactly
/// one racer wins and this returns `true`. Weaken `try_attach()` to
/// unconditionally claim (drop the `*state == Free` guard) and both racers win,
/// so `winners == 2` and this returns `false`.
pub(crate) fn tun_concurrent_claim_has_single_winner_for_test() -> bool {
    let shared = TunShared::new_detached_for_test(alloc::string::String::from("tunrace0"));
    // [winner_count, both-attached-detected]. A slot per racer avoids a shared
    // read-modify-write outside the atomic claim itself.
    let outcomes = Arc::new(IrqMutex::new([false, false]));

    arm_barrier();
    let tasks: [_; 2] = core::array::from_fn(|i| {
        let shared = shared.clone();
        let outcomes = outcomes.clone();
        ax_task::spawn(move || {
            // Both racers land here together, then contend on the claim.
            rendezvous();
            if shared.try_attach() {
                outcomes.lock()[i] = true;
            }
        })
    });
    for task in tasks {
        task.join();
    }
    disarm_barrier();

    let outcomes = outcomes.lock();
    let winners = outcomes.iter().filter(|won| **won).count();
    // Exactly one racer claimed the device, and the slot is left `Attached`
    // (never `Free`, which would mean the winner's claim was lost, nor `Dying`).
    winners == 1 && shared.is_attached_for_test() && !shared.is_dying_for_test()
}

/// Deterministic close-vs-attach interleaving over the detach/destroy window.
/// One racer plays `close()` on the last fd of a non-persistent device: it
/// `mark_dying()`s the slot (the latch `close()` sets before `detach()` +
/// `destroy_tun()`), then, inside that window, the other racer plays a fresh
/// `TUNSETIFF` that found the device by name and tries to claim it.
///
/// The dying latch must make the racing `try_attach()` fail, so the attacher
/// never binds a device that is about to be destroyed (Linux closes the same
/// window by refusing new queues once the device is going away). The barrier
/// pins the attacher inside the window every run.
///
/// Old-red / new-green: with `mark_dying()` run before `detach()` the attacher's
/// `try_attach()` fails and this returns `true`. Drop the `mark_dying()` (detach
/// only, leaving the slot momentarily `Free`) and the attacher wins the slot of
/// a doomed device, so `attached_during_teardown` is `true` and this returns
/// `false`.
pub(crate) fn tun_close_dying_latch_blocks_attach_for_test() -> bool {
    let shared = TunShared::new_detached_for_test(alloc::string::String::from("tunrace1"));
    // The device starts owned by the fd that is now closing.
    assert!(shared.try_attach());
    let attached_during_teardown = Arc::new(IrqMutex::new(false));

    arm_barrier();
    let closer = {
        let shared = shared.clone();
        ax_task::spawn(move || {
            // `close()` closes the TOCTOU window by marking the device dying
            // *before* releasing the queue and destroying it. Both steps run in
            // the window the attacher is parked in.
            rendezvous();
            shared.mark_dying();
            shared.detach();
        })
    };
    let attacher = {
        let shared = shared.clone();
        let attached_during_teardown = attached_during_teardown.clone();
        ax_task::spawn(move || {
            rendezvous();
            // Racing `TUNSETIFF` that resolved the (soon-gone) device by name.
            if shared.try_attach() {
                *attached_during_teardown.lock() = true;
            }
        })
    };
    closer.join();
    attacher.join();
    disarm_barrier();

    // The attacher must never have claimed a dying device, and the device is
    // left terminally `Dying` (destroy_tun would remove it next), so no later
    // racer can revive it either.
    !*attached_during_teardown.lock() && shared.is_dying_for_test() && !shared.try_attach()
}
