/* Exercise the userspace handshake with a permitted spurious FUTEX_WAIT
 * completion. Only the futex boundary is injected; clock, affinity and policy
 * setup use the same implementation as the benchmark. */
#include "../handoff.c"

#include <stdarg.h>

static struct handoff_state *active_state;
static unsigned wait_calls;
static int final_not_parked;

long __real_syscall(long number, ...);

long __wrap_syscall(long number, ...)
{
    va_list arguments;
    va_start(arguments, number);
    if (number == SYS_clock_gettime) {
        int clock_id = va_arg(arguments, int);
        struct timespec *timestamp = va_arg(arguments, struct timespec *);
        va_end(arguments);
        return __real_syscall(number, clock_id, timestamp);
    }
    if (number != SYS_futex) {
        va_end(arguments);
        errno = ENOSYS;
        return -1;
    }
    _Atomic uint32_t *word = va_arg(arguments, _Atomic uint32_t *);
    int operation = va_arg(arguments, int) & FUTEX_CMD_MASK;
    va_end(arguments);
    if (operation == FUTEX_WAKE) {
        return 0;
    }
    if (operation != FUTEX_WAIT || word != &active_state->gate) {
        errno = EINVAL;
        return -1;
    }
    if (++wait_calls == 1) {
        return 0; /* The predicate has not changed. */
    }
    if (wait_calls != 2) {
        errno = EIO;
        return -1;
    }
    atomic_store(&active_state->wake_timestamp_ns, bench_monotonic_ns());
    atomic_store_explicit(&active_state->gate, 1, memory_order_release);
    if (final_not_parked) {
        errno = EAGAIN;
        return -1;
    }
    return 0;
}

int main(void)
{
    cpu_set_t allowed;
    if (sched_getaffinity(0, sizeof(allowed), &allowed) != 0) {
        return 1;
    }
    int cpu = 0;
    while (cpu < CPU_SETSIZE && !CPU_ISSET(cpu, &allowed)) {
        cpu++;
    }
    const struct bench_config config = {
        .handoff_samples = 1,
        .warmup_samples = 0,
        .sender_cpu = cpu,
    };
    for (final_not_parked = 0; final_not_parked <= 1; final_not_parked++) {
        size_t mapping_size;
        active_state = allocate_handoff_state(&config, BENCH_POLICY_OTHER,
                                               cpu, 1, &mapping_size);
        if (active_state == NULL) {
            return 1;
        }
        wait_calls = 0;
        int status = run_receiver(active_state);
        int valid = status == 0 && wait_calls == 2 &&
            atomic_load(&active_state->valid_samples) ==
                (size_t)!final_not_parked &&
            atomic_load(&active_state->not_parked_samples) ==
                (size_t)final_not_parked;
        munmap(active_state, mapping_size);
        if (!valid) {
            fprintf(stderr, "spurious handoff: status=%d waits=%u final_not_parked=%d\n",
                    status, wait_calls, final_not_parked);
            return 1;
        }
    }
    return 0;
}
