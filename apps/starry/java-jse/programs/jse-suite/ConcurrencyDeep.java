import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.*;
import java.util.concurrent.locks.*;
import java.util.function.*;
import java.util.stream.*;

// Carpet-level coverage of java.util.concurrent (+ .atomic / .locks):
//   - atomics: AtomicInteger/Long/Boolean/Reference + Arrays + FieldUpdaters
//              + Stamped/MarkableReference + LongAdder/DoubleAdder
//              + Long/DoubleAccumulator
//   - locks:   ReentrantLock + Condition, ReentrantReadWriteLock (+downgrade),
//              StampedLock (optimistic/convert), LockSupport park/unpark/blocker
//   - sync:    CountDownLatch, CyclicBarrier (+action/reuse), Semaphore (fair),
//              Phaser (multi-phase), Exchanger
//   - exec:    Executors factories, ThreadPoolExecutor metrics + rejection,
//              ScheduledExecutorService, FutureTask, invokeAll/invokeAny,
//              ExecutorCompletionService, custom ThreadFactory, Future.cancel
//   - cfut:    CompletableFuture full combinator matrix + error handling
//   - forkjoin:ForkJoinPool, RecursiveTask, RecursiveAction, commonPool
//   - colls:   ConcurrentHashMap (full op matrix + bulk reduce/search/forEach),
//              ConcurrentSkipListMap/Set, CopyOnWriteArrayList/Set,
//              ConcurrentLinkedQueue/Deque
//   - queues:  LinkedBlockingQueue, ArrayBlockingQueue, PriorityBlockingQueue,
//              SynchronousQueue, LinkedBlockingDeque, LinkedTransferQueue,
//              DelayQueue
//   - misc:    ThreadLocal/InheritableThreadLocal, ThreadLocalRandom, TimeUnit
//   - stress:  high-contention counters, parallel stream, producer/consumer
// Deterministic + offline; exact-equality assertions only.
public class ConcurrencyDeep {
    static int ok = 0, fail = 0;
    static void chk(boolean c, String m) { if (c) ok++; else { fail++; System.out.println("FAIL " + m); } }

    static final long T_SEC = 60; // generous timeout for slow emulated targets

    public static void main(String[] args) throws Exception {
        testAtomics();
        testAtomicArrays();
        testFieldUpdaters();
        testStampedMarkable();
        testAdders();
        testReentrantLock();
        testCondition();
        testReadWriteLock();
        testStampedLock();
        testLockSupport();
        testLatch();
        testCyclicBarrier();
        testSemaphore();
        testPhaser();
        testExchanger();
        testExecutors();
        testThreadPoolExecutor();
        testRejection();
        testScheduled();
        testFutureTask();
        testInvokeAllAny();
        testCompletionService();
        testThreadFactoryAndCancel();
        testCompletableFuture();
        testForkJoin();
        testConcurrentHashMap();
        testSkipList();
        testCopyOnWrite();
        testConcurrentQueuesDeques();
        testBlockingQueues();
        testSynchronousQueue();
        testTransferQueue();
        testDelayQueue();
        testThreadLocalAndRandom();
        testTimeUnit();
        testDeepStress();

        System.out.println("CONCURRENCY_DEEP_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) System.out.println("CONCURRENCY_DEEP_DONE");
    }

    // ---------------------------------------------------------------- atomics
    static void testAtomics() {
        AtomicInteger ai = new AtomicInteger(10);
        chk(ai.get() == 10, "AtomicInteger.get init");
        chk(ai.getAndIncrement() == 10 && ai.get() == 11, "AtomicInteger.getAndIncrement");
        chk(ai.incrementAndGet() == 12, "AtomicInteger.incrementAndGet");
        chk(ai.getAndAdd(5) == 12 && ai.get() == 17, "AtomicInteger.getAndAdd");
        chk(ai.addAndGet(3) == 20, "AtomicInteger.addAndGet");
        chk(ai.compareAndSet(20, 100) && ai.get() == 100, "AtomicInteger.compareAndSet hit");
        chk(!ai.compareAndSet(20, 0) && ai.get() == 100, "AtomicInteger.compareAndSet miss");
        chk(ai.getAndSet(50) == 100 && ai.get() == 50, "AtomicInteger.getAndSet");
        chk(ai.updateAndGet(x -> x + 1) == 51, "AtomicInteger.updateAndGet");
        chk(ai.getAndUpdate(x -> x * 2) == 51 && ai.get() == 102, "AtomicInteger.getAndUpdate");
        chk(ai.accumulateAndGet(8, Integer::sum) == 110, "AtomicInteger.accumulateAndGet");
        chk(ai.getAndAccumulate(0, (a, b) -> a - 1) == 110 && ai.get() == 109, "AtomicInteger.getAndAccumulate");
        chk(ai.compareAndExchange(109, 200) == 109 && ai.get() == 200, "AtomicInteger.compareAndExchange");
        chk(ai.getAndDecrement() == 200 && ai.get() == 199, "AtomicInteger.getAndDecrement");
        chk(ai.decrementAndGet() == 198, "AtomicInteger.decrementAndGet");
        chk(ai.intValue() == 198 && ai.longValue() == 198L, "AtomicInteger.value conversions");

        AtomicLong al = new AtomicLong(100L);
        chk(al.incrementAndGet() == 101L, "AtomicLong.incrementAndGet");
        chk(al.addAndGet(99L) == 200L, "AtomicLong.addAndGet");
        chk(al.compareAndSet(200L, 1000L) && al.get() == 1000L, "AtomicLong.compareAndSet");
        chk(al.updateAndGet(x -> x / 2) == 500L, "AtomicLong.updateAndGet");
        chk(al.getAndSet(7L) == 500L && al.get() == 7L, "AtomicLong.getAndSet");
        chk(al.accumulateAndGet(3L, Long::sum) == 10L, "AtomicLong.accumulateAndGet");

        AtomicBoolean ab = new AtomicBoolean(false);
        chk(ab.compareAndSet(false, true) && ab.get(), "AtomicBoolean.compareAndSet hit");
        chk(!ab.compareAndSet(false, true) && ab.get(), "AtomicBoolean.compareAndSet miss");
        chk(ab.getAndSet(false) && !ab.get(), "AtomicBoolean.getAndSet");

        AtomicReference<String> ar = new AtomicReference<>("init");
        chk(ar.compareAndSet("init", "next") && ar.get().equals("next"), "AtomicReference.compareAndSet");
        chk(ar.getAndSet("last").equals("next"), "AtomicReference.getAndSet");
        chk(ar.updateAndGet(s -> s + "!").equals("last!"), "AtomicReference.updateAndGet");
        chk(ar.accumulateAndGet("?", (a, b) -> a + b).equals("last!?"), "AtomicReference.accumulateAndGet");
        String arWitness = ar.get(); // compareAndExchange compares by identity (==), so pass the live reference
        chk(ar.compareAndExchange(arWitness, "done") == arWitness && ar.get().equals("done"),
                "AtomicReference.compareAndExchange");
    }

    static void testAtomicArrays() {
        AtomicIntegerArray aia = new AtomicIntegerArray(5);
        chk(aia.length() == 5, "AtomicIntegerArray.length");
        aia.set(0, 10);
        chk(aia.getAndIncrement(0) == 10 && aia.get(0) == 11, "AtomicIntegerArray.getAndIncrement");
        chk(aia.addAndGet(1, 7) == 7, "AtomicIntegerArray.addAndGet");
        chk(aia.compareAndSet(2, 0, 99) && aia.get(2) == 99, "AtomicIntegerArray.compareAndSet");
        chk(aia.updateAndGet(3, x -> x + 5) == 5, "AtomicIntegerArray.updateAndGet");
        chk(aia.accumulateAndGet(4, 3, Integer::sum) == 3, "AtomicIntegerArray.accumulateAndGet");

        AtomicLongArray ala = new AtomicLongArray(3);
        ala.set(0, 100L);
        chk(ala.incrementAndGet(0) == 101L, "AtomicLongArray.incrementAndGet");
        chk(ala.addAndGet(1, 50L) == 50L, "AtomicLongArray.addAndGet");
        chk(ala.length() == 3, "AtomicLongArray.length");

        AtomicReferenceArray<String> ara = new AtomicReferenceArray<>(new String[]{"a", "b", "c"});
        chk(ara.get(1).equals("b"), "AtomicReferenceArray.get");
        chk(ara.compareAndSet(1, "b", "B") && ara.get(1).equals("B"), "AtomicReferenceArray.compareAndSet");
        chk(ara.getAndSet(2, "C").equals("c") && ara.get(2).equals("C"), "AtomicReferenceArray.getAndSet");
        chk(ara.length() == 3, "AtomicReferenceArray.length");
    }

    static void testFieldUpdaters() {
        FieldHolder fh = new FieldHolder();
        AtomicIntegerFieldUpdater<FieldHolder> iu = AtomicIntegerFieldUpdater.newUpdater(FieldHolder.class, "iv");
        iu.set(fh, 5);
        chk(iu.get(fh) == 5, "AtomicIntegerFieldUpdater.set/get");
        chk(iu.incrementAndGet(fh) == 6, "AtomicIntegerFieldUpdater.incrementAndGet");
        chk(iu.compareAndSet(fh, 6, 60) && fh.iv == 60, "AtomicIntegerFieldUpdater.compareAndSet");
        chk(iu.addAndGet(fh, 40) == 100, "AtomicIntegerFieldUpdater.addAndGet");

        AtomicLongFieldUpdater<FieldHolder> lu = AtomicLongFieldUpdater.newUpdater(FieldHolder.class, "lv");
        lu.set(fh, 100L);
        chk(lu.addAndGet(fh, 50L) == 150L, "AtomicLongFieldUpdater.addAndGet");
        chk(lu.getAndIncrement(fh) == 150L && fh.lv == 151L, "AtomicLongFieldUpdater.getAndIncrement");

        AtomicReferenceFieldUpdater<FieldHolder, String> ru =
                AtomicReferenceFieldUpdater.newUpdater(FieldHolder.class, String.class, "rv");
        ru.set(fh, "x");
        chk(ru.compareAndSet(fh, "x", "y") && fh.rv.equals("y"), "AtomicReferenceFieldUpdater.compareAndSet");
        chk(ru.getAndSet(fh, "z").equals("y"), "AtomicReferenceFieldUpdater.getAndSet");
    }

    static void testStampedMarkable() {
        AtomicStampedReference<String> asr = new AtomicStampedReference<>("v0", 0);
        chk(asr.getReference().equals("v0") && asr.getStamp() == 0, "AtomicStampedReference init");
        chk(asr.compareAndSet("v0", "v1", 0, 1) && asr.getReference().equals("v1") && asr.getStamp() == 1,
                "AtomicStampedReference.compareAndSet hit");
        chk(!asr.compareAndSet("v1", "v2", 0, 2), "AtomicStampedReference.compareAndSet wrong stamp");
        int[] sh = new int[1];
        chk(asr.get(sh).equals("v1") && sh[0] == 1, "AtomicStampedReference.get(holder)");
        asr.set("v2", 5);
        chk(asr.getReference().equals("v2") && asr.getStamp() == 5, "AtomicStampedReference.set");
        chk(asr.attemptStamp("v2", 9) && asr.getStamp() == 9, "AtomicStampedReference.attemptStamp");

        AtomicMarkableReference<String> amr = new AtomicMarkableReference<>("m0", false);
        chk(!amr.isMarked(), "AtomicMarkableReference init unmarked");
        chk(amr.compareAndSet("m0", "m1", false, true), "AtomicMarkableReference.compareAndSet");
        chk(amr.isMarked(), "AtomicMarkableReference marked");
        boolean[] mh = new boolean[1];
        chk(amr.get(mh).equals("m1") && mh[0], "AtomicMarkableReference.get(holder)");
        chk(amr.attemptMark("m1", false) && !amr.isMarked(), "AtomicMarkableReference.attemptMark");
    }

    static void testAdders() {
        LongAdder la = new LongAdder();
        la.add(5);
        la.increment();
        la.increment();
        chk(la.sum() == 7L && la.longValue() == 7L && la.intValue() == 7, "LongAdder.sum");
        la.decrement();
        chk(la.sum() == 6L, "LongAdder.decrement");
        la.reset();
        chk(la.sum() == 0L, "LongAdder.reset");
        chk(la.sumThenReset() == 0L, "LongAdder.sumThenReset");

        DoubleAdder da = new DoubleAdder();
        da.add(1.5);
        da.add(2.5);
        chk(da.sum() == 4.0, "DoubleAdder.sum");
        chk(da.doubleValue() == 4.0, "DoubleAdder.doubleValue");

        LongAccumulator lmax = new LongAccumulator(Long::max, Long.MIN_VALUE);
        lmax.accumulate(5);
        lmax.accumulate(3);
        lmax.accumulate(9);
        lmax.accumulate(1);
        chk(lmax.get() == 9L, "LongAccumulator max");
        LongAccumulator lsum = new LongAccumulator(Long::sum, 0L);
        lsum.accumulate(1);
        lsum.accumulate(2);
        lsum.accumulate(3);
        chk(lsum.get() == 6L, "LongAccumulator sum");
        chk(lsum.longValue() == 6L, "LongAccumulator.longValue");

        DoubleAccumulator dacc = new DoubleAccumulator((a, b) -> a + b, 0.0);
        dacc.accumulate(1.25);
        dacc.accumulate(2.75);
        chk(dacc.get() == 4.0, "DoubleAccumulator sum");
    }

    // ------------------------------------------------------------------ locks
    static void testReentrantLock() {
        ReentrantLock rl = new ReentrantLock();
        chk(!rl.isLocked(), "ReentrantLock initially unlocked");
        rl.lock();
        chk(rl.isLocked() && rl.isHeldByCurrentThread() && rl.getHoldCount() == 1, "ReentrantLock.lock");
        rl.lock();
        chk(rl.getHoldCount() == 2, "ReentrantLock reentrant holdCount");
        rl.unlock();
        rl.unlock();
        chk(!rl.isLocked() && rl.getHoldCount() == 0, "ReentrantLock fully unlocked");
        chk(rl.tryLock() && rl.isLocked(), "ReentrantLock.tryLock");
        rl.unlock();
        chk(!rl.isFair() && rl.getQueueLength() == 0, "ReentrantLock not fair, empty queue");

        ReentrantLock fair = new ReentrantLock(true);
        chk(fair.isFair(), "ReentrantLock fair");
    }

    static void testCondition() throws Exception {
        ReentrantLock lock = new ReentrantLock();
        Condition notEmpty = lock.newCondition();
        final Integer[] box = {null};
        ExecutorService ex = Executors.newSingleThreadExecutor();
        Future<Integer> f = ex.submit(() -> {
            lock.lock();
            try {
                while (box[0] == null) notEmpty.await();
                return box[0];
            } finally {
                lock.unlock();
            }
        });
        // ensure consumer is awaiting before signalling (getWaitQueueLength requires the lock)
        long deadline = System.nanoTime() + 30_000_000_000L;
        while (System.nanoTime() < deadline) {
            boolean waiting;
            lock.lock();
            try { waiting = lock.getWaitQueueLength(notEmpty) > 0; } finally { lock.unlock(); }
            if (waiting) break;
            Thread.onSpinWait();
        }
        lock.lock();
        try {
            box[0] = 42;
            notEmpty.signal();
        } finally {
            lock.unlock();
        }
        chk(f.get(T_SEC, TimeUnit.SECONDS) == 42, "ReentrantLock Condition await/signal");
        ex.shutdown();
        chk(ex.awaitTermination(T_SEC, TimeUnit.SECONDS), "Condition test pool terminated");

        // awaitNanos returns a remaining (<=0 implies timed out) without hanging
        ReentrantLock l2 = new ReentrantLock();
        Condition c2 = l2.newCondition();
        l2.lock();
        try {
            long rem = c2.awaitNanos(1_000_000L);
            chk(rem <= 1_000_000L, "Condition.awaitNanos returns");
        } finally {
            l2.unlock();
        }
    }

    static void testReadWriteLock() {
        ReentrantReadWriteLock rw = new ReentrantReadWriteLock();
        rw.readLock().lock();
        rw.readLock().lock();
        chk(rw.getReadLockCount() == 2 && rw.getReadHoldCount() == 2, "RWLock read counts");
        rw.readLock().unlock();
        rw.readLock().unlock();
        chk(rw.getReadLockCount() == 0, "RWLock reads released");

        rw.writeLock().lock();
        chk(rw.isWriteLocked() && rw.isWriteLockedByCurrentThread() && rw.getWriteHoldCount() == 1,
                "RWLock write held");
        // lock downgrade: acquire read while holding write, then release write
        rw.readLock().lock();
        rw.writeLock().unlock();
        chk(!rw.isWriteLocked() && rw.getReadLockCount() == 1, "RWLock downgrade to read");
        rw.readLock().unlock();
        chk(rw.getReadLockCount() == 0, "RWLock downgrade released");
        chk(!rw.isFair(), "RWLock default not fair");
    }

    static void testStampedLock() {
        StampedLock sl = new StampedLock();
        long ws = sl.writeLock();
        chk(sl.isWriteLocked() && ws != 0L, "StampedLock.writeLock");
        sl.unlockWrite(ws);
        chk(!sl.isWriteLocked(), "StampedLock write released");

        long rs = sl.readLock();
        chk(sl.getReadLockCount() == 1 && sl.isReadLocked(), "StampedLock.readLock");
        sl.unlockRead(rs);

        long opt = sl.tryOptimisticRead();
        chk(opt != 0L && sl.validate(opt), "StampedLock optimistic valid");
        long w2 = sl.writeLock();
        chk(!sl.validate(opt), "StampedLock optimistic invalidated by write");
        sl.unlockWrite(w2);

        long rst = sl.readLock();
        long conv = sl.tryConvertToWriteLock(rst);
        chk(conv != 0L && sl.isWriteLocked(), "StampedLock tryConvertToWriteLock");
        long back = sl.tryConvertToReadLock(conv);
        chk(back != 0L && sl.isReadLocked() && !sl.isWriteLocked(), "StampedLock tryConvertToReadLock");
        sl.unlock(back);
        chk(!sl.isReadLocked(), "StampedLock fully released");
    }

    static void testLockSupport() throws Exception {
        AtomicBoolean done = new AtomicBoolean(false);
        Thread parker = new Thread(() -> {
            LockSupport.park("BLOCKER_TOKEN");
            done.set(true);
        });
        parker.start();
        long deadline = System.nanoTime() + 30_000_000_000L;
        while (parker.getState() != Thread.State.WAITING && System.nanoTime() < deadline) Thread.onSpinWait();
        chk(parker.getState() == Thread.State.WAITING, "LockSupport.park blocks thread");
        chk("BLOCKER_TOKEN".equals(LockSupport.getBlocker(parker)), "LockSupport.getBlocker");
        LockSupport.unpark(parker);
        parker.join(30_000);
        chk(done.get() && !parker.isAlive(), "LockSupport.unpark resumes thread");

        // parkNanos with already-elapsed budget returns promptly
        LockSupport.parkNanos(1_000_000L);
        chk(true, "LockSupport.parkNanos returns");
    }

    // ----------------------------------------------------------- synchronizers
    static void testLatch() throws Exception {
        final int n = 4;
        CountDownLatch latch = new CountDownLatch(n);
        chk(latch.getCount() == n, "CountDownLatch initial count");
        AtomicInteger sum = new AtomicInteger();
        ExecutorService ex = Executors.newFixedThreadPool(n);
        for (int i = 1; i <= n; i++) {
            final int v = i;
            ex.submit(() -> {
                sum.addAndGet(v);
                latch.countDown();
            });
        }
        chk(latch.await(T_SEC, TimeUnit.SECONDS), "CountDownLatch.await completes");
        chk(latch.getCount() == 0, "CountDownLatch count reaches zero");
        chk(sum.get() == 10, "CountDownLatch all tasks ran (1+2+3+4)");
        ex.shutdown();
        chk(ex.awaitTermination(T_SEC, TimeUnit.SECONDS), "latch pool terminated");
    }

    static void testCyclicBarrier() throws Exception {
        final int parties = 4, rounds = 2;
        AtomicInteger actions = new AtomicInteger();
        CyclicBarrier cb = new CyclicBarrier(parties, actions::incrementAndGet);
        chk(cb.getParties() == 4 && cb.getNumberWaiting() == 0, "CyclicBarrier parties");
        AtomicInteger arrivals = new AtomicInteger();
        ExecutorService ex = Executors.newFixedThreadPool(parties);
        CountDownLatch done = new CountDownLatch(parties);
        for (int t = 0; t < parties; t++) {
            ex.submit(() -> {
                try {
                    for (int r = 0; r < rounds; r++) {
                        arrivals.incrementAndGet();
                        cb.await(T_SEC, TimeUnit.SECONDS);
                    }
                } catch (Exception ignored) {
                } finally {
                    done.countDown();
                }
            });
        }
        chk(done.await(T_SEC, TimeUnit.SECONDS), "CyclicBarrier rounds complete");
        chk(arrivals.get() == parties * rounds, "CyclicBarrier all arrivals");
        chk(actions.get() == rounds, "CyclicBarrier action ran once per round");
        chk(!cb.isBroken(), "CyclicBarrier not broken");
        ex.shutdown();
        chk(ex.awaitTermination(T_SEC, TimeUnit.SECONDS), "barrier pool terminated");
    }

    static void testSemaphore() throws Exception {
        Semaphore sem = new Semaphore(3);
        chk(sem.availablePermits() == 3, "Semaphore initial permits");
        chk(sem.tryAcquire() && sem.availablePermits() == 2, "Semaphore.tryAcquire");
        sem.acquire(2);
        chk(sem.availablePermits() == 0, "Semaphore acquire to empty");
        chk(!sem.tryAcquire(), "Semaphore tryAcquire when empty");
        chk(!sem.tryAcquire(50, TimeUnit.MILLISECONDS), "Semaphore timed tryAcquire fails");
        sem.release();
        chk(sem.availablePermits() == 1, "Semaphore.release");
        chk(sem.drainPermits() == 1 && sem.availablePermits() == 0, "Semaphore.drainPermits");
        sem.release(5);
        chk(sem.availablePermits() == 5, "Semaphore.release(n)");
        chk(sem.tryAcquire(3) && sem.availablePermits() == 2, "Semaphore.tryAcquire(n)");
        sem.acquireUninterruptibly(2);
        chk(sem.availablePermits() == 0, "Semaphore.acquireUninterruptibly");

        Semaphore fair = new Semaphore(0, true);
        chk(fair.isFair(), "Semaphore fair");
    }

    static void testPhaser() throws Exception {
        Phaser solo = new Phaser(1);
        chk(solo.getPhase() == 0 && solo.getRegisteredParties() == 1, "Phaser solo init");
        solo.register();
        chk(solo.getRegisteredParties() == 2, "Phaser.register");
        solo.arriveAndDeregister();
        chk(solo.getRegisteredParties() == 1, "Phaser.arriveAndDeregister");
        solo.arriveAndDeregister();
        chk(solo.isTerminated(), "Phaser terminated when parties drop to zero");

        final int parties = 4, rounds = 3;
        final Phaser ph = new Phaser(parties);
        AtomicInteger work = new AtomicInteger();
        ExecutorService ex = Executors.newFixedThreadPool(parties);
        CountDownLatch done = new CountDownLatch(parties);
        for (int t = 0; t < parties; t++) {
            ex.submit(() -> {
                for (int r = 0; r < rounds; r++) {
                    work.incrementAndGet();
                    ph.arriveAndAwaitAdvance();
                }
                done.countDown();
            });
        }
        chk(done.await(T_SEC, TimeUnit.SECONDS), "Phaser multi-phase completes");
        chk(work.get() == parties * rounds, "Phaser exact work units");
        chk(ph.getPhase() == rounds, "Phaser advanced through all phases");
        ex.shutdown();
        chk(ex.awaitTermination(T_SEC, TimeUnit.SECONDS), "phaser pool terminated");
    }

    static void testExchanger() throws Exception {
        Exchanger<String> x = new Exchanger<>();
        ExecutorService ex = Executors.newFixedThreadPool(2);
        Future<String> a = ex.submit(() -> x.exchange("A"));
        Future<String> b = ex.submit(() -> x.exchange("B"));
        chk(a.get(T_SEC, TimeUnit.SECONDS).equals("B"), "Exchanger thread A receives B");
        chk(b.get(T_SEC, TimeUnit.SECONDS).equals("A"), "Exchanger thread B receives A");
        ex.shutdown();
        chk(ex.awaitTermination(T_SEC, TimeUnit.SECONDS), "exchanger pool terminated");
    }

    // -------------------------------------------------------------- executors
    static void testExecutors() throws Exception {
        ExecutorService single = Executors.newSingleThreadExecutor();
        chk(single.submit(() -> 7).get(T_SEC, TimeUnit.SECONDS) == 7, "newSingleThreadExecutor submit Callable");
        AtomicInteger r = new AtomicInteger();
        single.submit((Runnable) () -> r.set(3)).get(T_SEC, TimeUnit.SECONDS);
        chk(r.get() == 3, "submit Runnable");
        chk(single.submit(() -> {}, 99).get(T_SEC, TimeUnit.SECONDS) == 99, "submit Runnable with result");
        single.shutdown();
        chk(single.awaitTermination(T_SEC, TimeUnit.SECONDS) && single.isShutdown() && single.isTerminated(),
                "single executor lifecycle");

        ExecutorService cached = Executors.newCachedThreadPool();
        chk(cached.submit(() -> 1).get(T_SEC, TimeUnit.SECONDS) == 1, "newCachedThreadPool");
        cached.shutdown();
        chk(cached.awaitTermination(T_SEC, TimeUnit.SECONDS), "cached pool terminated");

        ExecutorService work = Executors.newWorkStealingPool(2);
        chk(work.submit(() -> 5).get(T_SEC, TimeUnit.SECONDS) == 5, "newWorkStealingPool");
        work.shutdown();
        chk(work.awaitTermination(T_SEC, TimeUnit.SECONDS), "work-stealing pool terminated");
    }

    static void testThreadPoolExecutor() throws Exception {
        ThreadPoolExecutor tpe = new ThreadPoolExecutor(2, 4, 1, TimeUnit.SECONDS, new LinkedBlockingQueue<>());
        chk(tpe.getCorePoolSize() == 2 && tpe.getMaximumPoolSize() == 4, "ThreadPoolExecutor sizes");
        List<Callable<Integer>> tasks = new ArrayList<>();
        for (int i = 1; i <= 5; i++) {
            final int v = i;
            tasks.add(() -> v);
        }
        List<Future<Integer>> fs = tpe.invokeAll(tasks, T_SEC, TimeUnit.SECONDS);
        int s = 0;
        for (Future<Integer> f : fs) s += f.get();
        chk(s == 15, "ThreadPoolExecutor invokeAll sum");
        tpe.shutdown();
        chk(tpe.awaitTermination(T_SEC, TimeUnit.SECONDS), "tpe terminated");
        chk(tpe.getCompletedTaskCount() == 5, "ThreadPoolExecutor completedTaskCount");
        chk(tpe.getTaskCount() == 5, "ThreadPoolExecutor taskCount");
    }

    static void testRejection() throws Exception {
        CountDownLatch block = new CountDownLatch(1);
        ThreadPoolExecutor rej = new ThreadPoolExecutor(1, 1, 0, TimeUnit.SECONDS, new ArrayBlockingQueue<>(1),
                new ThreadPoolExecutor.AbortPolicy());
        rej.execute(() -> {
            try { block.await(); } catch (InterruptedException ignored) {}
        });
        rej.execute(() -> {}); // queued (capacity 1)
        boolean rejected = false;
        try {
            rej.execute(() -> {});
        } catch (RejectedExecutionException e) {
            rejected = true;
        }
        chk(rejected, "ThreadPoolExecutor AbortPolicy rejects when saturated");
        block.countDown();
        rej.shutdown();
        chk(rej.awaitTermination(T_SEC, TimeUnit.SECONDS), "rejection pool terminated");

        // DiscardPolicy: rejected task silently dropped, no exception
        CountDownLatch block2 = new CountDownLatch(1);
        ThreadPoolExecutor disc = new ThreadPoolExecutor(1, 1, 0, TimeUnit.SECONDS, new ArrayBlockingQueue<>(1),
                new ThreadPoolExecutor.DiscardPolicy());
        disc.execute(() -> {
            try { block2.await(); } catch (InterruptedException ignored) {}
        });
        disc.execute(() -> {});
        AtomicBoolean ran = new AtomicBoolean(false);
        disc.execute(() -> ran.set(true)); // discarded
        block2.countDown();
        disc.shutdown();
        chk(disc.awaitTermination(T_SEC, TimeUnit.SECONDS), "discard pool terminated");
        chk(!ran.get(), "ThreadPoolExecutor DiscardPolicy drops task");
    }

    static void testScheduled() throws Exception {
        ScheduledExecutorService ses = Executors.newScheduledThreadPool(2);
        ScheduledFuture<Integer> sf = ses.schedule(() -> 42, 10, TimeUnit.MILLISECONDS);
        chk(sf.get(T_SEC, TimeUnit.SECONDS) == 42, "ScheduledExecutorService.schedule Callable");

        CountDownLatch rate = new CountDownLatch(3);
        ScheduledFuture<?> rf = ses.scheduleAtFixedRate(rate::countDown, 0, 20, TimeUnit.MILLISECONDS);
        chk(rate.await(T_SEC, TimeUnit.SECONDS), "scheduleAtFixedRate fires repeatedly");
        rf.cancel(false);
        chk(rf.isCancelled(), "scheduleAtFixedRate cancelled");

        CountDownLatch delay = new CountDownLatch(2);
        ScheduledFuture<?> df = ses.scheduleWithFixedDelay(delay::countDown, 0, 20, TimeUnit.MILLISECONDS);
        chk(delay.await(T_SEC, TimeUnit.SECONDS), "scheduleWithFixedDelay fires repeatedly");
        df.cancel(false);

        ScheduledFuture<?> future = ses.schedule(() -> {}, 1, TimeUnit.HOURS);
        chk(future.getDelay(TimeUnit.MINUTES) > 0, "ScheduledFuture.getDelay positive");
        future.cancel(false);
        ses.shutdownNow();
        chk(ses.awaitTermination(T_SEC, TimeUnit.SECONDS), "scheduled pool terminated");
    }

    static void testFutureTask() throws Exception {
        FutureTask<Integer> ft = new FutureTask<>(() -> 7);
        chk(!ft.isDone(), "FutureTask not done before run");
        ft.run();
        chk(ft.isDone() && ft.get() == 7, "FutureTask run/get");

        FutureTask<Integer> cancelled = new FutureTask<>(() -> 1);
        chk(cancelled.cancel(false) && cancelled.isCancelled() && cancelled.isDone(), "FutureTask cancel");

        FutureTask<Integer> fromRunnable = new FutureTask<>(() -> {}, 55);
        fromRunnable.run();
        chk(fromRunnable.get() == 55, "FutureTask from Runnable+result");
    }

    static void testInvokeAllAny() throws Exception {
        ExecutorService ex = Executors.newFixedThreadPool(4);
        List<Callable<Integer>> tasks = new ArrayList<>();
        for (int i = 1; i <= 5; i++) {
            final int v = i;
            tasks.add(() -> v);
        }
        List<Future<Integer>> fs = ex.invokeAll(tasks);
        int sum = 0;
        boolean allDone = true;
        for (Future<Integer> f : fs) {
            sum += f.get();
            allDone &= f.isDone();
        }
        chk(sum == 15 && allDone, "invokeAll all complete, sum 15");

        List<Callable<String>> same = new ArrayList<>();
        for (int i = 0; i < 3; i++) same.add(() -> "X");
        chk(ex.invokeAny(same).equals("X"), "invokeAny returns a completed result");
        ex.shutdown();
        chk(ex.awaitTermination(T_SEC, TimeUnit.SECONDS), "invokeAll/Any pool terminated");
    }

    static void testCompletionService() throws Exception {
        ExecutorService ex = Executors.newFixedThreadPool(3);
        ExecutorCompletionService<Integer> ecs = new ExecutorCompletionService<>(ex);
        for (int i = 1; i <= 5; i++) {
            final int v = i;
            ecs.submit(() -> v);
        }
        int sum = 0;
        for (int k = 0; k < 5; k++) sum += ecs.take().get();
        chk(sum == 15, "ExecutorCompletionService take/get sum");
        chk(ecs.poll() == null, "ExecutorCompletionService poll empty");
        ex.shutdown();
        chk(ex.awaitTermination(T_SEC, TimeUnit.SECONDS), "completion service pool terminated");
    }

    static void testThreadFactoryAndCancel() throws Exception {
        ThreadFactory tf = r -> {
            Thread t = new Thread(r);
            t.setName("carpet-worker");
            t.setDaemon(true);
            return t;
        };
        ExecutorService tfx = Executors.newSingleThreadExecutor(tf);
        chk(tfx.submit(() -> Thread.currentThread().getName()).get(T_SEC, TimeUnit.SECONDS).equals("carpet-worker"),
                "custom ThreadFactory naming");
        chk(tfx.submit(() -> Thread.currentThread().isDaemon()).get(T_SEC, TimeUnit.SECONDS),
                "custom ThreadFactory daemon flag");
        tfx.shutdown();
        chk(tfx.awaitTermination(T_SEC, TimeUnit.SECONDS), "thread-factory pool terminated");

        // Future.cancel a queued (not-yet-running) task on a busy single thread
        ExecutorService busy = Executors.newSingleThreadExecutor();
        CountDownLatch hold = new CountDownLatch(1);
        busy.submit(() -> {
            try { hold.await(); } catch (InterruptedException ignored) {}
        });
        Future<Integer> queued = busy.submit(() -> 1);
        chk(queued.cancel(false) && queued.isCancelled() && queued.isDone(), "Future.cancel queued task");
        boolean threw = false;
        try {
            queued.get();
        } catch (CancellationException e) {
            threw = true;
        }
        chk(threw, "cancelled Future.get throws CancellationException");
        hold.countDown();
        busy.shutdown();
        chk(busy.awaitTermination(T_SEC, TimeUnit.SECONDS), "busy pool terminated");
    }

    // -------------------------------------------------------- CompletableFuture
    static void testCompletableFuture() throws Exception {
        CompletableFuture<Integer> base = CompletableFuture.completedFuture(5);
        chk(base.get() == 5 && base.isDone() && !base.isCompletedExceptionally(), "CF.completedFuture");
        chk(base.getNow(-1) == 5, "CF.getNow");
        chk(base.thenApply(x -> x + 1).get() == 6, "CF.thenApply");

        AtomicInteger acc = new AtomicInteger();
        base.thenAccept(acc::set).get();
        chk(acc.get() == 5, "CF.thenAccept");
        AtomicInteger ran = new AtomicInteger();
        base.thenRun(ran::incrementAndGet).get();
        chk(ran.get() == 1, "CF.thenRun");

        chk(base.thenCompose(x -> CompletableFuture.completedFuture(x * 2)).get() == 10, "CF.thenCompose");
        chk(base.thenCombine(CompletableFuture.completedFuture(3), Integer::sum).get() == 8, "CF.thenCombine");

        AtomicInteger both = new AtomicInteger();
        base.thenAcceptBoth(CompletableFuture.completedFuture(3), (a, b) -> both.set(a + b)).get();
        chk(both.get() == 8, "CF.thenAcceptBoth");
        AtomicInteger ab = new AtomicInteger();
        base.runAfterBoth(CompletableFuture.completedFuture(3), ab::incrementAndGet).get();
        chk(ab.get() == 1, "CF.runAfterBoth");

        chk(base.applyToEither(CompletableFuture.completedFuture(5), x -> x * 10).get() == 50, "CF.applyToEither");
        AtomicInteger ae = new AtomicInteger();
        base.acceptEither(CompletableFuture.completedFuture(5), ae::set).get();
        chk(ae.get() == 5, "CF.acceptEither");
        AtomicInteger re = new AtomicInteger();
        base.runAfterEither(CompletableFuture.completedFuture(5), re::incrementAndGet).get();
        chk(re.get() == 1, "CF.runAfterEither");

        // async variants over the common pool
        chk(CompletableFuture.supplyAsync(() -> 10).thenApplyAsync(x -> x * 2)
                .thenComposeAsync(x -> CompletableFuture.supplyAsync(() -> x + 5))
                .thenCombineAsync(CompletableFuture.supplyAsync(() -> 100), Integer::sum)
                .get(T_SEC, TimeUnit.SECONDS) == 125, "CF async chain");
        AtomicInteger raCount = new AtomicInteger();
        CompletableFuture.runAsync(raCount::incrementAndGet).get();
        chk(raCount.get() == 1, "CF.runAsync");

        // error handling
        CompletableFuture<Integer> failed = new CompletableFuture<>();
        failed.completeExceptionally(new RuntimeException("boom"));
        chk(failed.isCompletedExceptionally(), "CF.completeExceptionally");
        chk(failed.exceptionally(t -> -1).get() == -1, "CF.exceptionally");
        chk(failed.handle((v, t) -> t != null ? 99 : v).get() == 99, "CF.handle error path");
        chk(CompletableFuture.completedFuture(4).handle((v, t) -> t != null ? 0 : v + 1).get() == 5,
                "CF.handle value path");

        AtomicReference<String> wc = new AtomicReference<>();
        CompletableFuture.completedFuture("z").whenComplete((v, t) -> wc.set(v)).get();
        chk(wc.get().equals("z"), "CF.whenComplete");

        CompletableFuture<Integer> manual = new CompletableFuture<>();
        chk(manual.complete(7) && manual.get() == 7, "CF.complete");
        chk(!manual.complete(8) && manual.get() == 7, "CF.complete second call ignored");

        CompletableFuture<Integer> a1 = CompletableFuture.completedFuture(1);
        CompletableFuture<Integer> a2 = CompletableFuture.completedFuture(2);
        CompletableFuture.allOf(a1, a2).get();
        chk(a1.get() + a2.get() == 3, "CF.allOf");
        Object any = CompletableFuture.anyOf(CompletableFuture.completedFuture("Q"),
                CompletableFuture.completedFuture("Q")).get();
        chk("Q".equals(any), "CF.anyOf");

        chk(CompletableFuture.failedFuture(new IllegalStateException()).isCompletedExceptionally(),
                "CF.failedFuture");

        boolean joinThrew = false;
        try {
            failed.join();
        } catch (CompletionException ce) {
            joinThrew = ce.getCause() instanceof RuntimeException;
        }
        chk(joinThrew, "CF.join wraps in CompletionException");

        boolean getThrew = false;
        try {
            failed.get();
        } catch (ExecutionException ee) {
            getThrew = "boom".equals(ee.getCause().getMessage());
        }
        chk(getThrew, "CF.get wraps in ExecutionException");
    }

    // ------------------------------------------------------------- fork/join
    static void testForkJoin() throws Exception {
        ForkJoinPool fj = new ForkJoinPool(4);
        long s = fj.invoke(new SumTask(1, 100_000));
        chk(s == 100_000L * 100_001L / 2, "ForkJoin RecursiveTask sum");

        int[] arr = new int[2000];
        fj.invoke(new IncAction(arr, 0, arr.length));
        boolean allOne = true;
        for (int v : arr) if (v != 1) { allOne = false; break; }
        chk(allOne, "ForkJoin RecursiveAction increments all");
        chk(fj.getParallelism() == 4, "ForkJoinPool.getParallelism");

        Future<Long> submitted = fj.submit(new SumTask(1, 100));
        chk(submitted.get(T_SEC, TimeUnit.SECONDS) == 5050L, "ForkJoinPool.submit");
        fj.shutdown();
        chk(fj.awaitTermination(T_SEC, TimeUnit.SECONDS), "fork/join pool terminated");

        chk(ForkJoinPool.commonPool().getParallelism() >= 1, "ForkJoinPool.commonPool parallelism");

        SumTask ta = new SumTask(1, 50);
        SumTask tb = new SumTask(51, 100);
        ForkJoinTask.invokeAll(ta, tb);
        chk(ta.join() + tb.join() == 5050L && ta.isCompletedNormally(),
                "ForkJoinTask.invokeAll + isCompletedNormally");
    }

    // ----------------------------------------------------- concurrent maps
    static void testConcurrentHashMap() {
        ConcurrentHashMap<String, Integer> m = new ConcurrentHashMap<>();
        chk(m.putIfAbsent("a", 1) == null && m.get("a") == 1, "CHM.putIfAbsent new");
        chk(m.putIfAbsent("a", 2) == 1 && m.get("a") == 1, "CHM.putIfAbsent existing");
        chk(m.getOrDefault("x", 99) == 99, "CHM.getOrDefault");
        m.merge("a", 10, Integer::sum);
        chk(m.get("a") == 11, "CHM.merge");
        m.compute("a", (k, v) -> v + 1);
        chk(m.get("a") == 12, "CHM.compute");
        m.computeIfAbsent("b", k -> 5);
        chk(m.get("b") == 5, "CHM.computeIfAbsent");
        m.computeIfPresent("b", (k, v) -> v * 2);
        chk(m.get("b") == 10, "CHM.computeIfPresent");
        chk(m.replace("b", 10, 20) && m.get("b") == 20, "CHM.replace 3-arg hit");
        chk(!m.replace("b", 999, 0) && m.get("b") == 20, "CHM.replace 3-arg miss");
        chk(m.replace("b", 30) == 20 && m.get("b") == 30, "CHM.replace 2-arg");
        chk(m.remove("b", 30) && !m.containsKey("b"), "CHM.remove 2-arg");
        chk(m.size() == 1 && m.keySet().contains("a"), "CHM size/keySet");

        ConcurrentHashMap<Integer, Integer> nm = new ConcurrentHashMap<>();
        for (int i = 1; i <= 10; i++) nm.put(i, i);
        long red = nm.reduceValuesToLong(1, v -> (long) v, 0L, Long::sum);
        chk(red == 55L, "CHM.reduceValuesToLong");
        Integer found = nm.searchValues(1, v -> v == 7 ? v : null);
        chk(found != null && found == 7, "CHM.searchValues");
        AtomicInteger feSum = new AtomicInteger();
        nm.forEach(1, (k, v) -> feSum.addAndGet(v));
        chk(feSum.get() == 55, "CHM.forEach (bulk)");
        Integer maxKey = nm.reduceKeys(1, Integer::max);
        chk(maxKey == 10, "CHM.reduceKeys");

        Set<String> ks = ConcurrentHashMap.newKeySet();
        chk(ks.add("x") && !ks.add("x") && ks.contains("x"), "CHM.newKeySet");
    }

    static void testSkipList() {
        ConcurrentSkipListMap<Integer, String> sl = new ConcurrentSkipListMap<>();
        for (int k : new int[]{10, 20, 30, 40, 50}) sl.put(k, "v" + k);
        chk(sl.firstKey() == 10 && sl.lastKey() == 50, "CSLMap first/last key");
        chk(sl.ceilingKey(25) == 30 && sl.floorKey(25) == 20, "CSLMap ceiling/floor");
        chk(sl.higherKey(30) == 40 && sl.lowerKey(30) == 20, "CSLMap higher/lower");
        chk(sl.headMap(30).size() == 2 && sl.tailMap(30).size() == 3, "CSLMap head/tail map");
        chk(sl.subMap(20, 40).keySet().equals(new TreeSet<>(Arrays.asList(20, 30))), "CSLMap subMap");
        chk(sl.descendingMap().firstKey() == 50, "CSLMap descendingMap");
        chk(sl.firstEntry().getValue().equals("v10"), "CSLMap firstEntry");
        Map.Entry<Integer, String> polled = sl.pollFirstEntry();
        chk(polled.getKey() == 10 && sl.firstKey() == 20, "CSLMap pollFirstEntry");

        ConcurrentSkipListSet<Integer> ss = new ConcurrentSkipListSet<>(Arrays.asList(3, 1, 2, 5, 4));
        chk(ss.first() == 1 && ss.last() == 5, "CSLSet first/last");
        chk(ss.ceiling(3) == 3 && ss.floor(3) == 3 && ss.higher(3) == 4 && ss.lower(3) == 2, "CSLSet navigation");
        chk(ss.headSet(3).size() == 2 && ss.tailSet(3).size() == 3, "CSLSet head/tail set");
        chk(ss.pollFirst() == 1 && ss.first() == 2, "CSLSet pollFirst");
        chk(ss.descendingSet().first() == 5, "CSLSet descendingSet");
    }

    static void testCopyOnWrite() {
        CopyOnWriteArrayList<Integer> cow = new CopyOnWriteArrayList<>();
        cow.add(1);
        cow.add(2);
        chk(!cow.addIfAbsent(2), "COWList.addIfAbsent existing");
        chk(cow.addIfAbsent(3) && cow.size() == 3, "COWList.addIfAbsent new");
        Iterator<Integer> snap = cow.iterator();
        cow.add(4); // must not affect already-created snapshot iterator
        int c = 0;
        while (snap.hasNext()) { snap.next(); c++; }
        chk(c == 3, "COWList snapshot iterator unaffected by later add");
        chk(cow.size() == 4, "COWList size after add");
        chk(cow.addAllAbsent(Arrays.asList(3, 4, 5)) == 1, "COWList.addAllAbsent only new");
        chk(cow.indexOf(5) == 4, "COWList.indexOf");

        CopyOnWriteArraySet<Integer> cos = new CopyOnWriteArraySet<>();
        chk(cos.add(1) && cos.add(2) && !cos.add(1), "COWSet.add dedup");
        chk(cos.size() == 2 && cos.contains(1), "COWSet size/contains");
    }

    static void testConcurrentQueuesDeques() {
        ConcurrentLinkedQueue<Integer> clq = new ConcurrentLinkedQueue<>();
        chk(clq.isEmpty() && clq.poll() == null, "CLQueue empty");
        chk(clq.offer(1) && clq.offer(2), "CLQueue.offer");
        chk(clq.peek() == 1 && clq.poll() == 1 && clq.size() == 1, "CLQueue FIFO peek/poll");

        ConcurrentLinkedDeque<Integer> cld = new ConcurrentLinkedDeque<>();
        cld.offerFirst(1);
        cld.offerLast(2);
        cld.offerFirst(0);
        chk(cld.peekFirst() == 0 && cld.peekLast() == 2, "CLDeque peekFirst/Last");
        chk(cld.pollFirst() == 0 && cld.pollLast() == 2 && cld.size() == 1, "CLDeque pollFirst/Last");
    }

    static void testBlockingQueues() throws Exception {
        LinkedBlockingQueue<Integer> lbq = new LinkedBlockingQueue<>(5);
        chk(lbq.remainingCapacity() == 5, "LBQ initial remainingCapacity");
        chk(lbq.offer(1) && lbq.offer(2) && lbq.size() == 2, "LBQ.offer");
        List<Integer> drained = new ArrayList<>();
        chk(lbq.drainTo(drained) == 2 && drained.equals(Arrays.asList(1, 2)), "LBQ.drainTo");
        chk(lbq.poll() == null, "LBQ empty poll");
        lbq.put(10);
        chk(lbq.take() == 10, "LBQ put/take");

        LinkedBlockingQueue<Integer> cap1 = new LinkedBlockingQueue<>(1);
        cap1.put(1);
        chk(!cap1.offer(2), "LBQ.offer when full");
        chk(!cap1.offer(2, 50, TimeUnit.MILLISECONDS), "LBQ timed offer when full");

        ArrayBlockingQueue<Integer> abq = new ArrayBlockingQueue<>(3);
        chk(abq.offer(1) && abq.offer(2) && abq.offer(3), "ABQ.offer fills");
        chk(!abq.offer(4) && abq.remainingCapacity() == 0, "ABQ full");
        chk(abq.poll() == 1 && abq.peek() == 2, "ABQ FIFO");

        PriorityBlockingQueue<Integer> pbq = new PriorityBlockingQueue<>();
        pbq.addAll(Arrays.asList(5, 1, 3, 2, 4));
        chk(pbq.poll() == 1 && pbq.poll() == 2 && pbq.poll() == 3, "PBQ natural ordering");
        PriorityBlockingQueue<Integer> pbqr = new PriorityBlockingQueue<>(11, Comparator.reverseOrder());
        pbqr.addAll(Arrays.asList(1, 2, 3));
        chk(pbqr.poll() == 3 && pbqr.poll() == 2, "PBQ reverse comparator");

        LinkedBlockingDeque<Integer> dq = new LinkedBlockingDeque<>();
        dq.putFirst(1);
        dq.putLast(2);
        dq.putFirst(0);
        chk(dq.peekFirst() == 0 && dq.peekLast() == 2, "LBDeque peekFirst/Last");
        chk(dq.takeFirst() == 0 && dq.takeLast() == 2 && dq.takeFirst() == 1, "LBDeque takeFirst/Last");
    }

    static void testSynchronousQueue() throws Exception {
        SynchronousQueue<Integer> sq = new SynchronousQueue<>();
        chk(!sq.offer(1) && sq.isEmpty(), "SynchronousQueue.offer without consumer");
        ExecutorService ex = Executors.newSingleThreadExecutor();
        Future<Integer> consumer = ex.submit(() -> {
            try { return sq.take(); } catch (InterruptedException e) { return -1; }
        });
        sq.put(99); // blocks until consumer takes
        chk(consumer.get(T_SEC, TimeUnit.SECONDS) == 99, "SynchronousQueue put/take handoff");
        ex.shutdown();
        chk(ex.awaitTermination(T_SEC, TimeUnit.SECONDS), "synchronous queue pool terminated");
    }

    static void testTransferQueue() throws Exception {
        LinkedTransferQueue<Integer> ltq = new LinkedTransferQueue<>();
        chk(!ltq.hasWaitingConsumer() && ltq.getWaitingConsumerCount() == 0, "LTQ no waiting consumer");
        chk(!ltq.tryTransfer(1) && ltq.isEmpty(), "LTQ.tryTransfer without consumer (not enqueued)");
        ltq.put(5);
        chk(ltq.poll() == 5, "LTQ put/poll");

        ExecutorService ex = Executors.newSingleThreadExecutor();
        Future<Integer> consumer = ex.submit(() -> {
            try { return ltq.take(); } catch (InterruptedException e) { return -1; }
        });
        ltq.transfer(77); // blocks until consumed
        chk(consumer.get(T_SEC, TimeUnit.SECONDS) == 77, "LTQ.transfer handoff");
        ex.shutdown();
        chk(ex.awaitTermination(T_SEC, TimeUnit.SECONDS), "transfer queue pool terminated");
    }

    static void testDelayQueue() throws Exception {
        DelayQueue<DelayedItem> dq = new DelayQueue<>();
        dq.put(new DelayedItem("c", 60));
        dq.put(new DelayedItem("a", 20));
        dq.put(new DelayedItem("b", 40));
        chk(dq.size() == 3 && dq.poll() == null, "DelayQueue poll before expiry returns null");
        chk(dq.take().name.equals("a"), "DelayQueue.take earliest first (a)");
        chk(dq.take().name.equals("b"), "DelayQueue.take second (b)");
        chk(dq.take().name.equals("c"), "DelayQueue.take last (c)");
        chk(dq.isEmpty(), "DelayQueue drained");
    }

    // -------------------------------------------------------------- misc utils
    static void testThreadLocalAndRandom() throws Exception {
        ThreadLocal<Integer> tl = ThreadLocal.withInitial(() -> 100);
        chk(tl.get() == 100, "ThreadLocal initial value");
        tl.set(5);
        chk(tl.get() == 5, "ThreadLocal.set");
        AtomicInteger childView = new AtomicInteger(-1);
        Thread child = new Thread(() -> childView.set(tl.get()));
        child.start();
        child.join();
        chk(childView.get() == 100, "ThreadLocal isolated per-thread (child sees initial)");
        chk(tl.get() == 5, "ThreadLocal main unchanged by child");
        tl.remove();
        chk(tl.get() == 100, "ThreadLocal.remove resets to initial");

        InheritableThreadLocal<String> itl = new InheritableThreadLocal<>();
        itl.set("parent");
        AtomicReference<String> childInh = new AtomicReference<>();
        Thread c2 = new Thread(() -> childInh.set(itl.get()));
        c2.start();
        c2.join();
        chk("parent".equals(childInh.get()), "InheritableThreadLocal inherited by child");

        ThreadLocalRandom tlr = ThreadLocalRandom.current();
        chk(tlr.nextInt(5, 6) == 5, "TLR.nextInt(origin,bound) single value");
        chk(tlr.nextInt(1) == 0, "TLR.nextInt(1) is 0");
        int ri = tlr.nextInt(10, 20);
        chk(ri >= 10 && ri < 20, "TLR.nextInt range membership");
        long rl = tlr.nextLong(100);
        chk(rl >= 0 && rl < 100, "TLR.nextLong bound");
        chk(tlr.nextLong(7, 8) == 7L, "TLR.nextLong(origin,bound) single value");
        double rd = tlr.nextDouble();
        chk(rd >= 0.0 && rd < 1.0, "TLR.nextDouble range");
        double rd2 = tlr.nextDouble(2.0, 2.0 + 1e-12);
        chk(rd2 >= 2.0 && rd2 < 2.0 + 1e-12, "TLR.nextDouble(origin,bound)");
    }

    static void testTimeUnit() {
        chk(TimeUnit.SECONDS.toMillis(2) == 2000L, "TimeUnit SECONDS->millis");
        chk(TimeUnit.MINUTES.toSeconds(3) == 180L, "TimeUnit MINUTES->seconds");
        chk(TimeUnit.MILLISECONDS.convert(1, TimeUnit.SECONDS) == 1000L, "TimeUnit.convert");
        chk(TimeUnit.HOURS.toMinutes(2) == 120L, "TimeUnit HOURS->minutes");
        chk(TimeUnit.DAYS.toHours(1) == 24L, "TimeUnit DAYS->hours");
        chk(TimeUnit.NANOSECONDS.toMicros(1000) == 1L, "TimeUnit NANOS->micros");
        chk(TimeUnit.SECONDS.toNanos(1) == 1_000_000_000L, "TimeUnit SECONDS->nanos");
    }

    // ----------------------------------------------------------- deep stress
    static void testDeepStress() throws Exception {
        // 1. high-contention counters: 8 threads x 5000 incs on atomic + lock-guarded
        final int T = 8, N = 5000;
        AtomicLong atom = new AtomicLong();
        final long[] guarded = {0};
        ReentrantLock lock = new ReentrantLock();
        CountDownLatch start = new CountDownLatch(1), done = new CountDownLatch(T);
        ExecutorService ex = Executors.newFixedThreadPool(T);
        for (int t = 0; t < T; t++) {
            ex.submit(() -> {
                try { start.await(); } catch (InterruptedException ignored) {}
                for (int i = 0; i < N; i++) {
                    atom.incrementAndGet();
                    lock.lock();
                    try { guarded[0]++; } finally { lock.unlock(); }
                }
                done.countDown();
            });
        }
        start.countDown();
        chk(done.await(T_SEC * 2, TimeUnit.SECONDS), "stress 8-thread latch completes");
        chk(atom.get() == (long) T * N, "stress atomic exact " + atom.get());
        chk(guarded[0] == (long) T * N, "stress lock-guarded exact " + guarded[0]);
        ex.shutdown();
        chk(ex.awaitTermination(T_SEC, TimeUnit.SECONDS), "stress counter pool terminated");

        // 2. parallel stream sum -> Gauss closed form
        long n = 100_000L;
        long sum = LongStream.rangeClosed(1, n).parallel().sum();
        chk(sum == n * (n + 1) / 2, "stress parallelStream sum");
        long count = IntStream.range(0, 50_000).parallel().filter(i -> (i & 1) == 0).count();
        chk(count == 25_000L, "stress parallel filter count");

        // 3. producer/consumer over BlockingQueue: 2 prod x 2 cons x 2500 items
        BlockingQueue<Integer> q = new LinkedBlockingQueue<>(256);
        AtomicInteger consumed = new AtomicInteger();
        final int P = 2, C = 2, items = 2500;
        ExecutorService pc = Executors.newFixedThreadPool(P + C);
        CountDownLatch pdone = new CountDownLatch(P);
        for (int p = 0; p < P; p++) {
            pc.submit(() -> {
                try { for (int i = 0; i < items; i++) q.put(i); } catch (InterruptedException ignored) {}
                pdone.countDown();
            });
        }
        AtomicBoolean producing = new AtomicBoolean(true);
        List<Future<?>> cons = new ArrayList<>();
        for (int c = 0; c < C; c++) {
            cons.add(pc.submit(() -> {
                try {
                    while (producing.get() || !q.isEmpty()) {
                        Integer v = q.poll(50, TimeUnit.MILLISECONDS);
                        if (v != null) consumed.incrementAndGet();
                    }
                } catch (InterruptedException ignored) {}
            }));
        }
        chk(pdone.await(T_SEC * 2, TimeUnit.SECONDS), "stress producers done");
        producing.set(false);
        for (Future<?> f : cons) f.get(T_SEC, TimeUnit.SECONDS);
        chk(consumed.get() == P * items, "stress producer/consumer all consumed " + consumed.get());
        pc.shutdown();
        chk(pc.awaitTermination(T_SEC, TimeUnit.SECONDS), "stress pc pool terminated");

        // 4. ConcurrentHashMap under contention: 4 threads merge-increment 50 keys
        ConcurrentHashMap<Integer, Integer> chm = new ConcurrentHashMap<>();
        final int M = 4, perKey = 50;
        ExecutorService ce = Executors.newFixedThreadPool(M);
        CountDownLatch cl = new CountDownLatch(M);
        for (int t = 0; t < M; t++) {
            ce.submit(() -> {
                for (int i = 0; i < 50 * perKey; i++) chm.merge(i % 50, 1, Integer::sum);
                cl.countDown();
            });
        }
        chk(cl.await(T_SEC * 2, TimeUnit.SECONDS), "stress chm latch");
        chk(chm.values().stream().mapToInt(Integer::intValue).sum() == M * 50 * perKey, "stress chm merge exact");
        ce.shutdown();
        chk(ce.awaitTermination(T_SEC, TimeUnit.SECONDS), "stress chm pool terminated");
    }

    // ------------------------------------------------------------- helper types
    static final class FieldHolder {
        volatile int iv;
        volatile long lv;
        volatile String rv;
    }

    static final class SumTask extends RecursiveTask<Long> {
        final int lo, hi;
        SumTask(int lo, int hi) { this.lo = lo; this.hi = hi; }
        protected Long compute() {
            if (hi - lo <= 5000) {
                long s = 0;
                for (int i = lo; i <= hi; i++) s += i;
                return s;
            }
            int mid = (lo + hi) >>> 1;
            SumTask l = new SumTask(lo, mid);
            l.fork();
            SumTask r = new SumTask(mid + 1, hi);
            return r.compute() + l.join();
        }
    }

    static final class IncAction extends RecursiveAction {
        final int[] a;
        final int lo, hi;
        IncAction(int[] a, int lo, int hi) { this.a = a; this.lo = lo; this.hi = hi; }
        protected void compute() {
            if (hi - lo <= 200) {
                for (int i = lo; i < hi; i++) a[i]++;
                return;
            }
            int mid = (lo + hi) >>> 1;
            invokeAll(new IncAction(a, lo, mid), new IncAction(a, mid, hi));
        }
    }

    static final class DelayedItem implements Delayed {
        final String name;
        final long deadlineNanos;
        DelayedItem(String name, long delayMs) {
            this.name = name;
            this.deadlineNanos = System.nanoTime() + delayMs * 1_000_000L;
        }
        public long getDelay(TimeUnit unit) {
            return unit.convert(deadlineNanos - System.nanoTime(), TimeUnit.NANOSECONDS);
        }
        public int compareTo(Delayed o) {
            return Long.compare(getDelay(TimeUnit.NANOSECONDS), o.getDelay(TimeUnit.NANOSECONDS));
        }
    }
}
