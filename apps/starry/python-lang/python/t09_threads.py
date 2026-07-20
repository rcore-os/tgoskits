#!/usr/bin/env python3
"""threading + queue + concurrent.futures — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False

import threading
import time
import queue
from concurrent.futures import (
    ThreadPoolExecutor, Future, as_completed, wait,
    FIRST_COMPLETED, FIRST_EXCEPTION, ALL_COMPLETED, TimeoutError as FTimeoutError,
)

# ---------------------------------------------------------------------------
# threading.Thread — docs.python.org/3/library/threading.html#thread-objects
# A Thread runs `target(*args, **kwargs)` in a separate flow of control.
# 怎么测: build threads passing target/args/kwargs, start(), join(), observe
#   results pushed cross-thread; check is_alive/name/ident/native_id transitions.
# 期望: started thread runs target exactly once; is_alive True while running and
#   False after join; ident/native_id are ints once started, None before.
# 为什么: this is the core of the module; everything else synchronizes Threads.
# ---------------------------------------------------------------------------
_res = {}
def _job(a, b, key="r"):
    _res[key] = a + b

t = threading.Thread(target=_job, args=(3, 4), kwargs={"key": "sum"})
chk("thread_ident_before_start", t.ident is None)
t.start()
t.join()
chk("thread_target_args_kwargs", _res.get("sum") == 7)
chk("thread_not_alive_after_join", t.is_alive() is False)
chk("thread_ident_after_start", isinstance(t.ident, int) and t.ident != 0)

# Thread.name — settable label; default is "Thread-N (target)".
nt = threading.Thread(target=lambda: None, name="namedworker")
chk("thread_name_set", nt.name == "namedworker")
nt.name = "renamed"
chk("thread_name_mutable", nt.name == "renamed")
nt.start(); nt.join()

# Thread.native_id — OS-level thread id, set once running, int (PEP 3.8+).
ntid_box = {}
def _grab_ntid():
    ntid_box["nid"] = threading.get_native_id()
    ntid_box["self_nid"] = threading.current_thread().native_id
nt2 = threading.Thread(target=_grab_ntid)
nt2.start(); nt2.join()
if hasattr(threading, "get_native_id"):
    chk("thread_native_id", isinstance(nt2.native_id, int)
        and nt2.native_id == ntid_box.get("nid"))
    chk("get_native_id_int", isinstance(ntid_box.get("nid"), int) and ntid_box["nid"] > 0)
else:
    chk("thread_native_id", True, "(skip: needs 3.8 get_native_id)")
    chk("get_native_id_int", True, "(skip: needs 3.8 get_native_id)")

# Thread.is_alive — True between start() and target return.
alive_started = threading.Event()
alive_release = threading.Event()
def _hold():
    alive_started.set()
    alive_release.wait(5)
ht = threading.Thread(target=_hold)
ht.start()
alive_started.wait(5)
chk("thread_is_alive_running", ht.is_alive() is True)
alive_release.set()
ht.join()
chk("thread_is_alive_finished", ht.is_alive() is False)

# Thread.join(timeout=...) — blocks up to timeout; returns None unconditionally
# (py3); thread stays alive if it didn't finish within the timeout, then a final
# bare join() reaps it. Catches a kernel where a timed join hangs or returns early.
jt_release = threading.Event()
jt = threading.Thread(target=lambda: jt_release.wait(5))
jt.start()
jt0 = time.monotonic()
jret = jt.join(timeout=0.05)
jt_el = time.monotonic() - jt0
chk("thread_join_timeout_returns_none", jret is None)
chk("thread_join_timeout_still_alive", jt.is_alive() is True and jt_el >= 0.045,
    "el=%.3f" % jt_el)
jt_release.set()
jt.join()
chk("thread_join_after_timeout_reaps", jt.is_alive() is False)

# Starting a Thread twice raises RuntimeError; joining unstarted raises RuntimeError.
try:
    t.start()
    chk("thread_double_start", False)
except RuntimeError:
    chk("thread_double_start", True)
try:
    threading.Thread(target=lambda: None).join()
    chk("thread_join_unstarted", False)
except RuntimeError:
    chk("thread_join_unstarted", True)

# Thread daemon flag — daemon threads do not block interpreter exit; the flag is
# inherited from the creating thread and read/written via .daemon.
dt = threading.Thread(target=lambda: None, daemon=True)
chk("thread_daemon_true", dt.daemon is True)
dt.start(); dt.join()
ndt = threading.Thread(target=lambda: None)
chk("thread_daemon_default_false", ndt.daemon is False)
ndt.start(); ndt.join()

# Thread subclassing — override run(); start() invokes run() in the new thread.
class MyThread(threading.Thread):
    def __init__(self):
        super().__init__()
        self.value = None
    def run(self):
        self.value = 99
mt = MyThread()
mt.start(); mt.join()
chk("thread_subclass_run", mt.value == 99)

# ---------------------------------------------------------------------------
# threading module-level helpers — current_thread/main_thread/enumerate/
#   active_count/get_ident/stack_size.
# 怎么测: query identities from main and from a worker; enumerate during a live
#   worker; compare counts and identities.
# 期望: current_thread() differs per-thread; main_thread() is stable; enumerate
#   includes the live worker; active_count() == len(enumerate()).
# ---------------------------------------------------------------------------
main_t = threading.main_thread()
chk("main_thread_is_current_in_main", threading.current_thread() is main_t)

seen = {}
worker_running = threading.Event()
worker_go = threading.Event()
def _probe():
    seen["cur"] = threading.current_thread()
    seen["ident"] = threading.get_ident()
    seen["main"] = threading.main_thread()
    worker_running.set()
    worker_go.wait(5)
pw = threading.Thread(target=_probe, name="probe")
pw.start()
worker_running.wait(5)
enum_list = threading.enumerate()
chk("enumerate_contains_worker", pw in enum_list)
chk("enumerate_contains_main", main_t in enum_list)
chk("active_count_matches_enumerate", threading.active_count() == len(enum_list))
worker_go.set()
pw.join()
chk("current_thread_differs", seen["cur"] is pw and seen["cur"] is not main_t)
chk("worker_main_thread_stable", seen["main"] is main_t)
chk("get_ident_int", isinstance(seen["ident"], int) and seen["ident"] != 0)
chk("get_ident_distinct", seen["ident"] != threading.get_ident())

# stack_size() — returns current thread stack size (0 = platform default); may
# accept a new size. Treat as best-effort: must at least return an int.
try:
    ss = threading.stack_size()
    chk("stack_size_returns_int", isinstance(ss, int))
except (ValueError, RuntimeError) as e:
    chk("stack_size_returns_int", True, "(unsupported: %s)" % type(e).__name__)

# ---------------------------------------------------------------------------
# threading.Lock — primitive non-reentrant mutex; acquire()/release()/with.
# 怎么测: 8 threads each increment a shared counter 1000x under the same Lock →
#   must equal exactly 8000 (proves real mutual exclusion, no lost updates).
# 期望: 8000 exactly; acquire(blocking=False) returns False when held; releasing
#   an unlocked Lock raises RuntimeError; locked() reflects state.
# 为什么: Lock is the foundational synchronization primitive.
# ---------------------------------------------------------------------------
counter = 0
lock = threading.Lock()
def _bump():
    global counter
    for _ in range(1000):
        with lock:
            counter += 1
threads = [threading.Thread(target=_bump) for _ in range(8)]
for th in threads: th.start()
for th in threads: th.join()
chk("lock_8x1000_eq_8000", counter == 8000, "got=%d" % counter)

l2 = threading.Lock()
chk("lock_acquire_returns_true", l2.acquire() is True)
chk("lock_locked_true", l2.locked() is True)
chk("lock_nonblocking_when_held", l2.acquire(blocking=False) is False)
l2.release()
chk("lock_locked_false_after_release", l2.locked() is False)
try:
    l2.release()
    chk("lock_release_unlocked_raises", False)
except RuntimeError:
    chk("lock_release_unlocked_raises", True)
# acquire(timeout=...) returns False if not obtained in time.
l3 = threading.Lock()
l3.acquire()
t0 = time.monotonic()
got = l3.acquire(timeout=0.05)
# Must actually wait ~timeout: >= 0.045 catches a kernel that returns too early
# (coarse timer / no-op blocking) instead of honoring the requested 0.05s.
_lk_to_el = time.monotonic() - t0
# Lower bound proves it actually waited; a generous upper bound (5s) proves it
# did not wait grossly too long (e.g. ignoring the timeout and blocking forever
# until some unrelated wakeup) without being flaky under slow TCG scheduling.
chk("lock_acquire_timeout_false", got is False and 0.045 <= _lk_to_el < 5.0,
    "el=%.3f" % _lk_to_el)
l3.release()

# Lock as a context manager: __enter__ acquires and returns True (the acquire
# result), __exit__ releases. Verify the bound value and that the lock is held
# inside the block and free after.
l4 = threading.Lock()
with l4 as l4_entered:
    chk("lock_context_manager_enter_true", l4_entered is True)
    chk("lock_context_manager_held_inside", l4.locked() is True)
chk("lock_context_manager_released_after", l4.locked() is False)

# ---------------------------------------------------------------------------
# threading.RLock — reentrant lock: same thread may acquire repeatedly, must
#   release the same number of times. with-statement supported.
# 怎么测: nested with on one RLock from one thread; recursive function holding it.
# 期望: nested acquisition succeeds; lock released only after matching releases.
# ---------------------------------------------------------------------------
rlock = threading.RLock()
with rlock:
    with rlock:
        depth_ok = True
chk("rlock_reentrant_nested", depth_ok)

def _recurse(n):
    rlock.acquire()
    try:
        if n > 0:
            return _recurse(n - 1) + 1
        return 0
    finally:
        rlock.release()
chk("rlock_recursive_count", _recurse(5) == 5)
# After balanced releases the RLock is free for another thread.
rl2 = threading.RLock()
rl2.acquire(); rl2.acquire(); rl2.release(); rl2.release()
acq_box = {}
def _try_rl2():
    acq_box["got"] = rl2.acquire(timeout=1)
    if acq_box["got"]:
        rl2.release()
trl = threading.Thread(target=_try_rl2); trl.start(); trl.join()
chk("rlock_free_after_balanced", acq_box.get("got") is True)

# ---------------------------------------------------------------------------
# threading.Condition — wait()/notify()/notify_all()/wait_for(predicate).
# 怎么測: producer sets a shared value and notifies; consumer wait_for(predicate).
# 期望: consumer unblocks exactly when predicate true; notify_all wakes all.
# 为什么: Condition is the canonical wait/notify building block.
# ---------------------------------------------------------------------------
cond = threading.Condition()
shared = {"ready": False, "val": None}
cv_out = {}
def _consumer():
    with cond:
        cond.wait_for(lambda: shared["ready"])
        cv_out["val"] = shared["val"]
cc = threading.Thread(target=_consumer)
cc.start()
time.sleep(0.02)
with cond:
    shared["val"] = 123
    shared["ready"] = True
    cond.notify()
cc.join()
chk("condition_wait_for", cv_out.get("val") == 123)

# notify_all wakes every waiter; collect N woken counts.
cond2 = threading.Condition()
woke = []
gate = {"open": False}
def _w():
    with cond2:
        cond2.wait_for(lambda: gate["open"])
        woke.append(1)
ws = [threading.Thread(target=_w) for _ in range(5)]
for w in ws: w.start()
time.sleep(0.05)
with cond2:
    gate["open"] = True
    cond2.notify_all()
for w in ws: w.join()
chk("condition_notify_all", sum(woke) == 5)

# Condition.wait(timeout) returns False on timeout (no notify).
cond3 = threading.Condition()
with cond3:
    r = cond3.wait(timeout=0.03)
chk("condition_wait_timeout_false", r is False)
# Condition.wait_for(predicate, timeout) returns the (last) predicate value, i.e.
# False when the predicate never becomes true within timeout — and must actually
# block ~timeout, not return early.
cond_wf = threading.Condition()
with cond_wf:
    t_wf = time.monotonic()
    wf = cond_wf.wait_for(lambda: False, timeout=0.05)
    wf_el = time.monotonic() - t_wf
chk("condition_wait_for_timeout_false", wf is False and wf_el >= 0.045,
    "el=%.3f" % wf_el)
# Condition(lock=...) — wraps an existing Lock/RLock instead of creating its own;
# holding the Condition then holds that underlying lock (proved cross-thread:
# another thread cannot acquire it while the Condition is held, but can once
# released). Docs: threading.Condition(lock=None).
cond_lk = threading.Lock()
cond_custom = threading.Condition(cond_lk)
cl_box = {}
cond_custom.acquire()
def _probe_underlying_held():
    cl_box["held"] = cond_lk.acquire(blocking=False)
plk = threading.Thread(target=_probe_underlying_held); plk.start(); plk.join()
chk("condition_custom_lock_held", cl_box.get("held") is False)
cond_custom.release()
def _probe_underlying_free():
    got = cond_lk.acquire(blocking=False)
    cl_box["free"] = got
    if got:
        cond_lk.release()
plk2 = threading.Thread(target=_probe_underlying_free); plk2.start(); plk2.join()
chk("condition_custom_lock_free_after_release", cl_box.get("free") is True)

# Condition.notify(n) — wakes exactly n waiters (not all). Use plain wait() so each
# wakeup is observable; non-notified waiters stay blocked until notify_all().
cond_n = threading.Condition()
n_woke = []
n_woke_lock = threading.Lock()
n_ready = []
def _notify_n_waiter():
    with cond_n:
        n_ready.append(1)
        cond_n.wait()
        with n_woke_lock:
            n_woke.append(1)
NW = 4
nws = [threading.Thread(target=_notify_n_waiter) for _ in range(NW)]
for x in nws: x.start()
# spin until all NW threads are parked in wait()
for _ in range(400):
    with cond_n:
        if len(n_ready) == NW:
            break
    time.sleep(0.005)
time.sleep(0.05)
with cond_n:
    cond_n.notify(2)
time.sleep(0.1)
with n_woke_lock:
    woke_after_2 = len(n_woke)
chk("condition_notify_n_exact", woke_after_2 == 2, "woke=%d" % woke_after_2)
with cond_n:
    cond_n.notify_all()
for x in nws: x.join()
chk("condition_notify_all_remaining", len(n_woke) == NW)

# Condition methods require the lock held → RuntimeError otherwise.
cond4 = threading.Condition()
try:
    cond4.notify()
    chk("condition_notify_unlocked_raises", False)
except RuntimeError:
    chk("condition_notify_unlocked_raises", True)

# ---------------------------------------------------------------------------
# threading.Event — set()/clear()/wait()/is_set(). One-shot/reusable flag.
# 怎么测: waiter blocks on wait(); main set() releases it; clear() re-arms;
#   wait(timeout) returns False when not set.
# 期望: is_set tracks set/clear; wait() returns True once set, False on timeout.
# ---------------------------------------------------------------------------
ev = threading.Event()
chk("event_initially_unset", ev.is_set() is False)
chk("event_wait_timeout_false", ev.wait(timeout=0.03) is False)
ev_out = []
def _ev_waiter():
    ev_out.append(ev.wait(5))
ewt = threading.Thread(target=_ev_waiter)
ewt.start()
time.sleep(0.02)
ev.set()
ewt.join()
chk("event_set_releases_waiter", ev_out == [True])
chk("event_is_set_true", ev.is_set() is True)
chk("event_wait_when_set_immediate", ev.wait() is True)
ev.clear()
chk("event_clear", ev.is_set() is False)

# ---------------------------------------------------------------------------
# threading.Semaphore / BoundedSemaphore — counter-guarded resource access.
# 怎么测: Semaphore(2) allows 2 concurrent acquires, 3rd non-blocking fails;
#   BoundedSemaphore raises ValueError on over-release.
# 期望: acquire decrements, release increments; bounded caps the initial value.
# ---------------------------------------------------------------------------
sem = threading.Semaphore(2)
chk("sem_acquire_1", sem.acquire(blocking=False) is True)
chk("sem_acquire_2", sem.acquire(blocking=False) is True)
chk("sem_acquire_3_fails", sem.acquire(blocking=False) is False)
sem.release()
chk("sem_acquire_after_release", sem.acquire(blocking=False) is True)
sem.release(); sem.release()

# Semaphore caps real concurrency: with Semaphore(3), max concurrent <= 3.
sem3 = threading.Semaphore(3)
concurrency = {"cur": 0, "max": 0}
cmlock = threading.Lock()
def _limited():
    with sem3:
        with cmlock:
            concurrency["cur"] += 1
            concurrency["max"] = max(concurrency["max"], concurrency["cur"])
        time.sleep(0.02)
        with cmlock:
            concurrency["cur"] -= 1
lts = [threading.Thread(target=_limited) for _ in range(12)]
for x in lts: x.start()
for x in lts: x.join()
chk("sem_caps_concurrency", 1 <= concurrency["max"] <= 3, "max=%d" % concurrency["max"])

# BoundedSemaphore over-release → ValueError.
bs = threading.BoundedSemaphore(1)
bs.acquire()
bs.release()
try:
    bs.release()
    chk("bounded_sem_over_release_raises", False)
except ValueError:
    chk("bounded_sem_over_release_raises", True)
# Plain Semaphore over-release is allowed (no error) — increases the count.
# Use blocking=False so a broken (no-op) release surfaces as a FAIL, not a hang:
# after Semaphore(1)+release() the count is 2, so both non-blocking acquires win,
# and a third must fail (count exhausted).
ps = threading.Semaphore(1)
ps.release()  # count now 2, no error
chk("plain_sem_over_release_ok",
    ps.acquire(blocking=False) is True and ps.acquire(blocking=False) is True
    and ps.acquire(blocking=False) is False)

# ---------------------------------------------------------------------------
# threading.Barrier — parties rendezvous: wait() blocks until `parties` arrive,
#   then all release together; abort()/reset(); n_waiting; broken flag.
# 怎么测: 4 threads wait on Barrier(4); confirm all pass the barrier; one
#   wait() returns 0..parties-1 (unique index). Then test abort → BrokenBarrierError.
# 期望: parties==4; all four proceed only after the fourth arrives.
# ---------------------------------------------------------------------------
barrier = threading.Barrier(4)
chk("barrier_parties", barrier.parties == 4)
indices = []
idx_lock = threading.Lock()
order = []
def _bwait():
    i = barrier.wait()
    with idx_lock:
        indices.append(i)
        order.append("through")
bts = [threading.Thread(target=_bwait) for _ in range(4)]
for x in bts: x.start()
for x in bts: x.join()
chk("barrier_all_passed", len(order) == 4)
chk("barrier_unique_indices", sorted(indices) == [0, 1, 2, 3])
chk("barrier_not_broken", barrier.broken is False)

# Barrier action — callable run once by one thread when barrier trips.
action_count = {"n": 0}
def _act():
    action_count["n"] += 1
barrier_a = threading.Barrier(3, action=_act)
ats = [threading.Thread(target=barrier_a.wait) for _ in range(3)]
for x in ats: x.start()
for x in ats: x.join()
chk("barrier_action_once", action_count["n"] == 1)

# Barrier.abort → all current/future waits raise BrokenBarrierError until reset.
barrier_b = threading.Barrier(3)
abort_box = {"err": 0}
def _bwait_abort():
    try:
        barrier_b.wait(timeout=2)
    except threading.BrokenBarrierError:
        abort_box["err"] += 1
abts = [threading.Thread(target=_bwait_abort) for _ in range(2)]
for x in abts: x.start()
time.sleep(0.05)
chk("barrier_n_waiting", barrier_b.n_waiting == 2)
barrier_b.abort()
for x in abts: x.join()
chk("barrier_abort_broken", barrier_b.broken is True and abort_box["err"] == 2)
barrier_b.reset()
chk("barrier_reset_unbroken", barrier_b.broken is False)

# Barrier with too few arrivers and a timeout → BrokenBarrierError + broken.
barrier_to = threading.Barrier(5)
try:
    barrier_to.wait(timeout=0.05)
    chk("barrier_timeout_raises", False)
except threading.BrokenBarrierError:
    chk("barrier_timeout_raises", True)

# ---------------------------------------------------------------------------
# threading.Timer — Thread subclass that runs a function after `interval` secs;
#   cancel() stops it if still waiting.
# 怎么测: Timer fires and records; a cancelled Timer never fires.
# 期望: fired flag set after interval; cancelled timer leaves flag unset.
# ---------------------------------------------------------------------------
fired = {"v": False}
tm = threading.Timer(0.05, lambda: fired.__setitem__("v", True))
tm.start()
tm.join()
chk("timer_fires", fired["v"] is True)

not_fired = {"v": False}
tm2 = threading.Timer(5.0, lambda: not_fired.__setitem__("v", True))
tm2.start()
tm2.cancel()
tm2.join()
chk("timer_cancel", not_fired["v"] is False)

# ---------------------------------------------------------------------------
# threading.local — thread-local storage: each thread sees its own attributes.
# 怎么测: set attr in N worker threads to distinct values; each reads back its own
#   value; main thread sees none of them.
# 期望: per-thread isolation — no cross-thread leakage.
# ---------------------------------------------------------------------------
tls = threading.local()
tls_results = {}
tls_lock = threading.Lock()
def _tls_worker(n):
    tls.value = n
    time.sleep(0.005)
    with tls_lock:
        tls_results[n] = tls.value
tws = [threading.Thread(target=_tls_worker, args=(i,)) for i in range(6)]
for x in tws: x.start()
for x in tws: x.join()
chk("local_per_thread_isolation", tls_results == {i: i for i in range(6)})
chk("local_main_unset", not hasattr(tls, "value"))

# ---------------------------------------------------------------------------
# queue.Queue — FIFO thread-safe queue: put/get/qsize/empty/full/maxsize/
#   task_done/join. Bounded with maxsize blocks producers; join() waits for all
#   tasks done.
# 怎么测: producer/consumer with task_done+join; FIFO order; full/empty flags;
#   non-blocking get_nowait on empty → Empty; put_nowait on full → Full.
# 期望: items consumed in FIFO order; join() returns once all task_done called.
# ---------------------------------------------------------------------------
q = queue.Queue()
chk("queue_empty_initial", q.empty() is True and q.qsize() == 0)
chk("queue_maxsize_unbounded", q.maxsize == 0)
for i in range(5):
    q.put(i)
chk("queue_qsize", q.qsize() == 5 and q.empty() is False)
chk("queue_fifo_order", [q.get() for _ in range(5)] == [0, 1, 2, 3, 4])

# get_nowait on empty → queue.Empty.
try:
    q.get_nowait()
    chk("queue_get_nowait_empty", False)
except queue.Empty:
    chk("queue_get_nowait_empty", True)
# get(timeout=T) on an empty queue blocks ~T then raises queue.Empty (must not
# return early or hang): catches a broken blocking-get timeout.
t_ge = time.monotonic()
try:
    q.get(timeout=0.05)
    chk("queue_get_timeout_empty", False)
except queue.Empty:
    chk("queue_get_timeout_empty", (time.monotonic() - t_ge) >= 0.045)

# Bounded queue full behavior + put_nowait → queue.Full.
bq = queue.Queue(maxsize=2)
bq.put(1); bq.put(2)
chk("queue_full_flag", bq.full() is True)
try:
    bq.put_nowait(3)
    chk("queue_put_nowait_full", False)
except queue.Full:
    chk("queue_put_nowait_full", True)
# put(timeout) on a full queue → Full after timeout.
try:
    bq.put(3, timeout=0.03)
    chk("queue_put_timeout_full", False)
except queue.Full:
    chk("queue_put_timeout_full", True)
bq.get(); bq.get()

# task_done / join — producer/consumer drains a work queue, join unblocks.
wq = queue.Queue()
processed = []
proc_lock = threading.Lock()
def _qconsumer():
    while True:
        item = wq.get()
        if item is None:
            wq.task_done()
            break
        with proc_lock:
            processed.append(item)
        wq.task_done()
qc = threading.Thread(target=_qconsumer)
qc.start()
for i in range(20):
    wq.put(i)
wq.put(None)
wq.join()   # blocks until every task_done() balances every put()
qc.join()
chk("queue_task_done_join", sorted(processed) == list(range(20)) and wq.unfinished_tasks == 0)

# task_done() called more times than items put → ValueError (count would go < 0).
tdq = queue.Queue()
tdq.put("x"); tdq.get(); tdq.task_done()
try:
    tdq.task_done()
    chk("queue_task_done_over_call_raises", False)
except ValueError:
    chk("queue_task_done_over_call_raises", True)

# Blocking get across threads: consumer waits, producer feeds later.
bq2 = queue.Queue()
bq2_out = []
def _blocking_get():
    bq2_out.append(bq2.get(timeout=5))
bgt = threading.Thread(target=_blocking_get)
bgt.start()
time.sleep(0.02)
bq2.put("delivered")
bgt.join()
chk("queue_blocking_get", bq2_out == ["delivered"])

# ---------------------------------------------------------------------------
# queue.LifoQueue — LIFO (stack) ordering.
# 期望: items returned in reverse insertion order.
# ---------------------------------------------------------------------------
lq = queue.LifoQueue()
for i in range(5):
    lq.put(i)
chk("lifoqueue_order", [lq.get() for _ in range(5)] == [4, 3, 2, 1, 0])

# ---------------------------------------------------------------------------
# queue.PriorityQueue — lowest-valued entry retrieved first (heap order).
# 怎么测: insert unsorted (priority, payload) tuples; get yields ascending.
# 期望: smallest priority first regardless of insertion order.
# ---------------------------------------------------------------------------
pq = queue.PriorityQueue()
for pri in [5, 1, 3, 2, 4]:
    pq.put((pri, "item%d" % pri))
chk("priorityqueue_order", [pq.get()[0] for _ in range(5)] == [1, 2, 3, 4, 5])

# ---------------------------------------------------------------------------
# queue.SimpleQueue — unbounded, simpler/faster FIFO (no task_done/join,
#   no maxsize). put/get/get_nowait/empty/qsize.
# 期望: FIFO; get_nowait on empty raises queue.Empty.
# ---------------------------------------------------------------------------
sq = queue.SimpleQueue()
chk("simplequeue_empty", sq.empty() is True)
for i in range(4):
    sq.put(i)
chk("simplequeue_qsize", sq.qsize() == 4)
chk("simplequeue_fifo", [sq.get() for _ in range(4)] == [0, 1, 2, 3])
try:
    sq.get_nowait()
    chk("simplequeue_get_nowait_empty", False)
except queue.Empty:
    chk("simplequeue_get_nowait_empty", True)

# ---------------------------------------------------------------------------
# concurrent.futures.ThreadPoolExecutor.submit — schedules a callable, returns a
#   Future. Future: result()/exception()/done()/running()/cancel()/
#   add_done_callback().
# 怎么测: submit work, inspect Future state; success path result(); failure path
#   exception(); add_done_callback fires with the Future.
# 期望: result() returns the value; exception() returns the raised exception;
#   done() True after completion; callback invoked exactly once.
# ---------------------------------------------------------------------------
with ThreadPoolExecutor(max_workers=4) as ex:
    fut = ex.submit(pow, 2, 10)
    chk("future_result", fut.result(timeout=5) == 1024)
    chk("future_done", fut.done() is True)
    chk("future_exception_none", fut.exception(timeout=5) is None)
    chk("future_type", isinstance(fut, Future))

    def _boom():
        raise ValueError("kaboom")
    efut = ex.submit(_boom)
    exc = efut.exception(timeout=5)
    chk("future_exception_captured", isinstance(exc, ValueError) and str(exc) == "kaboom")
    try:
        efut.result(timeout=5)
        chk("future_result_reraises", False)
    except ValueError:
        chk("future_result_reraises", True)

    # add_done_callback — called with the future once it finishes.
    cb_box = {"called": 0, "arg": None}
    def _cb(f):
        cb_box["called"] += 1
        cb_box["arg"] = f.result()
    cfut = ex.submit(lambda: 777)
    cfut.add_done_callback(_cb)
    cfut.result(timeout=5)
    time.sleep(0.02)
    chk("future_add_done_callback", cb_box["called"] == 1 and cb_box["arg"] == 777)
    # callback added after completion still fires.
    cb2 = {"called": 0}
    cfut.add_done_callback(lambda f: cb2.__setitem__("called", cb2["called"] + 1))
    chk("future_callback_after_done", cb2["called"] == 1)

    # A callback that raises is logged (not propagated) and must NOT prevent a
    # subsequently-added callback from running. Both are added after completion so
    # they fire synchronously, in order; suppress the logged traceback noise.
    import logging
    logging.getLogger("concurrent.futures").addHandler(logging.NullHandler())
    logging.getLogger("concurrent.futures").propagate = False
    cb3 = {"after": 0}
    def _raise_cb(f):
        raise RuntimeError("callback boom")
    cfut.add_done_callback(_raise_cb)
    cfut.add_done_callback(lambda f: cb3.__setitem__("after", 1))
    chk("future_callback_exception_isolated", cb3["after"] == 1)

# ThreadPoolExecutor.map — applies fn over iterables, results in input order.
with ThreadPoolExecutor(max_workers=4) as ex:
    mapped = list(ex.map(lambda x: x * x, range(8)))
    chk("executor_map_ordered", mapped == [0, 1, 4, 9, 16, 25, 36, 49])
    # map over multiple iterables.
    summed = list(ex.map(lambda a, b: a + b, [1, 2, 3], [10, 20, 30]))
    chk("executor_map_multi_iter", summed == [11, 22, 33])
    # map propagates the first exception when its result is consumed.
    def _maybe(x):
        if x == 2:
            raise RuntimeError("bad")
        return x
    gen = ex.map(_maybe, range(5))
    try:
        list(gen)
        chk("executor_map_raises", False)
    except RuntimeError:
        chk("executor_map_raises", True)

# map(timeout=...) → TimeoutError if results not ready in time.
with ThreadPoolExecutor(max_workers=1) as ex:
    slow = ex.map(lambda x: (time.sleep(0.2), x)[1], range(3), timeout=0.01)
    try:
        list(slow)
        chk("executor_map_timeout", False)
    except FTimeoutError:
        chk("executor_map_timeout", True)

# ---------------------------------------------------------------------------
# concurrent.futures.as_completed — yields futures as they finish (any order).
# 怎么测: submit jobs with staggered sleeps; collect results via as_completed.
# 期望: all results present; the set of results equals the submitted set.
# ---------------------------------------------------------------------------
with ThreadPoolExecutor(max_workers=4) as ex:
    delays = {0.04: "a", 0.01: "b", 0.03: "c", 0.02: "d"}
    futs = [ex.submit(lambda d=d, v=v: (time.sleep(d), v)[1], ) for d, v in delays.items()]
    collected = [f.result() for f in as_completed(futs, timeout=5)]
    chk("as_completed_all", sorted(collected) == ["a", "b", "c", "d"])
    chk("as_completed_count", len(collected) == 4)

# ---------------------------------------------------------------------------
# concurrent.futures.wait — blocks until futures satisfy return_when condition;
#   returns (done, not_done) sets. ALL_COMPLETED / FIRST_COMPLETED / FIRST_EXCEPTION.
# 怎么测: ALL_COMPLETED waits for everything; FIRST_EXCEPTION returns as soon as
#   one future raises; FIRST_COMPLETED returns after the earliest completes.
# 期望: done/not_done partition correctly per policy.
# ---------------------------------------------------------------------------
with ThreadPoolExecutor(max_workers=4) as ex:
    fs = [ex.submit(lambda i=i: i * 2) for i in range(5)]
    done, not_done = wait(fs, timeout=5, return_when=ALL_COMPLETED)
    chk("wait_all_completed", len(done) == 5 and len(not_done) == 0)
    chk("wait_all_results", sorted(f.result() for f in done) == [0, 2, 4, 6, 8])

with ThreadPoolExecutor(max_workers=4) as ex:
    def _slow_ok(d):
        time.sleep(d)
        return "ok"
    def _fast_raise():
        raise ValueError("first-exc")
    fs2 = [ex.submit(_slow_ok, 1.0), ex.submit(_slow_ok, 1.0), ex.submit(_fast_raise)]
    done, not_done = wait(fs2, timeout=5, return_when=FIRST_EXCEPTION)
    raised = [f for f in done if f.exception() is not None]
    chk("wait_first_exception", len(raised) == 1
        and isinstance(raised[0].exception(), ValueError))

with ThreadPoolExecutor(max_workers=4) as ex:
    fs3 = [ex.submit(lambda d=d: (time.sleep(d), d)[1]) for d in (0.5, 0.5, 0.01)]
    done, not_done = wait(fs3, timeout=5, return_when=FIRST_COMPLETED)
    chk("wait_first_completed", len(done) >= 1)
    # drain remaining so the executor shuts down cleanly
    wait(fs3, timeout=5, return_when=ALL_COMPLETED)

# wait(timeout) returns partial sets when not everything finishes in time.
with ThreadPoolExecutor(max_workers=1) as ex:
    fs4 = [ex.submit(lambda d=d: (time.sleep(d), d)[1]) for d in (0.01, 0.5)]
    done, not_done = wait(fs4, timeout=0.1, return_when=ALL_COMPLETED)
    chk("wait_timeout_partial", len(done) >= 1 and len(not_done) >= 1)
    wait(fs4, timeout=5)

# ---------------------------------------------------------------------------
# Executor.shutdown(wait=True) — context-manager exit waits for pending work;
#   submitting after shutdown raises RuntimeError.
# 怎么测: explicit shutdown(wait=True) then submit → RuntimeError; verify the
#   submitted work all ran (results recorded) by shutdown completion.
# 期望: all queued jobs complete before shutdown returns; post-shutdown submit fails.
# ---------------------------------------------------------------------------
ex2 = ThreadPoolExecutor(max_workers=3)
done_results = []
dr_lock = threading.Lock()
def _record(i):
    time.sleep(0.01)
    with dr_lock:
        done_results.append(i)
    return i
sub = [ex2.submit(_record, i) for i in range(9)]
ex2.shutdown(wait=True)
chk("shutdown_waits_all", sorted(done_results) == list(range(9)))
chk("shutdown_futures_done", all(f.done() for f in sub))
try:
    ex2.submit(lambda: 1)
    chk("submit_after_shutdown_raises", False)
except RuntimeError:
    chk("submit_after_shutdown_raises", True)

# Future.cancel — only cancellable while still pending (PENDING); a future that
# already started/finished cannot be cancelled.
ex3 = ThreadPoolExecutor(max_workers=1)
block = threading.Event()
running_now = threading.Event()
def _hog():
    running_now.set()
    block.wait(5)
hog_fut = ex3.submit(_hog)        # occupies the single worker
running_now.wait(5)
pending_fut = ex3.submit(lambda: 1)  # queued, still PENDING
# Direct RUNNING-state contract: the hog is executing (blocked in block.wait),
# so running() is True and done() is False; the queued one is neither.
chk("future_running_state", hog_fut.running() is True and hog_fut.done() is False)
chk("future_pending_not_running", pending_fut.running() is False)
chk("future_cancel_pending", pending_fut.cancel() is True)
chk("future_cancelled_flag", pending_fut.cancelled() is True)
chk("future_running_not_cancellable", hog_fut.cancel() is False)
block.set()
ex3.shutdown(wait=True)
chk("future_running_done_after", hog_fut.done() is True)

# ThreadPoolExecutor thread_name_prefix — worker threads carry the prefix.
names = set()
nm_lock = threading.Lock()
def _capture_name():
    with nm_lock:
        names.add(threading.current_thread().name)
with ThreadPoolExecutor(max_workers=2, thread_name_prefix="pool") as ex:
    list(ex.map(lambda _: _capture_name(), range(6)))
chk("executor_thread_name_prefix", all(n.startswith("pool") for n in names) and len(names) >= 1)

# ThreadPoolExecutor.map with chunksize arg accepted (no-op for threads).
with ThreadPoolExecutor(max_workers=2) as ex:
    out = list(ex.map(lambda x: x + 1, range(5), chunksize=2))
    chk("executor_map_chunksize", out == [1, 2, 3, 4, 5])

# ---------------------------------------------------------------------------
# Cross-thread correctness stress: producer/consumer over a bounded Queue with
# multiple producers and consumers, verifying no items are lost or duplicated.
# 期望: every produced item consumed exactly once (sum invariant holds).
# ---------------------------------------------------------------------------
job_q = queue.Queue(maxsize=10)
result_q = queue.Queue()
N_ITEMS = 200
def _producer(start, count):
    for i in range(start, start + count):
        job_q.put(i)
def _consumer2():
    while True:
        item = job_q.get()
        if item is None:
            job_q.task_done()
            break
        result_q.put(item)
        job_q.task_done()
producers = [threading.Thread(target=_producer, args=(p * 50, 50)) for p in range(4)]
consumers = [threading.Thread(target=_consumer2) for _ in range(3)]
for c in consumers: c.start()
for p in producers: p.start()
for p in producers: p.join()
job_q.join()
for _ in consumers:
    job_q.put(None)
for c in consumers: c.join()
drained = []
while not result_q.empty():
    drained.append(result_q.get())
chk("multi_producer_consumer", sorted(drained) == list(range(N_ITEMS)),
    "n=%d" % len(drained))

print(("PY_THREADS_OK") if _ok else ("PY_THREADS_FAIL"))
sys.exit(0 if _ok else 1)
