#define _GNU_SOURCE

#include "wakeup-latency-bench.h"

#include <errno.h>
#include <limits.h>
#include <linux/futex.h>
#include <linux/perf_event.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

enum futex_wait_outcome {
    FUTEX_WAIT_WOKEN,
    FUTEX_WAIT_NOT_PARKED,
    FUTEX_WAIT_FAILED,
};

struct handoff_state {
    _Atomic uint32_t ready;
    _Atomic uint32_t error;
    _Atomic uint32_t aborted;
    _Atomic uint32_t armed;
    _Atomic uint32_t gate;
    _Atomic uint32_t done;
    _Atomic uint64_t wake_timestamp_ns;
    _Atomic uint64_t pmu_start;
    _Atomic size_t valid_samples;
    _Atomic size_t not_parked_samples;
    size_t warmup_samples;
    size_t measured_samples;
    int receiver_cpu;
    int fifo_priority;
    enum bench_policy policy;
    int private_futex;
    int pmu_fd;
    uint64_t samples[];
};

struct pmu_snapshot {
    uint64_t value;
    uint64_t enabled_ns;
    uint64_t running_ns;
};

static const char *pmu_event_name;

static int read_pmu(int fd, struct pmu_snapshot *snapshot)
{
    if (read(fd, snapshot, sizeof(*snapshot)) != (ssize_t)sizeof(*snapshot) ||
        snapshot->enabled_ns == 0 ||
        snapshot->enabled_ns != snapshot->running_ns) {
        errno = EIO;
        return -1;
    }
    return 0;
}

static int open_pmu(int cpu)
{
    const char *name = getenv("WAKEUP_PMU_EVENT");
    uint64_t event;
    if (name == NULL || strcmp(name, "instructions") == 0) {
        name = "instructions";
        event = 0x08;
    } else if (strcmp(name, "cycles") == 0) {
        event = 0x11;
    } else if (strcmp(name, "l1i_refill") == 0) {
        event = 0x01;
    } else if (strcmp(name, "l1d_refill") == 0) {
        event = 0x03;
    } else {
        errno = EINVAL;
        return -1;
    }
    pmu_event_name = name;
    struct perf_event_attr attr = {0};
    attr.size = sizeof(attr);
    attr.type = PERF_TYPE_RAW;
    attr.config = event;
    attr.disabled = 1;
    attr.sample_period = UINT64_C(0xffff0000);
    attr.sample_type = PERF_SAMPLE_IP;
    attr.exclude_user = 1;
    attr.read_format = PERF_FORMAT_TOTAL_TIME_ENABLED |
                       PERF_FORMAT_TOTAL_TIME_RUNNING;
    int fd = (int)syscall(SYS_perf_event_open, &attr, -1, cpu, -1,
                          PERF_FLAG_FD_CLOEXEC);
    if (fd < 0) {
        return -1;
    }
    if (ioctl(fd, PERF_EVENT_IOC_RESET, 0) != 0 ||
        ioctl(fd, PERF_EVENT_IOC_ENABLE, 0) != 0) {
        int saved_errno = errno;
        close(fd);
        errno = saved_errno;
        return -1;
    }
    return fd;
}

static int compare_u64(const void *left, const void *right)
{
    uint64_t a = *(const uint64_t *)left;
    uint64_t b = *(const uint64_t *)right;
    return (a > b) - (a < b);
}

static void report_pmu(struct handoff_state *state, size_t count)
{
    uint64_t *raw = state->samples + state->measured_samples;
    uint64_t *read_cost = raw + state->measured_samples;
    qsort(raw, count, sizeof(*raw), compare_u64);
    qsort(read_cost, count, sizeof(*read_cost), compare_u64);
    uint64_t raw_p50 = raw[(count - 1) / 2];
    uint64_t read_p50 = read_cost[(count - 1) / 2];
    printf("WAKEUP_PMU_WINDOW {\"event\":\"%s\",\"samples\":%zu,"
           "\"raw_p50\":%llu,\"read_p50\":%llu,\"adjusted_p50\":%llu,"
           "\"raw_p99\":%llu,\"read_p99\":%llu}\n",
           pmu_event_name, count, (unsigned long long)raw_p50,
           (unsigned long long)read_p50,
           (unsigned long long)(raw_p50 > read_p50 ? raw_p50 - read_p50 : 0),
           (unsigned long long)raw[(count - 1) * 99 / 100],
           (unsigned long long)read_cost[(count - 1) * 99 / 100]);
}

static int futex_operation(int private_futex, int operation)
{
    return operation | (private_futex ? FUTEX_PRIVATE_FLAG : 0);
}

static enum futex_wait_outcome futex_wait_once(_Atomic uint32_t *word,
                                                uint32_t expected,
                                                int private_futex)
{
    for (;;) {
        long result = syscall(SYS_futex, word,
                              futex_operation(private_futex, FUTEX_WAIT),
                              expected, NULL, NULL, 0);
        if (result == 0) {
            return FUTEX_WAIT_WOKEN;
        }
        if (errno == EAGAIN) {
            return FUTEX_WAIT_NOT_PARKED;
        }
        if (errno != EINTR) {
            return FUTEX_WAIT_FAILED;
        }
    }
}

static int futex_wake_one(_Atomic uint32_t *word, int private_futex)
{
    return syscall(SYS_futex, word,
                   futex_operation(private_futex, FUTEX_WAKE), 1, NULL, NULL,
                   0) < 0
               ? -1
               : 0;
}

static void futex_wake_all(_Atomic uint32_t *word, int private_futex)
{
    (void)syscall(SYS_futex, word,
                  futex_operation(private_futex, FUTEX_WAKE), INT_MAX, NULL,
                  NULL, 0);
}

static int abort_error(const struct handoff_state *state)
{
    if (atomic_load_explicit(&state->aborted, memory_order_acquire) == 0) {
        return 0;
    }
    uint32_t error =
        atomic_load_explicit(&state->error, memory_order_relaxed);
    errno = error == 0 ? ECANCELED : (int)error;
    return -1;
}

static void publish_abort(struct handoff_state *state, int error)
{
    uint32_t no_error = 0;
    (void)atomic_compare_exchange_strong_explicit(
        &state->error, &no_error, (uint32_t)error, memory_order_relaxed,
        memory_order_relaxed);
    atomic_store_explicit(&state->aborted, 1, memory_order_release);

    /* Every participant can be parked on one of these sequence words. */
    atomic_store_explicit(&state->ready, 1, memory_order_release);
    futex_wake_all(&state->ready, state->private_futex);
    futex_wake_all(&state->armed, state->private_futex);
    futex_wake_all(&state->gate, state->private_futex);
    futex_wake_all(&state->done, state->private_futex);
}

static int wait_for_sequence(struct handoff_state *state,
                             _Atomic uint32_t *word, uint32_t sequence,
                             int private_futex)
{
    for (;;) {
        if (abort_error(state) != 0) {
            return -1;
        }
        uint32_t observed = atomic_load_explicit(word, memory_order_acquire);
        if (observed == sequence) {
            return 0;
        }
        if (observed > sequence) {
            errno = EPROTO;
            return -1;
        }
        enum futex_wait_outcome outcome =
            futex_wait_once(word, observed, private_futex);
        if (outcome == FUTEX_WAIT_FAILED) {
            return -1;
        }
    }
}

static int wait_for_receiver_to_park(const struct handoff_state *state,
                                     const struct bench_config *config)
{
    if (state->receiver_cpu == config->sender_cpu) {
        /*
         * Both participants are pinned to one CPU. Yielding gives the armed
         * receiver a complete scheduling turn in which to enter FUTEX_WAIT;
         * a timer-based delay would profile an unrelated task deadline on
         * every handoff.
         */
        return sched_yield();
    }

    struct timespec duration = {
        .tv_sec = (time_t)(config->park_settle_ns / 1000000000ULL),
        .tv_nsec = (long)(config->park_settle_ns % 1000000000ULL),
    };
    while (nanosleep(&duration, &duration) != 0) {
        if (errno != EINTR) {
            return -1;
        }
    }
    return 0;
}

static void publish_receiver_error(struct handoff_state *state, int error)
{
    publish_abort(state, error);
}

static int run_receiver(struct handoff_state *state)
{
    int error = bench_pin_current_thread(state->receiver_cpu);
    if (error == 0) {
        error = bench_apply_policy(state->policy, state->fifo_priority);
    }
    if (error != 0) {
        publish_receiver_error(state, error);
        return 1;
    }

    atomic_store_explicit(&state->ready, 1, memory_order_release);
    if (futex_wake_one(&state->ready, state->private_futex) != 0) {
        publish_receiver_error(state, errno);
        return 2;
    }

    size_t valid_samples = 0;
    size_t not_parked_samples = 0;
    size_t total_samples = state->warmup_samples + state->measured_samples;
    for (uint32_t sequence = 1; sequence <= total_samples; sequence++) {
        atomic_store_explicit(&state->armed, sequence, memory_order_release);
        if (futex_wake_one(&state->armed, state->private_futex) != 0) {
            publish_receiver_error(state, errno);
            return 3;
        }

        enum futex_wait_outcome outcome;
        for (;;) {
            outcome = futex_wait_once(&state->gate, sequence - 1,
                                      state->private_futex);
            if (outcome == FUTEX_WAIT_FAILED) {
                publish_receiver_error(state, errno);
                return 4;
            }
            if (abort_error(state) != 0) {
                return 5;
            }
            uint32_t gate = atomic_load_explicit(&state->gate,
                                                 memory_order_acquire);
            if (gate == sequence) {
                break;
            }
            if (gate != sequence - 1) {
                publish_receiver_error(state, EPROTO);
                return 6;
            }
            /* FUTEX_WAIT may return spuriously. Only the attempt observing
             * this sequence decides whether the measured wake found us
             * parked; an earlier spurious wake must not turn EAGAIN into a
             * valid latency sample. */
        }

        uint64_t resumed_ns = bench_monotonic_ns();
        uint64_t wake_ns = atomic_load_explicit(&state->wake_timestamp_ns,
                                                memory_order_acquire);
        uint64_t pmu_raw = 0;
        uint64_t pmu_read_cost = 0;
        if (state->pmu_fd >= 0) {
            struct pmu_snapshot end, after_read;
            if (read_pmu(state->pmu_fd, &end) != 0 ||
                read_pmu(state->pmu_fd, &after_read) != 0) {
                publish_receiver_error(state, errno);
                return 8;
            }
            uint64_t start = atomic_load_explicit(&state->pmu_start,
                                                  memory_order_relaxed);
            pmu_raw = end.value - start;
            pmu_read_cost = after_read.value - end.value;
        }
        if (sequence > state->warmup_samples) {
            if (outcome == FUTEX_WAIT_WOKEN) {
                if (state->pmu_fd >= 0) {
                    state->samples[state->measured_samples + valid_samples] =
                        pmu_raw;
                    state->samples[2 * state->measured_samples + valid_samples] =
                        pmu_read_cost;
                }
                state->samples[valid_samples++] = resumed_ns - wake_ns;
            } else {
                not_parked_samples++;
            }
        }

        atomic_store_explicit(&state->done, sequence, memory_order_release);
        if (futex_wake_one(&state->done, state->private_futex) != 0) {
            publish_receiver_error(state, errno);
            return 7;
        }
    }

    atomic_store_explicit(&state->valid_samples, valid_samples,
                          memory_order_release);
    atomic_store_explicit(&state->not_parked_samples, not_parked_samples,
                          memory_order_release);
    return 0;
}

static void *thread_receiver(void *argument)
{
    return (void *)(uintptr_t)run_receiver(argument);
}

static struct handoff_state *allocate_handoff_state(
    const struct bench_config *config, enum bench_policy policy,
    int receiver_cpu, int private_futex, size_t *mapping_size)
{
    if (config->handoff_samples >= UINT32_MAX ||
        config->warmup_samples >=
            UINT32_MAX - config->handoff_samples) {
        errno = EOVERFLOW;
        return NULL;
    }
    if (config->handoff_samples >
        (SIZE_MAX - sizeof(struct handoff_state)) / (3 * sizeof(uint64_t))) {
        errno = EOVERFLOW;
        return NULL;
    }
    *mapping_size = sizeof(struct handoff_state) +
                    3 * config->handoff_samples * sizeof(uint64_t);
    struct handoff_state *state =
        mmap(NULL, *mapping_size, PROT_READ | PROT_WRITE,
             MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (state == MAP_FAILED) {
        return NULL;
    }
    *state = (struct handoff_state) {
        .warmup_samples = config->warmup_samples,
        .measured_samples = config->handoff_samples,
        .receiver_cpu = receiver_cpu,
        .fifo_priority = config->fifo_priority,
        .policy = policy,
        .private_futex = private_futex,
        .pmu_fd = -1,
    };
    bench_prefault_samples(state->samples, 3 * config->handoff_samples);
    return state;
}

static int abort_sender(struct handoff_state *state)
{
    int error = errno == 0 ? EIO : errno;
    publish_abort(state, error);
    errno = error;
    return -1;
}

static int run_sender(struct handoff_state *state,
                      const struct bench_config *config)
{
    if (wait_for_sequence(state, &state->ready, 1,
                          state->private_futex) != 0) {
        return abort_sender(state);
    }
    uint32_t receiver_error =
        atomic_load_explicit(&state->error, memory_order_acquire);
    if (receiver_error != 0) {
        errno = (int)receiver_error;
        return -1;
    }

    size_t total_samples = state->warmup_samples + state->measured_samples;
    for (uint32_t sequence = 1; sequence <= total_samples; sequence++) {
        if (wait_for_sequence(state, &state->armed, sequence,
                              state->private_futex) != 0) {
            return abort_sender(state);
        }
        if (wait_for_receiver_to_park(state, config) != 0) {
            return abort_sender(state);
        }
        if (state->pmu_fd >= 0) {
            struct pmu_snapshot start;
            if (read_pmu(state->pmu_fd, &start) != 0) {
                return abort_sender(state);
            }
            atomic_store_explicit(&state->pmu_start, start.value,
                                  memory_order_relaxed);
        }
        atomic_store_explicit(&state->wake_timestamp_ns,
                              bench_monotonic_ns(), memory_order_relaxed);
        atomic_store_explicit(&state->gate, sequence, memory_order_release);
        if (futex_wake_one(&state->gate, state->private_futex) != 0 ||
            wait_for_sequence(state, &state->done, sequence,
                              state->private_futex) != 0) {
            return abort_sender(state);
        }
    }
    return 0;
}

static void fill_handoff_result(struct handoff_state *state,
                                const char *case_name,
                                enum bench_policy policy,
                                size_t mapping_size,
                                struct latency_result *result)
{
    *result = (struct latency_result) {
        .case_name = case_name,
        .policy = policy,
        .samples = state->samples,
        .sample_count = atomic_load_explicit(&state->valid_samples,
                                             memory_order_acquire),
        .attempted_samples = state->measured_samples,
        .not_parked_samples = atomic_load_explicit(
            &state->not_parked_samples, memory_order_acquire),
        .storage = state,
        .storage_size = mapping_size,
        .mapped_storage = 1,
    };
}

int bench_thread_handoff(const struct bench_config *config,
                         enum bench_policy policy, int same_cpu,
                         struct latency_result *result)
{
    size_t mapping_size;
    int receiver_cpu = same_cpu ? config->sender_cpu : config->receiver_cpu;
    struct handoff_state *state = allocate_handoff_state(
        config, policy, receiver_cpu, 1, &mapping_size);
    if (state == NULL) {
        return -1;
    }

    if (same_cpu) {
        state->pmu_fd = open_pmu(config->sender_cpu);
        if (state->pmu_fd < 0) {
            munmap(state, mapping_size);
            return -1;
        }
    }

    pthread_t receiver;
    int error = pthread_create(&receiver, NULL, thread_receiver, state);
    if (error != 0) {
        errno = error;
        if (state->pmu_fd >= 0) {
            close(state->pmu_fd);
        }
        munmap(state, mapping_size);
        return -1;
    }
    int sender_status = run_sender(state, config);
    void *receiver_result = NULL;
    error = pthread_join(receiver, &receiver_result);
    if (sender_status != 0 || error != 0 || receiver_result != NULL) {
        if (error != 0) {
            errno = error;
        } else if (receiver_result != NULL) {
            uint32_t receiver_error = atomic_load_explicit(
                &state->error, memory_order_acquire);
            errno = receiver_error == 0 ? EPROTO : (int)receiver_error;
        }
        if (state->pmu_fd >= 0) {
            close(state->pmu_fd);
        }
        munmap(state, mapping_size);
        return -1;
    }

    if (state->pmu_fd >= 0) {
        close(state->pmu_fd);
        size_t valid = atomic_load_explicit(&state->valid_samples,
                                            memory_order_acquire);
        if (valid > 0) {
            report_pmu(state, valid);
        }
    }

    fill_handoff_result(state,
                        same_cpu ? "thread_futex_same_cpu"
                                 : "thread_futex_cross_cpu",
                        policy, mapping_size, result);
    return 0;
}

int bench_process_handoff(const struct bench_config *config,
                          enum bench_policy policy,
                          struct latency_result *result)
{
    size_t mapping_size;
    struct handoff_state *state = allocate_handoff_state(
        config, policy, config->receiver_cpu, 0, &mapping_size);
    if (state == NULL) {
        return -1;
    }

    pid_t child = fork();
    if (child < 0) {
        munmap(state, mapping_size);
        return -1;
    }
    if (child == 0) {
        _exit(run_receiver(state));
    }

    int sender_status = run_sender(state, config);
    int wait_status = 0;
    pid_t waited;
    do {
        waited = waitpid(child, &wait_status, 0);
    } while (waited < 0 && errno == EINTR);
    if (sender_status != 0 || waited != child || !WIFEXITED(wait_status) ||
        WEXITSTATUS(wait_status) != 0) {
        if (sender_status == 0) {
            uint32_t receiver_error = atomic_load_explicit(
                &state->error, memory_order_acquire);
            errno = receiver_error == 0 ? EPROTO : (int)receiver_error;
        }
        munmap(state, mapping_size);
        return -1;
    }

    fill_handoff_result(state, "process_futex_cross_cpu", policy,
                        mapping_size, result);
    return 0;
}
