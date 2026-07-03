import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.*;
import java.util.concurrent.locks.*;
import java.util.stream.*;
import java.util.function.*;

/* Carpet-grade coverage of java.util.concurrent + .atomic + .locks + java.lang threading.
 * Deterministic, offline, no external resources. Aggregate-value assertions only
 * (no reliance on scheduling order). Thread count kept small; threads joined per section. */
public class ConcurrencyTest {
    static int ok = 0, fail = 0;

    static synchronized void check(boolean c, String n) {
        if (c) ok++;
        else { fail++; System.out.println("FAIL " + n); }
    }
    static synchronized void eq(long a, long b, String n) {
        if (a == b) ok++;
        else { fail++; System.out.println("FAIL " + n + " expected=" + b + " actual=" + a); }
    }
    static synchronized void eqd(double a, double b, String n) {
        if (a == b) ok++;
        else { fail++; System.out.println("FAIL " + n + " expected=" + b + " actual=" + a); }
    }

    interface Job { void run() throws Throwable; }
    static void sec(String name, Job j) {
        try { j.run(); }
        catch (Throwable t) { fail++; System.out.println("FAIL sec:" + name + " ex=" + t); }
    }

    static void sleepQuiet(long ms) {
        try { Thread.sleep(ms); } catch (InterruptedException e) { Thread.currentThread().interrupt(); }
    }

    // ---- helper classes for field updaters / delay queue ----
    static class FieldHolder {
        volatile int iv = 1;
        volatile long lv = 2L;
        volatile String sv = "init";
    }
    static class DItem implements Delayed {
        final int id;
        final long deadline;
        DItem(int id, long delayMs) {
            this.id = id;
            this.deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(delayMs);
        }
        public long getDelay(TimeUnit u) { return u.convert(deadline - System.nanoTime(), TimeUnit.NANOSECONDS); }
        public int compareTo(Delayed o) { return Long.compare(this.deadline, ((DItem) o).deadline); }
    }
    static final class FibTask extends RecursiveTask<Integer> {
        final int n;
        FibTask(int n) { this.n = n; }
        protected Integer compute() {
            if (n <= 1) return n;
            FibTask a = new FibTask(n - 1);
            a.fork();
            FibTask b = new FibTask(n - 2);
            int br = b.compute();
            return a.join() + br;
        }
    }
    static final class IncAction extends RecursiveAction {
        final int[] arr; final int lo, hi;
        IncAction(int[] arr, int lo, int hi) { this.arr = arr; this.lo = lo; this.hi = hi; }
        protected void compute() {
            if (hi - lo <= 8) { for (int i = lo; i < hi; i++) arr[i]++; return; }
            int mid = (lo + hi) >>> 1;
            invokeAll(new IncAction(arr, lo, mid), new IncAction(arr, mid, hi));
        }
    }

    public static void main(String[] args) throws Exception {
        final ExecutorService pool = Executors.newFixedThreadPool(4);

        // ================= java.util.concurrent.atomic =================
        sec("AtomicInteger", () -> {
            AtomicInteger ai = new AtomicInteger(5);
            eq(ai.get(), 5, "ai-init");
            eq(ai.getAndIncrement(), 5, "ai-getAndInc-ret");
            eq(ai.get(), 6, "ai-getAndInc-val");
            eq(ai.incrementAndGet(), 7, "ai-incAndGet");
            eq(ai.getAndAdd(3), 7, "ai-getAndAdd-ret");
            eq(ai.get(), 10, "ai-getAndAdd-val");
            eq(ai.addAndGet(-4), 6, "ai-addAndGet");
            eq(ai.getAndSet(100), 6, "ai-getAndSet-ret");
            eq(ai.get(), 100, "ai-getAndSet-val");
            check(ai.compareAndSet(100, 200), "ai-cas-true");
            eq(ai.get(), 200, "ai-cas-val");
            check(!ai.compareAndSet(100, 300), "ai-cas-false");
            eq(ai.get(), 200, "ai-cas-unchanged");
            eq(ai.updateAndGet(x -> x + 1), 201, "ai-updateAndGet");
            eq(ai.getAndUpdate(x -> x * 2), 201, "ai-getAndUpdate-ret");
            eq(ai.get(), 402, "ai-getAndUpdate-val");
            eq(ai.accumulateAndGet(8, Integer::sum), 410, "ai-accAndGet");
            eq(ai.getAndAccumulate(0, (a, b) -> a - 100), 410, "ai-getAndAcc-ret");
            eq(ai.get(), 310, "ai-getAndAcc-val");
            eq(ai.decrementAndGet(), 309, "ai-decAndGet");
            eq(ai.getAndDecrement(), 309, "ai-getAndDec-ret");
            eq(ai.get(), 308, "ai-getAndDec-val");
            eq(ai.intValue(), 308, "ai-intValue");
            eq(ai.longValue(), 308L, "ai-longValue");
            eqd(ai.doubleValue(), 308.0, "ai-doubleValue");
        });

        sec("AtomicLong", () -> {
            AtomicLong al = new AtomicLong(1_000_000_000_000L);
            eq(al.get(), 1_000_000_000_000L, "al-init");
            eq(al.incrementAndGet(), 1_000_000_000_001L, "al-inc");
            eq(al.addAndGet(9L), 1_000_000_000_010L, "al-add");
            check(al.compareAndSet(1_000_000_000_010L, 42L), "al-cas");
            eq(al.getAndUpdate(x -> x * 2), 42L, "al-getAndUpdate");
            eq(al.get(), 84L, "al-after-update");
            eq(al.accumulateAndGet(16L, Long::max), 84L, "al-accMax");
        });

        sec("AtomicBoolean", () -> {
            AtomicBoolean ab = new AtomicBoolean();
            check(!ab.get(), "ab-init-false");
            check(ab.compareAndSet(false, true), "ab-cas-true");
            check(ab.get(), "ab-now-true");
            check(!ab.compareAndSet(false, true), "ab-cas-false");
            check(ab.getAndSet(false), "ab-getAndSet-ret");
            check(!ab.get(), "ab-final-false");
        });

        sec("AtomicReference", () -> {
            AtomicReference<String> ar = new AtomicReference<>("a");
            check(ar.get().equals("a"), "ar-init");
            check(ar.compareAndSet("a", "b"), "ar-cas");
            check(ar.getAndSet("c").equals("b"), "ar-getAndSet");
            check(ar.updateAndGet(s -> s + "x").equals("cx"), "ar-updateAndGet");
            check(ar.accumulateAndGet("y", (s, t) -> s + t).equals("cxy"), "ar-accAndGet");
            check(ar.get().equals("cxy"), "ar-final");
            AtomicReference<Integer> nil = new AtomicReference<>();
            check(nil.get() == null, "ar-null-init");
            check(nil.compareAndSet(null, 1), "ar-cas-from-null");
        });

        sec("AtomicIntegerArray", () -> {
            AtomicIntegerArray aia = new AtomicIntegerArray(4);
            eq(aia.length(), 4, "aia-length");
            eq(aia.get(0), 0, "aia-default0");
            aia.set(1, 10);
            eq(aia.get(1), 10, "aia-set");
            eq(aia.incrementAndGet(1), 11, "aia-inc");
            eq(aia.getAndAdd(1, 5), 11, "aia-getAndAdd");
            eq(aia.get(1), 16, "aia-add-val");
            check(aia.compareAndSet(1, 16, 99), "aia-cas");
            eq(aia.updateAndGet(2, x -> x + 7), 7, "aia-updateAndGet");
        });

        sec("AtomicLongArray+ReferenceArray", () -> {
            AtomicLongArray ala = new AtomicLongArray(3);
            ala.set(0, 5L);
            eq(ala.addAndGet(0, 10L), 15L, "ala-add");
            eq(ala.length(), 3, "ala-length");
            AtomicReferenceArray<String> ara = new AtomicReferenceArray<>(new String[]{"a", "b", "c"});
            eq(ara.length(), 3, "ara-length");
            check(ara.get(1).equals("b"), "ara-get");
            check(ara.compareAndSet(1, "b", "z"), "ara-cas");
            check(ara.getAndSet(2, "w").equals("c"), "ara-getAndSet");
            check(ara.get(2).equals("w"), "ara-final");
        });

        sec("LongAdder", () -> {
            LongAdder la = new LongAdder();
            la.add(5);
            la.increment();
            la.increment();
            la.add(3);
            eq(la.sum(), 10L, "la-sum");
            la.decrement();
            eq(la.sum(), 9L, "la-dec");
            eq(la.intValue(), 9, "la-intValue");
            la.reset();
            eq(la.sum(), 0L, "la-reset");
            // concurrent
            final LongAdder cla = new LongAdder();
            Thread[] ts = new Thread[4];
            for (int i = 0; i < 4; i++) ts[i] = new Thread(() -> { for (int j = 0; j < 2000; j++) cla.increment(); });
            for (Thread t : ts) t.start();
            for (Thread t : ts) t.join();
            eq(cla.sum(), 8000L, "la-concurrent");
        });

        sec("Accumulators", () -> {
            LongAccumulator max = new LongAccumulator(Long::max, Long.MIN_VALUE);
            max.accumulate(5); max.accumulate(9); max.accumulate(3);
            eq(max.get(), 9L, "lacc-max");
            max.reset();
            eq(max.get(), Long.MIN_VALUE, "lacc-reset");
            DoubleAdder da = new DoubleAdder();
            da.add(1.5); da.add(2.5);
            eqd(da.sum(), 4.0, "dadder-sum");
            DoubleAccumulator dacc = new DoubleAccumulator(Double::sum, 0.0);
            dacc.accumulate(1.25); dacc.accumulate(2.75);
            eqd(dacc.get(), 4.0, "dacc-sum");
        });

        sec("FieldUpdaters", () -> {
            AtomicIntegerFieldUpdater<FieldHolder> ivu = AtomicIntegerFieldUpdater.newUpdater(FieldHolder.class, "iv");
            AtomicLongFieldUpdater<FieldHolder> lvu = AtomicLongFieldUpdater.newUpdater(FieldHolder.class, "lv");
            AtomicReferenceFieldUpdater<FieldHolder, String> svu =
                AtomicReferenceFieldUpdater.newUpdater(FieldHolder.class, String.class, "sv");
            FieldHolder h = new FieldHolder();
            eq(ivu.get(h), 1, "ifu-get");
            eq(ivu.incrementAndGet(h), 2, "ifu-inc");
            check(ivu.compareAndSet(h, 2, 50), "ifu-cas");
            eq(ivu.getAndAdd(h, 5), 50, "ifu-getAndAdd");
            eq(ivu.get(h), 55, "ifu-final");
            eq(lvu.addAndGet(h, 8L), 10L, "lfu-add");
            check(svu.compareAndSet(h, "init", "next"), "rfu-cas");
            check(svu.get(h).equals("next"), "rfu-get");
            check(svu.getAndSet(h, "last").equals("next"), "rfu-getAndSet");
        });

        sec("StampedAndMarkableReference", () -> {
            AtomicStampedReference<String> asr = new AtomicStampedReference<>("v0", 0);
            check(asr.getReference().equals("v0"), "asr-ref");
            eq(asr.getStamp(), 0, "asr-stamp");
            check(asr.compareAndSet("v0", "v1", 0, 1), "asr-cas");
            eq(asr.getStamp(), 1, "asr-stamp1");
            check(!asr.compareAndSet("v0", "v2", 1, 2), "asr-cas-badref");
            int[] holder = new int[1];
            check(asr.get(holder).equals("v1") && holder[0] == 1, "asr-get-holder");
            check(asr.attemptStamp("v1", 7), "asr-attemptStamp");
            eq(asr.getStamp(), 7, "asr-stamp7");

            AtomicMarkableReference<String> amr = new AtomicMarkableReference<>("m0", false);
            check(amr.getReference().equals("m0"), "amr-ref");
            check(!amr.isMarked(), "amr-unmarked");
            check(amr.compareAndSet("m0", "m1", false, true), "amr-cas");
            check(amr.isMarked(), "amr-marked");
            boolean[] mh = new boolean[1];
            check(amr.get(mh).equals("m1") && mh[0], "amr-get-holder");
            check(amr.attemptMark("m1", false), "amr-attemptMark");
            check(!amr.isMarked(), "amr-final-unmarked");
        });

        // ================= java.util.concurrent.locks =================
        sec("ReentrantLock", () -> {
            ReentrantLock rl = new ReentrantLock();
            check(!rl.isLocked(), "rl-init-unlocked");
            check(!rl.isFair(), "rl-not-fair");
            rl.lock();
            check(rl.isLocked(), "rl-locked");
            check(rl.isHeldByCurrentThread(), "rl-held");
            eq(rl.getHoldCount(), 1, "rl-hold1");
            rl.lock();
            eq(rl.getHoldCount(), 2, "rl-hold2");
            rl.unlock();
            eq(rl.getHoldCount(), 1, "rl-hold-back1");
            rl.unlock();
            eq(rl.getHoldCount(), 0, "rl-hold0");
            check(!rl.isLocked(), "rl-unlocked");
            check(rl.tryLock(), "rl-tryLock");
            rl.unlock();
            check(rl.tryLock(1, TimeUnit.SECONDS), "rl-tryLock-timed");
            rl.unlock();
            check(new ReentrantLock(true).isFair(), "rl-fair");
        });

        sec("ReentrantReadWriteLock", () -> {
            ReentrantReadWriteLock rw = new ReentrantReadWriteLock();
            rw.readLock().lock();
            eq(rw.getReadLockCount(), 1, "rw-readcount1");
            eq(rw.getReadHoldCount(), 1, "rw-readhold1");
            rw.readLock().lock();
            eq(rw.getReadHoldCount(), 2, "rw-readhold2");
            rw.readLock().unlock();
            rw.readLock().unlock();
            eq(rw.getReadLockCount(), 0, "rw-readcount0");
            rw.writeLock().lock();
            check(rw.isWriteLocked(), "rw-write-locked");
            check(rw.isWriteLockedByCurrentThread(), "rw-write-held");
            eq(rw.getWriteHoldCount(), 1, "rw-writehold1");
            rw.writeLock().unlock();
            check(!rw.isWriteLocked(), "rw-write-unlocked");
            check(!rw.isFair(), "rw-not-fair");
        });

        sec("StampedLock", () -> {
            StampedLock sl = new StampedLock();
            long w = sl.writeLock();
            check(sl.isWriteLocked(), "sl-write-locked");
            sl.unlockWrite(w);
            check(!sl.isWriteLocked(), "sl-write-unlocked");
            long r = sl.readLock();
            eq(sl.getReadLockCount(), 1, "sl-readcount1");
            sl.unlockRead(r);
            // optimistic read invalidated by write
            long opt = sl.tryOptimisticRead();
            check(opt != 0L, "sl-opt-nonzero");
            check(sl.validate(opt), "sl-opt-valid");
            long w2 = sl.writeLock();
            check(!sl.validate(opt), "sl-opt-invalid-after-write");
            sl.unlockWrite(w2);
            // convert read -> write
            long rs = sl.readLock();
            long conv = sl.tryConvertToWriteLock(rs);
            check(conv != 0L, "sl-convert-ok");
            check(sl.isWriteLocked(), "sl-converted-write");
            sl.unlockWrite(conv);
        });

        sec("Condition-bounded-buffer", () -> {
            final ReentrantLock lk = new ReentrantLock();
            final Condition notFull = lk.newCondition();
            final Condition notEmpty = lk.newCondition();
            final ArrayDeque<Integer> buf = new ArrayDeque<>();
            final int CAP = 4, N = 50;
            final AtomicInteger produced = new AtomicInteger(), consumed = new AtomicInteger();
            final long[] sumHolder = new long[1];
            Thread p = new Thread(() -> {
                for (int i = 1; i <= N; i++) {
                    lk.lock();
                    try {
                        while (buf.size() == CAP) notFull.await();
                        buf.addLast(i);
                        produced.incrementAndGet();
                        notEmpty.signal();
                    } catch (InterruptedException e) { return; }
                    finally { lk.unlock(); }
                }
            });
            Thread c = new Thread(() -> {
                for (int i = 0; i < N; i++) {
                    lk.lock();
                    try {
                        while (buf.isEmpty()) notEmpty.await();
                        sumHolder[0] += buf.pollFirst();
                        consumed.incrementAndGet();
                        notFull.signal();
                    } catch (InterruptedException e) { return; }
                    finally { lk.unlock(); }
                }
            });
            c.start(); p.start();
            p.join(10000); c.join(10000);
            eq(produced.get(), N, "cond-produced");
            eq(consumed.get(), N, "cond-consumed");
            eq(sumHolder[0], (long) N * (N + 1) / 2, "cond-sum");
        });

        sec("LockSupport", () -> {
            final boolean[] ran = new boolean[1];
            Thread t = new Thread(() -> {
                // permit made available before park -> park returns immediately
                LockSupport.unpark(Thread.currentThread());
                LockSupport.park();
                ran[0] = true;
            });
            t.start();
            t.join(5000);
            check(ran[0], "locksupport-permit");
            // parkNanos returns after timeout deterministically
            long start = System.nanoTime();
            LockSupport.parkNanos(2_000_000L);
            check(System.nanoTime() - start >= 0, "locksupport-parkNanos");
            // unpark a blocked thread
            final boolean[] woke = new boolean[1];
            Thread blocked = new Thread(() -> { LockSupport.park(); woke[0] = true; });
            blocked.start();
            sleepQuiet(50);
            LockSupport.unpark(blocked);
            blocked.join(5000);
            check(woke[0], "locksupport-unpark-blocked");
        });

        // ================= ExecutorService / Future =================
        sec("ExecutorService-Future", () -> {
            List<Future<Integer>> fs = new ArrayList<>();
            for (int i = 1; i <= 8; i++) { final int k = i; fs.add(pool.submit(() -> k * k)); }
            int sum = 0;
            for (Future<Integer> f : fs) { sum += f.get(); check(f.isDone(), "future-done"); }
            eq(sum, 204, "executor-future-sum");

            // submit Runnable with result
            Future<String> fr = pool.submit(() -> {}, "RESULT");
            check(fr.get().equals("RESULT"), "submit-runnable-result");

            // invokeAll
            List<Callable<Integer>> tasks = Arrays.asList(() -> 1, () -> 2, () -> 3, () -> 4);
            List<Future<Integer>> done = pool.invokeAll(tasks);
            int s2 = 0;
            for (Future<Integer> f : done) s2 += f.get();
            eq(s2, 10, "invokeAll-sum");

            // invokeAny (all yield 42)
            List<Callable<Integer>> same = Arrays.asList(() -> 42, () -> 42, () -> 42);
            eq(pool.invokeAny(same), 42, "invokeAny");

            // TimeoutException
            Future<Integer> slow = pool.submit(() -> { sleepQuiet(400); return 1; });
            try { slow.get(20, TimeUnit.MILLISECONDS); check(false, "timeout-should-throw"); }
            catch (TimeoutException te) { check(true, "timeout-thrown"); }
            slow.cancel(true);
            check(slow.isCancelled(), "future-cancelled");

            // ExecutionException wraps cause
            Future<Integer> boom = pool.submit(() -> { throw new IllegalStateException("boom"); });
            try { boom.get(); check(false, "ee-should-throw"); }
            catch (ExecutionException ee) { check(ee.getCause() instanceof IllegalStateException, "ee-cause"); }
        });

        sec("Executors-factories", () -> {
            ExecutorService single = Executors.newSingleThreadExecutor();
            eq(single.submit(() -> 7).get(), 7, "single-thread");
            single.shutdown();
            check(single.awaitTermination(5, TimeUnit.SECONDS), "single-terminated");
            check(single.isShutdown(), "single-isShutdown");
            check(single.isTerminated(), "single-isTerminated");

            ExecutorService cached = Executors.newCachedThreadPool();
            eq(cached.submit(() -> 8).get(), 8, "cached");
            cached.shutdown();

            ExecutorService stealing = Executors.newWorkStealingPool();
            List<Callable<Integer>> ts = new ArrayList<>();
            for (int i = 1; i <= 10; i++) { final int k = i; ts.add(() -> k); }
            int total = 0;
            for (Future<Integer> f : stealing.invokeAll(ts)) total += f.get();
            eq(total, 55, "stealing-sum");
            stealing.shutdown();

            // Executors.callable wraps Runnable
            final int[] flag = new int[1];
            Runnable rb = () -> flag[0] = 99;
            Callable<Object> wrapped = Executors.callable(rb);
            wrapped.call();
            eq(flag[0], 99, "executors-callable");
        });

        sec("ScheduledExecutor", () -> {
            ScheduledExecutorService ses = Executors.newScheduledThreadPool(2);
            ScheduledFuture<Integer> sf = ses.schedule(() -> 123, 20, TimeUnit.MILLISECONDS);
            eq(sf.get(), 123, "scheduled-callable");

            final CountDownLatch latch = new CountDownLatch(3);
            final AtomicInteger ticks = new AtomicInteger();
            ScheduledFuture<?> rate = ses.scheduleAtFixedRate(() -> {
                ticks.incrementAndGet();
                latch.countDown();
            }, 0, 15, TimeUnit.MILLISECONDS);
            check(latch.await(3, TimeUnit.SECONDS), "fixedrate-fired");
            rate.cancel(false);
            check(ticks.get() >= 3, "fixedrate-count");

            final AtomicInteger delayTicks = new AtomicInteger();
            final CountDownLatch dl = new CountDownLatch(2);
            ScheduledFuture<?> wfd = ses.scheduleWithFixedDelay(() -> { delayTicks.incrementAndGet(); dl.countDown(); },
                0, 15, TimeUnit.MILLISECONDS);
            check(dl.await(3, TimeUnit.SECONDS), "fixeddelay-fired");
            wfd.cancel(false);
            ses.shutdownNow();
        });

        sec("ThreadPoolExecutor-detail", () -> {
            ThreadPoolExecutor tpe = new ThreadPoolExecutor(2, 4, 1, TimeUnit.SECONDS,
                new LinkedBlockingQueue<>());
            eq(tpe.getCorePoolSize(), 2, "tpe-core");
            eq(tpe.getMaximumPoolSize(), 4, "tpe-max");
            List<Future<Integer>> fs = new ArrayList<>();
            for (int i = 0; i < 20; i++) { final int k = i; fs.add(tpe.submit(() -> k)); }
            int sum = 0;
            for (Future<Integer> f : fs) sum += f.get();
            eq(sum, 190, "tpe-sum");
            tpe.shutdown();
            check(tpe.awaitTermination(5, TimeUnit.SECONDS), "tpe-terminated");
            eq(tpe.getCompletedTaskCount(), 20, "tpe-completed");
            eq(tpe.getTaskCount(), 20, "tpe-taskcount");

            // Rejection with AbortPolicy
            ThreadPoolExecutor bounded = new ThreadPoolExecutor(1, 1, 0, TimeUnit.MILLISECONDS,
                new ArrayBlockingQueue<>(1), new ThreadPoolExecutor.AbortPolicy());
            final CountDownLatch started = new CountDownLatch(1);
            final CountDownLatch release = new CountDownLatch(1);
            bounded.execute(() -> { started.countDown(); try { release.await(); } catch (InterruptedException e) {} });
            check(started.await(2, TimeUnit.SECONDS), "bounded-started");
            bounded.execute(() -> {});       // fills queue (cap 1)
            boolean rejected = false;
            try { bounded.execute(() -> {}); } // queue full, max threads busy
            catch (RejectedExecutionException re) { rejected = true; }
            check(rejected, "tpe-rejected");
            release.countDown();
            bounded.shutdown();
            check(bounded.awaitTermination(5, TimeUnit.SECONDS), "bounded-terminated");

            // CallerRunsPolicy runs on caller
            ThreadPoolExecutor crp = new ThreadPoolExecutor(1, 1, 0, TimeUnit.MILLISECONDS,
                new ArrayBlockingQueue<>(1), new ThreadPoolExecutor.CallerRunsPolicy());
            final CountDownLatch s2 = new CountDownLatch(1);
            final CountDownLatch r2 = new CountDownLatch(1);
            final AtomicReference<String> ranOn = new AtomicReference<>();
            crp.execute(() -> { s2.countDown(); try { r2.await(); } catch (InterruptedException e) {} });
            check(s2.await(2, TimeUnit.SECONDS), "crp-started");
            crp.execute(() -> {});  // queued
            final String main = Thread.currentThread().getName();
            crp.execute(() -> ranOn.set(Thread.currentThread().getName())); // runs on caller
            check(main.equals(ranOn.get()), "crp-caller-runs");
            r2.countDown();
            crp.shutdown();
        });

        // ================= CompletableFuture =================
        sec("CompletableFuture", () -> {
            eq(CompletableFuture.completedFuture(5).get(), 5, "cf-completed");
            eq(CompletableFuture.completedFuture(5).getNow(-1), 5, "cf-getNow-done");
            eq(new CompletableFuture<Integer>().getNow(-1), -1, "cf-getNow-pending");

            int r = CompletableFuture.supplyAsync(() -> 10, pool)
                .thenApply(x -> x + 5)
                .thenCompose(x -> CompletableFuture.supplyAsync(() -> x * 2, pool))
                .get();
            eq(r, 30, "cf-chain");

            // thenAccept / thenRun
            final int[] acc = new int[1];
            CompletableFuture.supplyAsync(() -> 21, pool).thenAccept(x -> acc[0] = x).get();
            eq(acc[0], 21, "cf-thenAccept");
            final boolean[] ran = new boolean[1];
            CompletableFuture.runAsync(() -> {}, pool).thenRun(() -> ran[0] = true).get();
            check(ran[0], "cf-thenRun");

            // thenCombine
            CompletableFuture<Integer> a = CompletableFuture.supplyAsync(() -> 3, pool);
            CompletableFuture<Integer> b = CompletableFuture.supplyAsync(() -> 4, pool);
            eq(a.thenCombine(b, (x, y) -> x * y).get(), 12, "cf-thenCombine");

            // allOf / anyOf
            CompletableFuture<Integer> c1 = CompletableFuture.supplyAsync(() -> 1, pool);
            CompletableFuture<Integer> c2 = CompletableFuture.supplyAsync(() -> 2, pool);
            CompletableFuture<Integer> c3 = CompletableFuture.supplyAsync(() -> 3, pool);
            CompletableFuture.allOf(c1, c2, c3).get();
            eq(c1.join() + c2.join() + c3.join(), 6, "cf-allOf");
            Object any = CompletableFuture.anyOf(
                CompletableFuture.supplyAsync(() -> 7, pool),
                CompletableFuture.supplyAsync(() -> 7, pool)).get();
            eq((Integer) any, 7, "cf-anyOf");

            // exceptionally
            eq(CompletableFuture.<Integer>supplyAsync(() -> { throw new RuntimeException("x"); }, pool)
                .exceptionally(t -> 99).get(), 99, "cf-exceptionally");
            // handle
            eq(CompletableFuture.<Integer>supplyAsync(() -> { throw new RuntimeException("e"); }, pool)
                .handle((v, t) -> t != null ? -1 : v).get(), -1, "cf-handle");
            // whenComplete
            final int[] seen = new int[1];
            CompletableFuture.supplyAsync(() -> 55, pool).whenComplete((v, t) -> seen[0] = v).get();
            eq(seen[0], 55, "cf-whenComplete");

            // manual complete
            CompletableFuture<Integer> manual = new CompletableFuture<>();
            check(manual.complete(77), "cf-manual-complete");
            check(!manual.complete(0), "cf-double-complete");
            eq(manual.get(), 77, "cf-manual-get");
            check(manual.isDone(), "cf-isDone");

            // failedFuture
            CompletableFuture<Integer> bad = CompletableFuture.failedFuture(new IllegalStateException());
            check(bad.isCompletedExceptionally(), "cf-failed");

            // applyToEither (both 7 -> apply +1 = 8)
            CompletableFuture<Integer> e1 = CompletableFuture.supplyAsync(() -> 7, pool);
            CompletableFuture<Integer> e2 = CompletableFuture.supplyAsync(() -> 7, pool);
            eq(e1.applyToEither(e2, x -> x + 1).get(), 8, "cf-applyToEither");

            // runAfterBoth
            final boolean[] both = new boolean[1];
            CompletableFuture.supplyAsync(() -> 1, pool)
                .runAfterBoth(CompletableFuture.supplyAsync(() -> 2, pool), () -> both[0] = true).get();
            check(both[0], "cf-runAfterBoth");
        });

        // ================= ForkJoin =================
        sec("ForkJoin", () -> {
            ForkJoinPool fj = new ForkJoinPool();
            long fjsum = fj.submit(() -> IntStream.rangeClosed(1, 1000).parallel().asLongStream().sum()).get();
            eq(fjsum, 500500L, "fj-parallel-sum");
            eq(fj.invoke(new FibTask(15)), 610, "fj-recursivetask-fib");
            int[] arr = new int[100];
            fj.invoke(new IncAction(arr, 0, arr.length));
            int s = 0; for (int v : arr) s += v;
            eq(s, 100, "fj-recursiveaction");
            eq(ForkJoinPool.commonPool().invoke(new FibTask(10)), 55, "fj-commonpool");
            fj.shutdown();
        });

        sec("parallelStream", () -> {
            long psum = IntStream.rangeClosed(1, 10000).parallel().mapToLong(x -> x).sum();
            eq(psum, 50005000L, "parallelstream-sum");
            long count = Stream.iterate(1, x -> x + 1).limit(1000).parallel().filter(x -> x % 2 == 0).count();
            eq(count, 500, "parallelstream-filter-count");
            int reduced = Arrays.asList(1, 2, 3, 4, 5).parallelStream().reduce(0, Integer::sum);
            eq(reduced, 15, "parallelstream-reduce");
        });

        // ================= synchronizers =================
        sec("CountDownLatch", () -> {
            CountDownLatch latch = new CountDownLatch(3);
            eq(latch.getCount(), 3, "cdl-count3");
            final AtomicInteger started = new AtomicInteger();
            for (int i = 0; i < 3; i++) {
                new Thread(() -> { started.incrementAndGet(); latch.countDown(); }).start();
            }
            check(latch.await(5, TimeUnit.SECONDS), "cdl-await");
            eq(latch.getCount(), 0, "cdl-count0");
            eq(started.get(), 3, "cdl-all-started");
            // already-zero await returns immediately
            check(latch.await(1, TimeUnit.MILLISECONDS), "cdl-zero-await");
        });

        sec("CyclicBarrier", () -> {
            final int parties = 4;
            final AtomicInteger action = new AtomicInteger();
            final CyclicBarrier cb = new CyclicBarrier(parties, action::incrementAndGet);
            eq(cb.getParties(), parties, "cb-parties");
            check(!cb.isBroken(), "cb-not-broken");
            final AtomicInteger passed = new AtomicInteger();
            Thread[] ts = new Thread[parties];
            for (int i = 0; i < parties; i++) {
                ts[i] = new Thread(() -> {
                    try { cb.await(); passed.incrementAndGet(); }
                    catch (Exception e) {}
                });
            }
            for (Thread t : ts) t.start();
            for (Thread t : ts) t.join(5000);
            eq(passed.get(), parties, "cb-all-passed");
            eq(action.get(), 1, "cb-action-once");
            eq(cb.getNumberWaiting(), 0, "cb-no-waiting");
            cb.reset();
            check(!cb.isBroken(), "cb-reset-ok");
        });

        sec("Semaphore", () -> {
            Semaphore sem = new Semaphore(3);
            eq(sem.availablePermits(), 3, "sem-avail3");
            check(sem.tryAcquire(), "sem-acquire1");
            eq(sem.availablePermits(), 2, "sem-avail2");
            check(sem.tryAcquire(2), "sem-acquire2");
            check(!sem.tryAcquire(), "sem-empty");
            sem.release();
            eq(sem.availablePermits(), 1, "sem-release");
            sem.acquireUninterruptibly();
            eq(sem.availablePermits(), 0, "sem-uninterruptible");
            sem.release(2);
            eq(sem.drainPermits(), 2, "sem-drain");
            eq(sem.availablePermits(), 0, "sem-drained");
            check(new Semaphore(0, true).isFair(), "sem-fair");
        });

        sec("Phaser", () -> {
            Phaser ph = new Phaser(1);
            eq(ph.getPhase(), 0, "ph-phase0");
            eq(ph.getRegisteredParties(), 1, "ph-registered1");
            ph.register();
            eq(ph.getRegisteredParties(), 2, "ph-registered2");
            ph.arriveAndDeregister();
            eq(ph.getRegisteredParties(), 1, "ph-deregistered");
            ph.arriveAndDeregister();
            check(ph.isTerminated(), "ph-terminated");

            final Phaser ph2 = new Phaser(3);
            final int rounds = 2;
            Thread[] ts = new Thread[3];
            for (int i = 0; i < 3; i++) {
                ts[i] = new Thread(() -> { for (int k = 0; k < rounds; k++) ph2.arriveAndAwaitAdvance(); });
            }
            for (Thread t : ts) t.start();
            for (Thread t : ts) t.join(5000);
            eq(ph2.getPhase(), rounds, "ph2-phase-advanced");
            eq(ph2.getRegisteredParties(), 3, "ph2-registered");
            ph2.forceTermination();
            check(ph2.isTerminated(), "ph2-forced-term");
        });

        sec("Exchanger", () -> {
            final Exchanger<String> ex = new Exchanger<>();
            final String[] got = new String[2];
            Thread t = new Thread(() -> { try { got[0] = ex.exchange("A"); } catch (InterruptedException e) {} });
            t.start();
            got[1] = ex.exchange("B");
            t.join(5000);
            check("B".equals(got[0]), "exchanger-a-gets-b");
            check("A".equals(got[1]), "exchanger-b-gets-a");
        });

        // ================= concurrent collections =================
        sec("ConcurrentHashMap", () -> {
            ConcurrentHashMap<String, Integer> chm = new ConcurrentHashMap<>();
            chm.put("a", 1);
            check(chm.putIfAbsent("a", 99) == 1, "chm-putIfAbsent-existing");
            check(chm.putIfAbsent("b", 2) == null, "chm-putIfAbsent-new");
            eq(chm.get("a"), 1, "chm-get");
            eq(chm.getOrDefault("zzz", -1), -1, "chm-getOrDefault");
            chm.merge("a", 10, Integer::sum);
            eq(chm.get("a"), 11, "chm-merge");
            chm.compute("a", (k, v) -> v + 1);
            eq(chm.get("a"), 12, "chm-compute");
            chm.computeIfAbsent("c", k -> 3);
            eq(chm.get("c"), 3, "chm-computeIfAbsent");
            chm.computeIfPresent("c", (k, v) -> v * 10);
            eq(chm.get("c"), 30, "chm-computeIfPresent");
            check(!chm.remove("a", 999), "chm-remove-badval");
            check(chm.remove("a", 12), "chm-remove-okval");
            check(chm.replace("b", 2, 20), "chm-replace");
            eq(chm.get("b"), 20, "chm-after-replace");
            check(chm.containsKey("b"), "chm-containsKey");
            check(chm.contains(20), "chm-containsValue");
            eq(chm.mappingCount(), chm.size(), "chm-mappingCount");

            // bulk ops (sequential threshold)
            ConcurrentHashMap<Integer, Integer> m = new ConcurrentHashMap<>();
            for (int i = 1; i <= 10; i++) m.put(i, i);
            int sum = m.reduceValuesToInt(Long.MAX_VALUE, v -> v, 0, Integer::sum);
            eq(sum, 55, "chm-reduceValuesToInt");
            Integer found = m.search(Long.MAX_VALUE, (k, v) -> v == 7 ? v : null);
            eq(found, 7, "chm-search");
            final AtomicInteger fe = new AtomicInteger();
            m.forEach(Long.MAX_VALUE, (k, v) -> fe.addAndGet(v));
            eq(fe.get(), 55, "chm-forEach");

            // newKeySet
            Set<String> ks = ConcurrentHashMap.newKeySet();
            ks.add("x"); ks.add("x"); ks.add("y");
            eq(ks.size(), 2, "chm-newKeySet");

            // concurrent merge
            final ConcurrentHashMap<String, Integer> cc = new ConcurrentHashMap<>();
            Thread[] ts = new Thread[4];
            for (int i = 0; i < 4; i++) ts[i] = new Thread(() -> { for (int j = 0; j < 500; j++) cc.merge("k", 1, Integer::sum); });
            for (Thread t : ts) t.start();
            for (Thread t : ts) t.join();
            eq(cc.get("k"), 2000, "chm-concurrent-merge");
        });

        sec("ConcurrentSkipListMap", () -> {
            ConcurrentSkipListMap<Integer, String> m = new ConcurrentSkipListMap<>();
            for (int k : new int[]{5, 1, 9, 3, 7}) m.put(k, "v" + k);
            eq(m.firstKey(), 1, "cslm-first");
            eq(m.lastKey(), 9, "cslm-last");
            eq(m.floorKey(4), 3, "cslm-floor");
            eq(m.ceilingKey(4), 5, "cslm-ceiling");
            eq(m.higherKey(5), 7, "cslm-higher");
            eq(m.lowerKey(5), 3, "cslm-lower");
            eq(m.headMap(5).size(), 2, "cslm-headMap");
            eq(m.tailMap(5).size(), 3, "cslm-tailMap");
            eq(m.subMap(3, 8).size(), 3, "cslm-subMap");
            eq(m.descendingMap().firstKey(), 9, "cslm-descending");
            eq(m.pollFirstEntry().getKey(), 1, "cslm-pollFirst");
            eq(m.pollLastEntry().getKey(), 9, "cslm-pollLast");
            eq(m.size(), 3, "cslm-size-after-poll");

            ConcurrentSkipListSet<Integer> s = new ConcurrentSkipListSet<>(Arrays.asList(3, 1, 2));
            eq(s.first(), 1, "cslset-first");
            eq(s.last(), 3, "cslset-last");
            eq(s.ceiling(2), 2, "cslset-ceiling");
            eq(s.higher(2), 3, "cslset-higher");
        });

        sec("CopyOnWrite", () -> {
            CopyOnWriteArrayList<Integer> cow = new CopyOnWriteArrayList<>();
            cow.add(1); cow.add(2); cow.add(3);
            eq(cow.size(), 3, "cow-size");
            eq(cow.get(1), 2, "cow-get");
            Iterator<Integer> it = cow.iterator();   // snapshot at this moment
            cow.add(4);
            int snap = 0; while (it.hasNext()) { it.next(); snap++; }
            eq(snap, 3, "cow-snapshot-iterator");
            eq(cow.size(), 4, "cow-after-add");
            check(!cow.addIfAbsent(2), "cow-addIfAbsent-existing");
            check(cow.addIfAbsent(5), "cow-addIfAbsent-new");
            check(cow.contains(5), "cow-contains");
            eq(cow.indexOf(3), 2, "cow-indexOf");

            CopyOnWriteArraySet<String> cset = new CopyOnWriteArraySet<>();
            check(cset.add("a"), "cowset-add");
            check(!cset.add("a"), "cowset-dup");
            eq(cset.size(), 1, "cowset-size");
        });

        sec("ConcurrentLinkedQueueDeque", () -> {
            ConcurrentLinkedQueue<Integer> q = new ConcurrentLinkedQueue<>();
            check(q.isEmpty(), "clq-empty");
            check(q.offer(1) && q.offer(2) && q.offer(3), "clq-offer");
            eq(q.peek(), 1, "clq-peek");
            eq(q.poll(), 1, "clq-poll");
            eq(q.size(), 2, "clq-size");

            ConcurrentLinkedDeque<Integer> d = new ConcurrentLinkedDeque<>();
            d.addFirst(1); d.addLast(2); d.addFirst(0);
            eq(d.peekFirst(), 0, "cld-peekFirst");
            eq(d.peekLast(), 2, "cld-peekLast");
            eq(d.pollFirst(), 0, "cld-pollFirst");
            eq(d.pollLast(), 2, "cld-pollLast");
            eq(d.size(), 1, "cld-size");
        });

        sec("BlockingQueues", () -> {
            // ArrayBlockingQueue
            ArrayBlockingQueue<Integer> abq = new ArrayBlockingQueue<>(3);
            check(abq.offer(1) && abq.offer(2) && abq.offer(3), "abq-offer");
            check(!abq.offer(4), "abq-full");
            eq(abq.remainingCapacity(), 0, "abq-remaining0");
            eq(abq.poll(), 1, "abq-poll");
            check(abq.offer(5, 100, TimeUnit.MILLISECONDS), "abq-offer-timed");
            List<Integer> drain = new ArrayList<>();
            int n = abq.drainTo(drain);
            eq(n, 3, "abq-drain-count");
            eq(drain.size(), 3, "abq-drain-list");

            // LinkedBlockingQueue
            LinkedBlockingQueue<Integer> lbq = new LinkedBlockingQueue<>();
            lbq.put(10); lbq.put(20);
            eq(lbq.take(), 10, "lbq-take");
            eq(lbq.poll(50, TimeUnit.MILLISECONDS), 20, "lbq-poll-timed");
            check(lbq.poll(10, TimeUnit.MILLISECONDS) == null, "lbq-poll-empty");

            // PriorityBlockingQueue ordering
            PriorityBlockingQueue<Integer> pbq = new PriorityBlockingQueue<>();
            pbq.put(3); pbq.put(1); pbq.put(2);
            eq(pbq.poll(), 1, "pbq-order1");
            eq(pbq.poll(), 2, "pbq-order2");
            eq(pbq.poll(), 3, "pbq-order3");

            // LinkedBlockingDeque
            LinkedBlockingDeque<Integer> lbd = new LinkedBlockingDeque<>();
            lbd.putFirst(1); lbd.putLast(2); lbd.putFirst(0);
            eq(lbd.takeFirst(), 0, "lbd-takeFirst");
            eq(lbd.takeLast(), 2, "lbd-takeLast");

            // DelayQueue (smallest delay leaves first)
            DelayQueue<DItem> dq = new DelayQueue<>();
            dq.put(new DItem(3, 60));
            dq.put(new DItem(1, 20));
            dq.put(new DItem(2, 40));
            try {
                eq(dq.take().id, 1, "dq-order1");
                eq(dq.take().id, 2, "dq-order2");
                eq(dq.take().id, 3, "dq-order3");
            } catch (InterruptedException e) { check(false, "dq-interrupted"); }
        });

        sec("SynchronousQueue", () -> {
            final SynchronousQueue<Integer> sq = new SynchronousQueue<>();
            check(sq.poll() == null, "sq-empty-poll");
            final int[] got = new int[1];
            Thread t = new Thread(() -> { try { got[0] = sq.take(); } catch (InterruptedException e) {} });
            t.start();
            sq.put(9);
            t.join(5000);
            eq(got[0], 9, "sq-rendezvous");
        });

        sec("TransferQueue", () -> {
            final LinkedTransferQueue<Integer> ltq = new LinkedTransferQueue<>();
            check(!ltq.tryTransfer(1), "ltq-noconsumer");  // no waiting consumer -> false
            final int[] got = new int[1];
            Thread c = new Thread(() -> { try { got[0] = ltq.take(); } catch (InterruptedException e) {} });
            c.start();
            // wait until consumer is waiting
            long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(3);
            while (!ltq.hasWaitingConsumer() && System.nanoTime() < deadline) sleepQuiet(5);
            check(ltq.hasWaitingConsumer(), "ltq-hasWaitingConsumer");
            ltq.transfer(42);
            c.join(5000);
            eq(got[0], 42, "ltq-transfer");
        });

        // ================= TimeUnit / ThreadLocalRandom =================
        sec("TimeUnit", () -> {
            eq(TimeUnit.SECONDS.toMillis(2), 2000L, "tu-toMillis");
            eq(TimeUnit.MILLISECONDS.toSeconds(2500), 2L, "tu-toSeconds");
            eq(TimeUnit.MINUTES.toSeconds(3), 180L, "tu-minToSec");
            eq(TimeUnit.HOURS.toMinutes(2), 120L, "tu-hrToMin");
            eq(TimeUnit.DAYS.toHours(1), 24L, "tu-dayToHr");
            eq(TimeUnit.SECONDS.convert(1500, TimeUnit.MILLISECONDS), 1L, "tu-convert");
            eq(TimeUnit.NANOSECONDS.toMicros(1500), 1L, "tu-nanoToMicros");
            eq(TimeUnit.SECONDS.toNanos(1), 1_000_000_000L, "tu-toNanos");
        });

        sec("ThreadLocalRandom", () -> {
            ThreadLocalRandom rnd = ThreadLocalRandom.current();
            for (int i = 0; i < 50; i++) {
                int x = rnd.nextInt(10, 20);
                if (x < 10 || x >= 20) { check(false, "tlr-int-range"); break; }
            }
            check(true, "tlr-int-range");
            long y = rnd.nextLong(100);
            check(y >= 0 && y < 100, "tlr-long-range");
            double d = rnd.nextDouble();
            check(d >= 0.0 && d < 1.0, "tlr-double-unit");
            double dd = rnd.nextDouble(5.0, 10.0);
            check(dd >= 5.0 && dd < 10.0, "tlr-double-range");
            rnd.nextBoolean(); // smoke a boolean draw
            check(true, "tlr-boolean");
        });

        sec("ExecutorCompletionService", () -> {
            ExecutorCompletionService<Integer> ecs = new ExecutorCompletionService<>(pool);
            for (int i = 1; i <= 5; i++) { final int k = i; ecs.submit(() -> k * k); }
            int total = 0;
            for (int i = 0; i < 5; i++) total += ecs.take().get();
            eq(total, 55, "ecs-total");
            check(ecs.poll() == null, "ecs-poll-empty");
        });

        // ================= java.lang threading =================
        sec("Thread-basics", () -> {
            final AtomicBoolean done = new AtomicBoolean(false);
            Thread t = new Thread(() -> done.set(true), "worker-1");
            check(t.getName().equals("worker-1"), "th-name");
            t.setName("worker-2");
            check(t.getName().equals("worker-2"), "th-rename");
            t.setPriority(Thread.NORM_PRIORITY);
            eq(t.getPriority(), Thread.NORM_PRIORITY, "th-priority");
            t.setDaemon(true);
            check(t.isDaemon(), "th-daemon");
            check(t.getState() == Thread.State.NEW, "th-state-new");
            check(t.getId() > 0, "th-id");
            t.start();
            t.join(5000);
            check(t.getState() == Thread.State.TERMINATED, "th-state-terminated");
            check(!t.isAlive(), "th-not-alive");
            check(done.get(), "th-ran");
        });

        sec("Thread-interrupt", () -> {
            final AtomicBoolean caught = new AtomicBoolean(false);
            Thread t = new Thread(() -> {
                try { Thread.sleep(60000); }
                catch (InterruptedException e) { caught.set(true); }
            });
            t.start();
            // ensure it is sleeping
            long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(3);
            while (t.getState() != Thread.State.TIMED_WAITING && System.nanoTime() < deadline) sleepQuiet(5);
            t.interrupt();
            t.join(5000);
            check(caught.get(), "th-interrupt-caught");

            // interrupt status flag on current thread
            Thread.currentThread().interrupt();
            check(Thread.interrupted(), "th-interrupted-clears");
            check(!Thread.interrupted(), "th-interrupted-cleared");
        });

        sec("ThreadLocal", () -> {
            ThreadLocal<Integer> tl = ThreadLocal.withInitial(() -> 100);
            eq(tl.get(), 100, "tl-initial");
            tl.set(7);
            eq(tl.get(), 7, "tl-set");
            final int[] other = {-1};
            Thread t = new Thread(() -> other[0] = tl.get());
            t.start(); t.join(5000);
            eq(other[0], 100, "tl-other-thread-initial");
            eq(tl.get(), 7, "tl-still-set");
            tl.remove();
            eq(tl.get(), 100, "tl-after-remove");
        });

        sec("wait-notify", () -> {
            final Object lock = new Object();
            final int[] state = {0};  // 0=idle 1=go 2=done
            Thread w = new Thread(() -> {
                synchronized (lock) {
                    while (state[0] == 0) {
                        try { lock.wait(); } catch (InterruptedException e) { return; }
                    }
                    state[0] = 2;
                    lock.notifyAll();
                }
            });
            w.start();
            sleepQuiet(50);
            synchronized (lock) { state[0] = 1; lock.notifyAll(); }
            synchronized (lock) {
                long deadline = System.currentTimeMillis() + 5000;
                while (state[0] != 2 && System.currentTimeMillis() < deadline) {
                    try { lock.wait(200); } catch (InterruptedException e) { break; }
                }
            }
            w.join(5000);
            eq(state[0], 2, "wait-notify-done");
        });

        sec("synchronized-counter", () -> {
            final int[] counter = {0};
            final Object mon = new Object();
            Thread[] ts = new Thread[8];
            for (int i = 0; i < 8; i++) {
                ts[i] = new Thread(() -> {
                    for (int j = 0; j < 1000; j++) synchronized (mon) { counter[0]++; }
                });
            }
            for (Thread t : ts) t.start();
            for (Thread t : ts) t.join();
            eq(counter[0], 8000, "synchronized-counter");
        });

        sec("AtomicInteger-contended", () -> {
            final AtomicInteger ai = new AtomicInteger();
            final CountDownLatch latch = new CountDownLatch(8);
            for (int i = 0; i < 8; i++) {
                pool.submit(() -> { for (int j = 0; j < 1000; j++) ai.incrementAndGet(); latch.countDown(); });
            }
            check(latch.await(10, TimeUnit.SECONDS), "atomic-latch-await");
            eq(ai.get(), 8000, "atomic-contended");
        });

        sec("ReentrantLock-contended", () -> {
            final ReentrantLock lock = new ReentrantLock();
            final int[] counter = {0};
            final CountDownLatch latch = new CountDownLatch(8);
            for (int i = 0; i < 8; i++) {
                pool.submit(() -> {
                    for (int j = 0; j < 1000; j++) { lock.lock(); try { counter[0]++; } finally { lock.unlock(); } }
                    latch.countDown();
                });
            }
            check(latch.await(10, TimeUnit.SECONDS), "rlock-latch-await");
            eq(counter[0], 8000, "rlock-contended");
        });

        sec("BlockingQueue-prodcons", () -> {
            final BlockingQueue<Integer> bq = new ArrayBlockingQueue<>(16);
            final AtomicInteger consumed = new AtomicInteger();
            final long[] sum = new long[1];
            Thread prod = new Thread(() -> { try { for (int i = 1; i <= 200; i++) bq.put(i); } catch (InterruptedException e) {} });
            Thread cons = new Thread(() -> {
                try { for (int i = 0; i < 200; i++) { sum[0] += bq.take(); consumed.incrementAndGet(); } }
                catch (InterruptedException e) {}
            });
            cons.start(); prod.start();
            prod.join(10000); cons.join(10000);
            eq(consumed.get(), 200, "bq-consumed");
            eq(sum[0], 200L * 201 / 2, "bq-sum");
        });

        pool.shutdown();
        check(pool.awaitTermination(10, TimeUnit.SECONDS), "pool-terminated");

        System.out.println("CONC_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) System.out.println("CONC_DONE");
    }
}
