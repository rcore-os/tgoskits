#!/usr/bin/env python3
"""asyncio & coroutines deep surface — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False

import asyncio

# =====================================================================
# Coroutine objects & the `async def`/`await` protocol
# Ref: Language Reference 8.8.1 "Coroutines" / "Coroutine objects".
#   how: build a coroutine object without running it, inspect its type, then run.
#   expected: `async def` returns a coroutine; awaiting it yields the return value;
#             a coroutine is a single-shot awaitable (re-running raises RuntimeError).
#   why: the await machinery underpins every asyncio API; verify the bare protocol.
# =====================================================================
import types
import inspect

async def _coro_ret(x):
    return x * 2

_c = _coro_ret(21)
chk("coro_is_coroutine", inspect.iscoroutine(_c))
chk("coro_type", isinstance(_c, types.CoroutineType))
chk("coro_not_started", asyncio.run(_c) == 42)
# A consumed coroutine cannot be awaited/run again.
try:
    asyncio.run(_c)
    _reuse = False
except RuntimeError:
    _reuse = True
chk("coro_single_shot", _reuse)

# iscoroutinefunction distinguishes `async def` from plain functions.
def _plain():
    return 1
chk("iscoroutinefunction", asyncio.iscoroutinefunction(_coro_ret)
    and not asyncio.iscoroutinefunction(_plain))

# =====================================================================
# asyncio.run — top-level entry point
# Ref: asyncio.run(coro, *, debug=None).
#   how: run a coroutine returning a value; confirm a fresh loop each call;
#        confirm passing a non-coroutine raises ValueError.
#   expected: returns the coroutine's result; creates+closes its own loop.
#   why: canonical program entry; must isolate event loops correctly.
# =====================================================================
async def _identity(v):
    return v
chk("run_returns_value", asyncio.run(_identity("hi")) == "hi")
try:
    asyncio.run(123)  # not a coroutine
    _run_bad = False
except (ValueError, TypeError):
    _run_bad = True
chk("run_rejects_non_coro", _run_bad)
# Two separate run() calls each build and tear down a loop -> different objects.
async def _grab_loop():
    return asyncio.get_running_loop()
_l1 = asyncio.run(_grab_loop())
_l2 = asyncio.run(_grab_loop())
chk("run_fresh_loop_each", _l1 is not _l2 and _l1.is_closed() and _l2.is_closed())

# =====================================================================
# get_running_loop / get_event_loop inside a coroutine
# Ref: asyncio.get_running_loop() — only valid while a loop runs.
#   how: call inside a running coroutine vs. from the main thread (no loop).
#   expected: returns the running loop inside; raises RuntimeError outside.
#   why: library code must locate the active loop safely.
# =====================================================================
async def _loop_present():
    lp = asyncio.get_running_loop()
    return lp.is_running()
chk("get_running_loop_inside", asyncio.run(_loop_present()))
try:
    asyncio.get_running_loop()
    _no_loop = False
except RuntimeError:
    _no_loop = True
chk("get_running_loop_outside_raises", _no_loop)

# =====================================================================
# create_task + await — concurrent scheduling
# Ref: loop.create_task / asyncio.create_task(coro, *, name=None).
#   how: spawn tasks, await them; check Task type, name, done()/result().
#   expected: tasks run concurrently on the loop; awaiting yields results.
#   why: create_task is THE primitive for fan-out concurrency.
# =====================================================================
async def _task_basics():
    async def work(n):
        await asyncio.sleep(0)
        return n * n
    t = asyncio.create_task(work(7), name="sq7")
    chk("create_task_type", isinstance(t, asyncio.Task))
    chk("task_get_name", t.get_name() == "sq7")
    chk("task_not_done_yet", not t.done())
    r = await t
    chk("task_await_result", r == 49)
    chk("task_done", t.done() and t.result() == 49)
    # For a normal (non-eager) Task, get_coro() returns the wrapped coroutine
    # object even after completion (None is only returned for eagerly-completed
    # tasks created via eager_task_factory). Assert the coroutine identity/type
    # strictly so a divergence (e.g. dropping the ref) is caught.
    _gc = t.get_coro()
    chk("task_get_coro", inspect.iscoroutine(_gc) and isinstance(_gc, types.CoroutineType))
    # set_name is mutable
    t2 = asyncio.create_task(work(2))
    t2.set_name("renamed")
    chk("task_set_name", t2.get_name() == "renamed")
    await t2
asyncio.run(_task_basics())

# =====================================================================
# ensure_future — wrap coroutine/future
# Ref: asyncio.ensure_future(obj).
#   how: wrap a coroutine -> Task; pass an existing Task -> returns it unchanged.
#   expected: coroutine becomes a scheduled Task; Task passthrough is identity.
#   why: APIs that accept "awaitable or future" rely on this normalization.
# =====================================================================
async def _ensure_future():
    async def c():
        return 5
    fut = asyncio.ensure_future(c())
    chk("ensure_future_makes_task", isinstance(fut, asyncio.Task))
    chk("ensure_future_result", (await fut) == 5)
    t = asyncio.create_task(c())
    chk("ensure_future_passthrough", asyncio.ensure_future(t) is t)
    await t
asyncio.run(_ensure_future())

# =====================================================================
# asyncio.sleep — yielding & ordering
# Ref: asyncio.sleep(delay, result=None).
#   how: sleep(0) yields control without delay; sleep returns its `result` arg;
#        interleave two tasks with sleep(0) and observe round-robin ordering.
#   expected: sleep(0) is a pure yield point; ordering is deterministic FIFO.
#   why: sleep(0) is the standard cooperative-yield idiom.
# =====================================================================
async def _sleep_result():
    return await asyncio.sleep(0, result="done")
chk("sleep_returns_result", asyncio.run(_sleep_result()) == "done")

async def _sleep_ordering():
    seq = []
    async def step(label):
        for i in range(3):
            seq.append((label, i))
            await asyncio.sleep(0)
    await asyncio.gather(step("A"), step("B"))
    return seq
_seq = asyncio.run(_sleep_ordering())
# With sleep(0) round-robin the two tasks interleave: A0,B0,A1,B1,A2,B2.
chk("sleep0_round_robin",
    _seq == [("A", 0), ("B", 0), ("A", 1), ("B", 1), ("A", 2), ("B", 2)],
    repr(_seq))

# =====================================================================
# asyncio.gather — aggregate awaitables, ordered results
# Ref: asyncio.gather(*aws, return_exceptions=False).
#   how: gather several coroutines; check result order matches arg order even
#        when completion order differs; test return_exceptions True vs False.
#   expected: results preserve argument order; with return_exceptions=False the
#             first raised exception propagates; =True collects exceptions inline.
#   why: gather is the most-used aggregation primitive.
# =====================================================================
async def _gather_order():
    async def d(val, n):
        for _ in range(n):
            await asyncio.sleep(0)
        return val
    # arg order: first sleeps longest, yet result order must follow args.
    return await asyncio.gather(d("a", 3), d("b", 1), d("c", 2))
chk("gather_preserves_order", asyncio.run(_gather_order()) == ["a", "b", "c"])

async def _gather_raise():
    async def boom():
        await asyncio.sleep(0)
        raise ValueError("x")
    async def fine():
        await asyncio.sleep(0)
        return 1
    try:
        await asyncio.gather(fine(), boom())
        return "noraise"
    except ValueError as e:
        return str(e)
chk("gather_propagates_first_exc", asyncio.run(_gather_raise()) == "x")

async def _gather_collect():
    async def boom():
        raise KeyError("k")
    async def fine():
        return 9
    res = await asyncio.gather(fine(), boom(), return_exceptions=True)
    return res
_gc = asyncio.run(_gather_collect())
chk("gather_return_exceptions", _gc[0] == 9 and isinstance(_gc[1], KeyError))

# =====================================================================
# asyncio.wait — partial completion (FIRST_COMPLETED / ALL_COMPLETED)
# Ref: asyncio.wait(aws, *, timeout=None, return_when=...).
#   how: wait on several tasks with FIRST_COMPLETED, then ALL_COMPLETED;
#        check the (done, pending) partition; check FIRST_EXCEPTION.
#   expected: returns two sets; FIRST_COMPLETED returns as soon as one done;
#             ALL_COMPLETED leaves pending empty.
#   why: wait gives fine-grained control over which tasks to harvest.
# =====================================================================
async def _wait_first():
    async def quick():
        await asyncio.sleep(0)
        return "q"
    async def slow():
        for _ in range(20):
            await asyncio.sleep(0)
        return "s"
    tq = asyncio.create_task(quick())
    ts = asyncio.create_task(slow())
    done, pending = await asyncio.wait({tq, ts}, return_when=asyncio.FIRST_COMPLETED)
    ok_first = tq in done and ts in pending
    ts.cancel()
    try:
        await ts
    except asyncio.CancelledError:
        pass
    return ok_first
chk("wait_first_completed", asyncio.run(_wait_first()))

async def _wait_all():
    async def w(n):
        await asyncio.sleep(0)
        return n
    tasks = {asyncio.create_task(w(i)) for i in range(4)}
    done, pending = await asyncio.wait(tasks, return_when=asyncio.ALL_COMPLETED)
    return len(done) == 4 and len(pending) == 0 and {t.result() for t in done} == {0, 1, 2, 3}
chk("wait_all_completed", asyncio.run(_wait_all()))

async def _wait_first_exc():
    async def good():
        for _ in range(10):
            await asyncio.sleep(0)
        return 1
    async def bad():
        await asyncio.sleep(0)
        raise RuntimeError("boom")
    tg = asyncio.create_task(good())
    tb = asyncio.create_task(bad())
    done, pending = await asyncio.wait({tg, tb}, return_when=asyncio.FIRST_EXCEPTION)
    found_exc = any(t.done() and t.exception() is not None for t in done)
    for t in pending:
        t.cancel()
    for t in pending:
        try:
            await t
        except asyncio.CancelledError:
            pass
    return found_exc
chk("wait_first_exception", asyncio.run(_wait_first_exc()))

# =====================================================================
# asyncio.wait_for — apply a timeout to one awaitable
# Ref: asyncio.wait_for(aw, timeout).
#   how: wait_for a fast coroutine within budget -> result; wait_for a slow
#        coroutine past a tiny timeout -> TimeoutError, inner cancelled.
#   expected: returns result if in time; raises TimeoutError otherwise.
#   why: per-operation deadlines are essential for robustness.
# =====================================================================
async def _wait_for_ok():
    async def q():
        await asyncio.sleep(0)
        return "fast"
    return await asyncio.wait_for(q(), timeout=5)
chk("wait_for_in_time", asyncio.run(_wait_for_ok()) == "fast")

async def _wait_for_timeout():
    async def slow():
        await asyncio.sleep(10)
        return "never"
    try:
        await asyncio.wait_for(slow(), timeout=0.01)
        return "no_timeout"
    except asyncio.TimeoutError:
        return "timeout"
chk("wait_for_timeout", asyncio.run(_wait_for_timeout()) == "timeout")
# TimeoutError is an alias of builtins.TimeoutError since 3.11.
chk("timeouterror_is_builtin", asyncio.TimeoutError is TimeoutError)

# =====================================================================
# asyncio.timeout — context-manager timeout (3.11+)
# Ref: asyncio.timeout(delay) async context manager.
#   how: wrap a slow await; expect TimeoutError on exit; also test a fast
#        body that completes within budget; inspect expired().
#   expected: body exceeding delay raises TimeoutError at the `async with` exit.
#   why: ergonomic structured timeout for whole code blocks.
# =====================================================================
if hasattr(asyncio, "timeout"):
    async def _timeout_ctx():
        try:
            async with asyncio.timeout(0.01):
                await asyncio.sleep(10)
            return "no_timeout"
        except asyncio.TimeoutError:
            return "timeout"
    chk("asyncio_timeout_ctx", asyncio.run(_timeout_ctx()) == "timeout")

    async def _timeout_ok():
        async with asyncio.timeout(5) as cm:
            await asyncio.sleep(0)
        return cm.expired()
    chk("asyncio_timeout_not_expired", asyncio.run(_timeout_ok()) is False)

    async def _timeout_at():
        # timeout_at uses an absolute loop time.
        loop = asyncio.get_running_loop()
        try:
            async with asyncio.timeout_at(loop.time() + 0.01):
                await asyncio.sleep(10)
            return "no"
        except asyncio.TimeoutError:
            return "yes"
    chk("asyncio_timeout_at", asyncio.run(_timeout_at()) == "yes")
else:
    chk("asyncio_timeout_ctx", True, "(skip: needs 3.11)")
    chk("asyncio_timeout_not_expired", True, "(skip: needs 3.11)")
    chk("asyncio_timeout_at", True, "(skip: needs 3.11)")

# =====================================================================
# asyncio.shield — protect an awaitable from outer cancellation
# Ref: asyncio.shield(aw).
#   how: shield an inner task while the awaiting wait_for times out; verify the
#        inner coroutine keeps running (is not cancelled by the timeout).
#   expected: the outer wait_for raises TimeoutError but the shielded inner
#             task continues and can still finish.
#   why: shield decouples a critical operation from caller cancellation.
# =====================================================================
async def _shield():
    done_flag = {"v": False}
    async def inner():
        await asyncio.sleep(0.05)
        done_flag["v"] = True
        return "inner"
    inner_task = asyncio.create_task(inner())
    try:
        await asyncio.wait_for(asyncio.shield(inner_task), timeout=0.01)
    except asyncio.TimeoutError:
        pass
    # Inner was shielded -> still alive; await it to completion.
    res = await inner_task
    return res == "inner" and done_flag["v"]
chk("shield_protects_inner", asyncio.run(_shield()))

# =====================================================================
# asyncio.to_thread — run blocking sync code off the loop (3.9+)
# Ref: asyncio.to_thread(func, /, *args, **kwargs).
#   how: offload a blocking function with positional + keyword args.
#   expected: returns the function's result; runs in a worker thread.
#   why: integrates blocking/CPU-light sync calls without stalling the loop.
# =====================================================================
if hasattr(asyncio, "to_thread"):
    import threading as _thr
    async def _to_thread():
        def blocking(a, b, *, mul):
            return (a + b) * mul, _thr.current_thread() is not _thr.main_thread()
        val, off_main = await asyncio.to_thread(blocking, 2, 3, mul=10)
        return val == 50 and off_main
    chk("to_thread", asyncio.run(_to_thread()))
else:
    chk("to_thread", True, "(skip: needs 3.9)")

# =====================================================================
# asyncio.TaskGroup — structured concurrency (3.11+)
# Ref: asyncio.TaskGroup() async context manager.
#   how: spawn several tasks within a group, all complete; then a group where
#        two tasks raise -> aggregated into an ExceptionGroup; sibling tasks
#        get cancelled.
#   expected: `async with TaskGroup()` waits for all children; failures surface
#             as an ExceptionGroup; remaining tasks are cancelled.
#   why: the modern, leak-free way to manage concurrent tasks.
# =====================================================================
if hasattr(asyncio, "TaskGroup"):
    async def _tg_ok():
        results = []
        async def work(n):
            await asyncio.sleep(0)
            results.append(n)
            return n
        async with asyncio.TaskGroup() as tg:
            tasks = [tg.create_task(work(i)) for i in range(5)]
        # Each child task must have completed with its input index as result,
        # in submission order (tasks[i] runs work(i) -> returns i).
        return (sorted(results) == [0, 1, 2, 3, 4]
                and [t.result() for t in tasks] == [0, 1, 2, 3, 4])
    chk("taskgroup_all_complete", asyncio.run(_tg_ok()))

    async def _tg_errors():
        cancelled = {"v": 0}
        async def boom(code):
            await asyncio.sleep(0)
            raise ValueError(code)
        async def victim():
            try:
                await asyncio.sleep(10)
            except asyncio.CancelledError:
                cancelled["v"] += 1
                raise
        try:
            async with asyncio.TaskGroup() as tg:
                tg.create_task(boom("a"))
                tg.create_task(boom("b"))
                tg.create_task(victim())
            return "no_exc"
        except BaseExceptionGroup as eg:
            vals = sorted(str(e) for e in eg.exceptions if isinstance(e, ValueError))
            return (vals, cancelled["v"])
    _tge = asyncio.run(_tg_errors())
    chk("taskgroup_exception_group", _tge == (["a", "b"], 1), repr(_tge))
else:
    chk("taskgroup_all_complete", True, "(skip: needs 3.11)")
    chk("taskgroup_exception_group", True, "(skip: needs 3.11)")

# =====================================================================
# current_task / all_tasks — introspection
# Ref: asyncio.current_task() / asyncio.all_tasks().
#   how: from within a task, fetch current_task and the set of all tasks.
#   expected: current_task returns the running Task; all_tasks includes it;
#             outside any task current_task() is None.
#   why: schedulers/observability need to introspect live tasks.
# =====================================================================
async def _introspect():
    cur = asyncio.current_task()
    allt = asyncio.all_tasks()
    return cur is not None and cur in allt
chk("current_and_all_tasks", asyncio.run(_introspect()))
# Outside any running task, current_task() requires (and uses) a loop; with no
# running loop at all, asking for current_task raises RuntimeError. We check the
# in-loop-but-not-in-a-task semantics via run_until_complete on a bare future:
def _current_task_no_task():
    loop = asyncio.new_event_loop()
    try:
        # current_task(loop) returns None when no Task is executing on that loop.
        return asyncio.current_task(loop) is None
    finally:
        loop.close()
chk("current_task_none_when_no_task", _current_task_no_task())

# =====================================================================
# asyncio.Lock — mutual exclusion
# Ref: asyncio.Lock; acquire()/release()/locked(); `async with`.
#   how: guard a shared counter across concurrent tasks; check locked() state;
#        verify mutual exclusion via a critical-section interleaving probe.
#   expected: only one task holds the lock at a time; locked() reflects state.
#   why: the fundamental async mutex.
# =====================================================================
async def _lock():
    lock = asyncio.Lock()
    chk("lock_initially_unlocked", not lock.locked())
    in_cs = {"max": 0, "cur": 0}
    async def crit():
        async with lock:
            in_cs["cur"] += 1
            in_cs["max"] = max(in_cs["max"], in_cs["cur"])
            await asyncio.sleep(0)
            in_cs["cur"] -= 1
    await asyncio.gather(*(crit() for _ in range(5)))
    chk("lock_mutual_exclusion", in_cs["max"] == 1)
    # manual acquire/release
    await lock.acquire()
    chk("lock_locked_after_acquire", lock.locked())
    lock.release()
    chk("lock_unlocked_after_release", not lock.locked())
asyncio.run(_lock())

# =====================================================================
# asyncio.Event — broadcast signaling
# Ref: asyncio.Event; set()/clear()/is_set()/wait().
#   how: have waiters block on wait(), then set() to release all; clear resets.
#   expected: wait() blocks until set(); is_set() tracks the flag; clear()
#             makes subsequent wait() block again.
#   why: one-to-many notification primitive.
# =====================================================================
async def _event():
    ev = asyncio.Event()
    chk("event_initially_clear", not ev.is_set())
    woke = []
    async def waiter(i):
        await ev.wait()
        woke.append(i)
    waiters = [asyncio.create_task(waiter(i)) for i in range(3)]
    await asyncio.sleep(0)  # let them block
    ev.set()
    await asyncio.gather(*waiters)
    chk("event_set_releases_all", sorted(woke) == [0, 1, 2] and ev.is_set())
    ev.clear()
    chk("event_clear", not ev.is_set())
    # wait() returns immediately when already set
    ev.set()
    chk("event_wait_when_set", (await ev.wait()) is True)
asyncio.run(_event())

# =====================================================================
# asyncio.Semaphore / BoundedSemaphore — bounded concurrency
# Ref: asyncio.Semaphore(value); BoundedSemaphore.
#   how: limit concurrent entrants to N and observe the high-water mark;
#        BoundedSemaphore raises ValueError on over-release.
#   expected: at most `value` tasks inside concurrently; bounded variant guards
#             release count.
#   why: rate/concurrency limiting.
# =====================================================================
async def _semaphore():
    sem = asyncio.Semaphore(2)
    state = {"cur": 0, "max": 0}
    async def task():
        async with sem:
            state["cur"] += 1
            state["max"] = max(state["max"], state["cur"])
            await asyncio.sleep(0)
            await asyncio.sleep(0)
            state["cur"] -= 1
    await asyncio.gather(*(task() for _ in range(6)))
    chk("semaphore_limits_concurrency", state["max"] <= 2 and state["max"] >= 1, repr(state))

    bs = asyncio.BoundedSemaphore(1)
    await bs.acquire()
    bs.release()
    try:
        bs.release()  # over-release
        over = False
    except ValueError:
        over = True
    chk("bounded_semaphore_over_release", over)
asyncio.run(_semaphore())

# =====================================================================
# asyncio.Condition — wait/notify on a predicate
# Ref: asyncio.Condition; acquire()/wait()/wait_for()/notify()/notify_all().
#   how: a consumer wait_for(predicate); a producer flips the predicate and
#        notifies; verify the consumer wakes with the predicate true.
#   expected: wait_for blocks until predicate true after notify; notify_all
#             wakes all waiters.
#   why: classic producer/consumer coordination.
# =====================================================================
async def _condition():
    cond = asyncio.Condition()
    shared = {"ready": False, "items": []}
    async def consumer(i):
        async with cond:
            await cond.wait_for(lambda: shared["ready"])
            shared["items"].append(i)
    consumers = [asyncio.create_task(consumer(i)) for i in range(3)]
    await asyncio.sleep(0)
    async with cond:
        shared["ready"] = True
        cond.notify_all()
    await asyncio.gather(*consumers)
    return sorted(shared["items"]) == [0, 1, 2]
chk("condition_wait_for_notify_all", asyncio.run(_condition()))

# =====================================================================
# asyncio.Barrier — synchronization rendezvous (3.11+)
# Ref: asyncio.Barrier(parties); wait(); n_waiting; parties.
#   how: N tasks reach the barrier; all proceed only after the Nth arrives.
#   expected: all parties pass together; wait() returns a distinct index 0..N-1.
#   why: phase synchronization across tasks.
# =====================================================================
if hasattr(asyncio, "Barrier"):
    async def _barrier():
        bar = asyncio.Barrier(3)
        order = []
        async def party(i):
            idx = await bar.wait()
            order.append(idx)
            return idx
        idxs = await asyncio.gather(party(0), party(1), party(2))
        return bar.parties == 3 and sorted(idxs) == [0, 1, 2]
    chk("barrier_rendezvous", asyncio.run(_barrier()))
else:
    chk("barrier_rendezvous", True, "(skip: needs 3.11)")

# =====================================================================
# asyncio.Queue — producer/consumer FIFO
# Ref: asyncio.Queue(maxsize); put/get/put_nowait/get_nowait/join/task_done;
#      qsize/empty/full.
#   how: bounded queue with producers and consumers; join() blocks until every
#        put item is task_done()'d; check empty/full/qsize; nowait variants
#        raise QueueFull/QueueEmpty.
#   expected: FIFO order; join unblocks after all task_done; nowait raises.
#   why: the standard async work-distribution structure.
# =====================================================================
async def _queue():
    q = asyncio.Queue(maxsize=2)
    chk("queue_empty_initial", q.empty() and q.qsize() == 0 and not q.full())
    await q.put(1)
    q.put_nowait(2)
    chk("queue_full", q.full() and q.qsize() == 2)
    try:
        q.put_nowait(3)
        qf = False
    except asyncio.QueueFull:
        qf = True
    chk("queue_put_nowait_full", qf)
    chk("queue_get_fifo", (await q.get()) == 1)
    chk("queue_get_nowait", q.get_nowait() == 2)
    try:
        q.get_nowait()
        qe = False
    except asyncio.QueueEmpty:
        qe = True
    chk("queue_get_nowait_empty", qe)

async def _queue_join():
    q = asyncio.Queue()
    processed = []
    async def consumer():
        while True:
            item = await q.get()
            processed.append(item)
            q.task_done()
    cons = asyncio.create_task(consumer())
    for i in range(5):
        await q.put(i)
    await q.join()  # blocks until all task_done called
    cons.cancel()
    try:
        await cons
    except asyncio.CancelledError:
        pass
    return processed == [0, 1, 2, 3, 4]
asyncio.run(_queue())
chk("queue_join_task_done", asyncio.run(_queue_join()))

# PriorityQueue / LifoQueue variants
async def _queue_variants():
    pq = asyncio.PriorityQueue()
    for v in (3, 1, 2):
        await pq.put(v)
    ordered = [await pq.get() for _ in range(3)]
    lq = asyncio.LifoQueue()
    for v in (1, 2, 3):
        await lq.put(v)
    lifo = [await lq.get() for _ in range(3)]
    return ordered == [1, 2, 3] and lifo == [3, 2, 1]
chk("queue_priority_and_lifo", asyncio.run(_queue_variants()))

# =====================================================================
# Async generators — asend / athrow / aclose
# Ref: Language Reference "Asynchronous generator functions"; PEP 525.
#   how: drive an async generator manually via __anext__/asend; inject an
#        exception with athrow; close it with aclose and confirm exhaustion.
#   expected: asend resumes at the yield with the sent value; athrow raises
#             inside the generator; aclose finalizes it (StopAsyncIteration next).
#   why: async generators power streaming/pipelines.
# =====================================================================
async def _agen_asend():
    async def ag():
        received = []
        while True:
            x = yield len(received)
            received.append(x)
    g = ag()
    first = await g.__anext__()        # prime -> yields 0
    r1 = await g.asend("a")            # resumes, yields 1
    r2 = await g.asend("b")            # yields 2
    await g.aclose()
    return first == 0 and r1 == 1 and r2 == 2
chk("agen_asend", asyncio.run(_agen_asend()))

async def _agen_athrow():
    caught = {"v": None}
    async def ag():
        try:
            yield 1
            yield 2
        except ValueError as e:
            caught["v"] = str(e)
            yield 99
    g = ag()
    a = await g.__anext__()           # 1
    b = await g.athrow(ValueError("injected"))  # caught -> yields 99
    await g.aclose()
    return a == 1 and b == 99 and caught["v"] == "injected"
chk("agen_athrow", asyncio.run(_agen_athrow()))

async def _agen_aclose():
    cleaned = {"v": False}
    async def ag():
        try:
            yield 1
            yield 2
        finally:
            cleaned["v"] = True
    g = ag()
    await g.__anext__()
    await g.aclose()
    # after aclose, anext raises StopAsyncIteration
    try:
        await g.__anext__()
        stopped = False
    except StopAsyncIteration:
        stopped = True
    return cleaned["v"] and stopped
chk("agen_aclose_finally", asyncio.run(_agen_aclose()))

# =====================================================================
# Async comprehensions & `async for`
# Ref: PEP 530 — asynchronous comprehensions; "async for" statement.
#   how: build list/set/dict comprehensions over an async iterator; use a bare
#        `async for` loop; verify ordering & contents.
#   expected: comprehensions consume the async iterator fully in order.
#   why: idiomatic collection from async streams.
# =====================================================================
async def _async_comprehensions():
    async def asrc(n):
        for i in range(n):
            await asyncio.sleep(0)
            yield i
    lst = [x async for x in asrc(4)]
    st = {x async for x in asrc(4)}
    dct = {x: x * x async for x in asrc(3)}
    # comprehension with async filter condition
    evens = [x async for x in asrc(6) if x % 2 == 0]
    # bare async for
    collected = []
    async for x in asrc(3):
        collected.append(x)
    return (lst == [0, 1, 2, 3] and st == {0, 1, 2, 3}
            and dct == {0: 0, 1: 1, 2: 4} and evens == [0, 2, 4]
            and collected == [0, 1, 2])
chk("async_comprehensions_and_for", asyncio.run(_async_comprehensions()))

# =====================================================================
# Async context managers — __aenter__ / __aexit__ ; contextlib helpers
# Ref: PEP 492 `async with`; contextlib.asynccontextmanager;
#      contextlib.AsyncExitStack.
#   how: a class-based async CM tracking enter/exit; an @asynccontextmanager
#        generator; AsyncExitStack with multiple async callbacks (LIFO);
#        verify __aexit__ runs on exception and can suppress.
#   expected: enter/exit order correct; AsyncExitStack unwinds LIFO; returning
#             True from __aexit__ suppresses the exception.
#   why: async resource management is core to real services.
# =====================================================================
async def _async_cm():
    events = []
    class CM:
        def __init__(self, name, suppress=False):
            self.name = name
            self.suppress = suppress
        async def __aenter__(self):
            events.append("enter " + self.name)
            return self.name
        async def __aexit__(self, et, ev, tb):
            events.append("exit " + self.name)
            return self.suppress  # True -> suppress
    async with CM("a") as v:
        events.append("body " + v)
    chk("async_cm_enter_exit", events == ["enter a", "body a", "exit a"], repr(events))

    # __aexit__ suppression
    suppressed = True
    try:
        async with CM("s", suppress=True):
            raise ValueError("inside")
    except ValueError:
        suppressed = False
    chk("async_cm_suppress", suppressed)

    # __aexit__ sees the exception when not suppressed
    seen = {}
    class CM2:
        async def __aenter__(self):
            return self
        async def __aexit__(self, et, ev, tb):
            seen["type"] = et
            return False
    try:
        async with CM2():
            raise KeyError("k")
    except KeyError:
        pass
    chk("async_cm_exit_sees_exc", seen.get("type") is KeyError)
asyncio.run(_async_cm())

import contextlib
if hasattr(contextlib, "asynccontextmanager"):
    async def _acm_decorator():
        log = []
        @contextlib.asynccontextmanager
        async def acm(tag):
            log.append("enter " + tag)
            await asyncio.sleep(0)
            try:
                yield tag
            finally:
                log.append("exit " + tag)
        async with acm("z") as v:
            log.append("body " + v)
        return log == ["enter z", "body z", "exit z"]
    chk("asynccontextmanager", asyncio.run(_acm_decorator()))
else:
    chk("asynccontextmanager", True, "(skip: needs 3.7)")

if hasattr(contextlib, "AsyncExitStack"):
    async def _async_exit_stack():
        order = []
        async with contextlib.AsyncExitStack() as stack:
            for i in range(3):
                stack.push_async_callback(_mkcb(order, i))
        return order == [2, 1, 0]
    def _mkcb(order, i):
        async def cb():
            order.append(i)
        return cb
    chk("async_exit_stack_lifo", asyncio.run(_async_exit_stack()))
else:
    chk("async_exit_stack_lifo", True, "(skip: needs 3.7)")

# =====================================================================
# Task cancellation — task.cancel + CancelledError + finally cleanup
# Ref: Task.cancel(msg=None); asyncio.CancelledError; "Task cancellation".
#   how: cancel a sleeping task; confirm CancelledError propagates; confirm a
#        `finally` block runs cleanup; check cancelled() state; confirm
#        CancelledError derives from BaseException (not Exception) since 3.8.
#   expected: cancel() schedules a CancelledError at the next await; finally
#             runs; task.cancelled() becomes True after the cancellation settles.
#   why: cooperative cancellation correctness prevents resource leaks.
# =====================================================================
chk("cancellederror_baseexception",
    issubclass(asyncio.CancelledError, BaseException)
    and not issubclass(asyncio.CancelledError, Exception))

async def _cancel_finally():
    cleaned = {"v": False}
    started = asyncio.Event()
    async def long_op():
        started.set()
        try:
            await asyncio.sleep(100)
            return "done"
        finally:
            cleaned["v"] = True
    t = asyncio.create_task(long_op())
    await started.wait()
    t.cancel()
    try:
        await t
        raised = False
    except asyncio.CancelledError:
        raised = True
    return raised and t.cancelled() and cleaned["v"]
chk("task_cancel_finally", asyncio.run(_cancel_finally()))

async def _cancel_swallow_then_complete():
    # A task may catch CancelledError and choose to finish; but the modern
    # recommendation is to re-raise. Here we verify catching works and the
    # task is NOT marked cancelled if it returns normally.
    async def stubborn():
        try:
            await asyncio.sleep(100)
        except asyncio.CancelledError:
            return "survived"
    t = asyncio.create_task(stubborn())
    await asyncio.sleep(0)
    t.cancel()
    res = await t
    return res == "survived" and not t.cancelled()
chk("task_catch_cancel_returns", asyncio.run(_cancel_swallow_then_complete()))

async def _cancel_with_message():
    async def s():
        await asyncio.sleep(100)
    t = asyncio.create_task(s())
    await asyncio.sleep(0)
    t.cancel("stop now")
    try:
        await t
        msg = None
    except asyncio.CancelledError as e:
        msg = e.args[0] if e.args else None
    return msg == "stop now"
chk("task_cancel_message", asyncio.run(_cancel_with_message()))

# Task.cancelling() / Task.uncancel() — cancellation request counter (3.11+).
# how: observe the counter go 0->1 on cancel(); a task that catches the
#      CancelledError and calls uncancel() decrements it back and is NOT marked
#      cancelled when it returns normally.
# expected: cancelling() reflects outstanding cancel requests; uncancel()
#           returns the decremented count; settled task.cancelled() stays False.
if hasattr(asyncio.Task, "cancelling"):
    async def _cancel_uncancel():
        async def s():
            try:
                await asyncio.sleep(100)
            except asyncio.CancelledError:
                # decrement the outstanding-cancellation counter and survive.
                return ("survived", asyncio.current_task().uncancel())
        t = asyncio.create_task(s())
        await asyncio.sleep(0)
        before = t.cancelling()
        t.cancel()
        after = t.cancelling()
        res = await t
        return (before == 0 and after == 1
                and res == ("survived", 0) and not t.cancelled())
    chk("task_cancelling_uncancel", asyncio.run(_cancel_uncancel()))
else:
    chk("task_cancelling_uncancel", True, "(skip: needs 3.11)")

# A gather() future is itself cancellable, and cancelling it propagates a
# CancelledError into every still-pending child coroutine.
async def _gather_cancel_propagates():
    inner_cancelled = {"v": 0}
    async def w():
        try:
            await asyncio.sleep(100)
        except asyncio.CancelledError:
            inner_cancelled["v"] += 1
            raise
    g = asyncio.gather(w(), w())
    await asyncio.sleep(0)
    ok_cancel = g.cancel()
    try:
        await g
        raised = False
    except asyncio.CancelledError:
        raised = True
    return ok_cancel and raised and inner_cancelled["v"] == 2
chk("gather_cancel_propagates", asyncio.run(_gather_cancel_propagates()))

# =====================================================================
# Futures — low-level awaitable result containers
# Ref: loop.create_future(); Future.set_result/set_exception/done/result.
#   how: create a future, schedule a setter via call_soon, await it; also test
#        set_exception path; add_done_callback fires.
#   expected: awaiting a future yields its set result; exceptions propagate;
#             done callbacks run after completion.
#   why: futures bridge callback-based code into async/await.
# =====================================================================
async def _future_result():
    loop = asyncio.get_running_loop()
    fut = loop.create_future()
    cb_fired = {"v": False}
    fut.add_done_callback(lambda f: cb_fired.__setitem__("v", True))
    loop.call_soon(fut.set_result, "value")
    r = await fut
    await asyncio.sleep(0)  # let done callback run
    return r == "value" and fut.done() and cb_fired["v"]
chk("future_set_result_and_callback", asyncio.run(_future_result()))

async def _future_exception():
    loop = asyncio.get_running_loop()
    fut = loop.create_future()
    loop.call_soon(fut.set_exception, RuntimeError("ferr"))
    try:
        await fut
        return "no"
    except RuntimeError as e:
        return str(e)
chk("future_set_exception", asyncio.run(_future_exception()) == "ferr")

async def _future_cancel():
    # Future.cancel() transitions a pending future to cancelled: cancel()
    # returns True, awaiting it raises CancelledError, cancelled()/done() True.
    loop = asyncio.get_running_loop()
    fut = loop.create_future()
    ok = fut.cancel()
    try:
        await fut
        raised = False
    except asyncio.CancelledError:
        raised = True
    return ok and raised and fut.cancelled() and fut.done()
chk("future_cancel", asyncio.run(_future_cancel()))

# =====================================================================
# loop.call_soon / call_later — scheduling callbacks
# Ref: loop.call_soon(cb, *args); loop.call_later(delay, cb, *args).
#   how: schedule callbacks and observe ordering; call_soon is FIFO; verify
#        a future fires after call_later fires.
#   expected: call_soon callbacks run in registration order on the next tick.
#   why: callback scheduling is the loop's lowest-level primitive.
# =====================================================================
async def _call_soon_order():
    loop = asyncio.get_running_loop()
    seq = []
    done = loop.create_future()
    loop.call_soon(seq.append, 1)
    loop.call_soon(seq.append, 2)
    loop.call_soon(lambda: done.set_result(None))
    await done
    return seq == [1, 2]
chk("call_soon_fifo", asyncio.run(_call_soon_order()))

async def _call_later():
    loop = asyncio.get_running_loop()
    fired = loop.create_future()
    loop.call_later(0.01, lambda: fired.set_result("late"))
    return (await fired) == "late"
chk("call_later", asyncio.run(_call_later()))

async def _call_at():
    # call_at schedules at an absolute loop.time(); returns a TimerHandle.
    loop = asyncio.get_running_loop()
    fired = loop.create_future()
    h = loop.call_at(loop.time() + 0.01, lambda: fired.set_result("at"))
    r = await fired
    return r == "at" and isinstance(h, asyncio.TimerHandle)
chk("call_at", asyncio.run(_call_at()))

async def _handle_cancel():
    # call_soon returns a Handle; cancelling it before the next tick prevents
    # the callback from ever firing, and Handle.cancelled() becomes True.
    loop = asyncio.get_running_loop()
    ran = {"v": False}
    gate = loop.create_future()
    h = loop.call_soon(ran.__setitem__, "v", True)
    h.cancel()
    loop.call_soon(gate.set_result, None)
    await gate
    await asyncio.sleep(0)
    return h.cancelled() and ran["v"] is False
chk("handle_cancel_prevents_callback", asyncio.run(_handle_cancel()))

# =====================================================================
# loop.run_in_executor — default thread pool offload
# Ref: loop.run_in_executor(executor, func, *args).
#   how: offload a blocking function via the default executor (None).
#   expected: returns the function result computed off the loop thread.
#   why: legacy/explicit counterpart to to_thread.
# =====================================================================
async def _run_in_executor():
    loop = asyncio.get_running_loop()
    def blocking(n):
        return n * n
    return await loop.run_in_executor(None, blocking, 9)
chk("run_in_executor", asyncio.run(_run_in_executor()) == 81)

# =====================================================================
# Awaitable protocol — custom __await__
# Ref: Language Reference: an object is awaitable if it implements __await__.
#   how: define a class whose __await__ delegates to a generator yielding once;
#        await it inside a coroutine.
#   expected: `await obj` drives obj.__await__() to completion, returns its value.
#   why: frameworks build custom awaitables on this protocol.
# =====================================================================
class _MyAwaitable:
    def __init__(self, v):
        self.v = v
    def __await__(self):
        yield  # suspend once
        return self.v
async def _custom_await():
    return await _MyAwaitable("awaited")
chk("custom_awaitable", asyncio.run(_custom_await()) == "awaited")

# =====================================================================
# asyncio.as_completed — iterate results in completion order
# Ref: asyncio.as_completed(aws, *, timeout=None).
#   how: schedule tasks with different yield counts; collect via as_completed;
#        results arrive in completion order, not submission order.
#   expected: yields awaitables that resolve as their underlying task finishes.
#   why: stream results as soon as each is ready.
# =====================================================================
async def _as_completed():
    async def d(val, n):
        for _ in range(n):
            await asyncio.sleep(0)
        return val
    coros = [d("slow", 5), d("fast", 1), d("mid", 3)]
    order = []
    for fut in asyncio.as_completed(coros):
        order.append(await fut)
    # fast (1 tick) completes before mid (3) before slow (5).
    return order == ["fast", "mid", "slow"]
chk("as_completed_order", asyncio.run(_as_completed()))

# as_completed(timeout=...) — a too-slow awaitable surfaces TimeoutError when
# iterating the yielded futures past the deadline.
async def _as_completed_timeout():
    async def slow():
        await asyncio.sleep(10)
        return "never"
    try:
        for fut in asyncio.as_completed([slow()], timeout=0.01):
            await fut
        return "no_timeout"
    except (asyncio.TimeoutError, TimeoutError):
        return "timeout"
chk("as_completed_timeout", asyncio.run(_as_completed_timeout()) == "timeout")

# =====================================================================
# Running coroutine across explicit loop lifecycle
# Ref: asyncio.new_event_loop / loop.run_until_complete / loop.close.
#   how: manually create a loop, run a coroutine, close it; check is_closed.
#   expected: run_until_complete returns the coroutine result; close() finalizes.
#   why: lower-level control for embedding/legacy code.
# =====================================================================
def _manual_loop():
    loop = asyncio.new_event_loop()
    try:
        async def c():
            await asyncio.sleep(0)
            return "manual"
        r = loop.run_until_complete(c())
    finally:
        loop.close()
    return r == "manual" and loop.is_closed()
chk("manual_loop_lifecycle", _manual_loop())

# loop.run_forever() + loop.stop(): a callback that calls stop() ends the loop;
# is_running() is True inside and False after run_forever returns.
def _run_forever_stop():
    loop = asyncio.new_event_loop()
    seq = []
    try:
        def cb():
            seq.append(loop.is_running())  # True while the loop drives us
            loop.stop()
        loop.call_soon(cb)
        loop.run_forever()
    finally:
        ran_after = loop.is_running()
        loop.close()
    return seq == [True] and ran_after is False
chk("loop_run_forever_stop", _run_forever_stop())

# set_event_loop / get_event_loop manage the thread's default loop binding.
# After set_event_loop(L), get_event_loop() must return L (when no loop runs).
def _get_set_event_loop():
    loop = asyncio.new_event_loop()
    try:
        asyncio.set_event_loop(loop)
        same = asyncio.get_event_loop() is loop
    finally:
        asyncio.set_event_loop(None)
        loop.close()
    # After set_event_loop(None) the default binding is cleared; fetching it now
    # raises RuntimeError (no current event loop) outside a running loop.
    try:
        asyncio.get_event_loop()
        cleared = False
    except RuntimeError:
        cleared = True
    return same and cleared
chk("get_set_event_loop", _get_set_event_loop())

# =====================================================================
# Edge / version: sleep & wait_for boundary behavior; Queue.shutdown (3.13+)
#   how: negative sleep delay behaves like an immediate yield; wait_for with a
#        None timeout means "no timeout"; Queue.shutdown (3.13+) stops the queue.
#   why: boundary inputs and newest stdlib surface must behave per docs.
# =====================================================================
async def _sleep_negative():
    # A negative delay is clamped to an immediate (zero) yield and returns result.
    return await asyncio.sleep(-1, result="neg")
chk("sleep_negative_delay", asyncio.run(_sleep_negative()) == "neg")

async def _wait_for_none_timeout():
    # timeout=None disables the timeout and just awaits to completion.
    async def q():
        await asyncio.sleep(0)
        return "ok"
    return await asyncio.wait_for(q(), None)
chk("wait_for_none_timeout", asyncio.run(_wait_for_none_timeout()) == "ok")

if hasattr(asyncio.Queue, "shutdown"):
    async def _queue_shutdown():
        # shutdown() makes further put/get raise QueueShutDown (3.13+).
        q = asyncio.Queue()
        await q.put(1)
        q.shutdown()
        got = await q.get()  # buffered item still retrievable
        try:
            await q.put(2)
            put_raised = False
        except asyncio.QueueShutDown:
            put_raised = True
        try:
            await q.get()  # empty + shut down -> raises
            get_raised = False
        except asyncio.QueueShutDown:
            get_raised = True
        return got == 1 and put_raised and get_raised
    chk("queue_shutdown", asyncio.run(_queue_shutdown()))
else:
    chk("queue_shutdown", True, "(skip: needs 3.13)")

print(("PY_ASYNC_OK") if _ok else ("PY_ASYNC_FAIL"))
sys.exit(0 if _ok else 1)
