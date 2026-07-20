# cpu-concurrency

Pure-CPU single-core cooperative-concurrency correctness carpet for StarryOS. No GPU,
no networking - only the kernel's thread and synchronization primitives.

StarryOS runs on one vCPU (SMP is off by default), so these sub-tests do not measure
throughput. Each drives a parallel pattern from multiple threads and asserts that the
result equals a deterministic sequential reference: on a single core the threads
interleave cooperatively through the RR scheduler and the blocking primitives, and a
correct kernel must still produce the sequentially-correct answer. Every C-library
primitive used here (`pthread_mutex`/`cond`/`rwlock`, `sem_t`, C11 atomics) reduces on
musl to the `clone` / `FUTEX_*` / `sched_yield` syscalls, so a failing assertion points
at a kernel gap, not a userspace one.

## Sub-tests (14 assertions)

1. **Parallel reduction** - 8 threads sum+max a partitioned `[0, 10^6)`; assert the sum
   equals `N(N-1)/2` and the max equals `N-1`. (2)
2. **Producer/consumer** - a 16-slot bounded queue (mutex + two condvars), 4 producers
   x 4 consumers, 100000 items; assert every item is consumed exactly once, the
   produced/consumed key checksums match, and each producer's items stay FIFO. (3)
3. **Futex barrier** - a raw `SYS_futex` generation barrier, 6 threads x 50 rounds;
   assert no thread crosses into the next phase before the barrier opens. (1)
4. **Atomic contention** - 16 threads x 50000 `fetch_add` under both `relaxed` and
   `seq_cst`; assert the final counters equal `16 * 50000` (no lost updates). (2)
5. **Work pool** - 1000 tasks fanned out to 8 workers popping under a mutex; assert
   every task ran exactly once. (1)
6. **RW-lock invariant** - readers snapshot a versioned `(a, b)` pair under a read lock
   while a writer mutates it under the write lock keeping `a + b == K`; assert no reader
   ever observed a torn `a + b != K`. (1)
7. **Counting semaphore** - `sem_t` with 3 permits, 12 threads; assert the peak number
   of threads simultaneously inside the critical section never exceeds the permit count
   and the semaphore value is restored at the end. (2)
8. **Thread-local isolation** - `__thread` and `pthread_key` storage; assert each
   thread reads back only its own value across scheduler yields. (1)
9. **RR fairness** - N threads each advance their own counter and `sched_yield`; assert
   every thread made forward progress within a fixed wall budget (no starvation). (1)

The carpet prints `ALL PASS 14/14` and `CPU_CONCURRENCY_PASSED` only when the failure
count is zero and all 14 assertions hold.

## Build and run

`prebuild.sh` cross-compiles the single C11 source fully static against musl (pthread is
part of musl libc) for the target arch and installs it into the overlay. No prebuilt
binaries or fetched dependencies. Run on each architecture (single vCPU, `-smp 1`):

```
cargo xtask starry app qemu -t cpu-concurrency --arch x86_64
cargo xtask starry app qemu -t cpu-concurrency --arch aarch64
cargo xtask starry app qemu -t cpu-concurrency --arch riscv64
cargo xtask starry app qemu -t cpu-concurrency --arch loongarch64
```
