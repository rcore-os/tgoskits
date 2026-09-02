#define _GNU_SOURCE

#include <errno.h>
#include <inttypes.h>
#include <limits.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

enum metric_kind {
    METRIC_UNSET,
    METRIC_PERIODIC_JITTER,
    METRIC_DISPATCH_LATENCY,
    METRIC_EMULATED_IRQ_RESPONSE,
    METRIC_CPU_STRESS,
};

struct options {
    enum metric_kind metric;
    size_t iterations;
    size_t warmup_iterations;
    int64_t period_ns;
    int cpu;
    int fifo_priority;
};

struct sample {
    int64_t target_ns;
    int64_t observed_ns;
    int64_t latency_ns;
};

struct dispatch_context {
    int request_fd;
    int acknowledgement_fd;
    size_t iterations;
    size_t warmup_iterations;
    int cpu;
    int fifo_priority;
    struct sample *samples;
    atomic_int error_code;
    atomic_int_fast64_t signal_time_ns;
};

static void usage(FILE *stream, const char *program);
static int parse_options(int argc, char **argv, struct options *options);
static int configure_current_thread(int cpu, int fifo_priority);
static int run_periodic_jitter(const struct options *options, struct sample *samples);
static int run_dispatch_latency(const struct options *options, struct sample *samples);
static int run_emulated_irq_response(const struct options *options, struct sample *samples);
static int run_cpu_stress(const struct options *options);
static void emit_samples(
    const char *metric,
    const struct options *options,
    const struct sample *samples
);

int main(int argc, char **argv)
{
    struct options options = {
        .metric = METRIC_UNSET,
        .iterations = 10000,
        .warmup_iterations = 100,
        .period_ns = 1000000,
        .cpu = 0,
        .fifo_priority = 80,
    };

    int error = parse_options(argc, argv, &options);
    if (error != 0) {
        usage(stderr, argv[0]);
        return error;
    }
    if (options.iterations > SIZE_MAX / sizeof(struct sample)) {
        fprintf(stderr, "sample allocation size overflows size_t\n");
        return 2;
    }
    if (options.warmup_iterations > SIZE_MAX - options.iterations) {
        fprintf(stderr, "warmup plus measured iterations overflows size_t\n");
        return 2;
    }
    if (options.metric == METRIC_CPU_STRESS) {
        error = configure_current_thread(options.cpu, options.fifo_priority);
        if (error != 0) {
            fprintf(stderr, "failed to configure stress thread: %s\n", strerror(error));
            return 1;
        }
        error = run_cpu_stress(&options);
        if (error != 0) {
            fprintf(stderr, "CPU stress workload failed: %s\n", strerror(error));
            return 1;
        }
        return 0;
    }

    struct sample *samples = calloc(options.iterations, sizeof(*samples));
    if (samples == NULL) {
        perror("calloc samples");
        return 1;
    }

    error = configure_current_thread(options.cpu, options.fifo_priority);
    if (error != 0) {
        fprintf(stderr, "failed to configure benchmark thread: %s\n", strerror(error));
        free(samples);
        return 1;
    }

    const char *metric_name = NULL;
    switch (options.metric) {
    case METRIC_PERIODIC_JITTER:
        metric_name = "periodic_jitter";
        error = run_periodic_jitter(&options, samples);
        break;
    case METRIC_DISPATCH_LATENCY:
        metric_name = "dispatch_latency";
        error = run_dispatch_latency(&options, samples);
        break;
    case METRIC_EMULATED_IRQ_RESPONSE:
        metric_name = "emulated_irq_response";
        error = run_emulated_irq_response(&options, samples);
        break;
    case METRIC_CPU_STRESS:
        error = EINVAL;
        break;
    case METRIC_UNSET:
        error = EINVAL;
        break;
    }

    if (error != 0) {
        fprintf(stderr, "benchmark metric failed: %s\n", strerror(error));
        free(samples);
        return 1;
    }

    setvbuf(stdout, NULL, _IOLBF, 0);
    emit_samples(metric_name, &options, samples);
    free(samples);
    return 0;
}

static void usage(FILE *stream, const char *program)
{
    fprintf(
        stream,
        "usage: %s --metric periodic_jitter|dispatch_latency|emulated_irq_response|cpu_stress "
        "[--iterations N] [--warmup N] [--period-us N] [--cpu N] "
        "[--fifo-priority 0..98]\n",
        program
    );
}

static int parse_unsigned(const char *value, uint64_t maximum, uint64_t *parsed)
{
    if (value[0] == '-') {
        return EINVAL;
    }
    char *end = NULL;
    errno = 0;
    unsigned long long candidate = strtoull(value, &end, 10);
    if (errno != 0 || end == value || *end != '\0' || candidate > maximum) {
        return EINVAL;
    }
    *parsed = (uint64_t)candidate;
    return 0;
}

static int parse_metric(const char *value, enum metric_kind *metric)
{
    if (strcmp(value, "periodic_jitter") == 0) {
        *metric = METRIC_PERIODIC_JITTER;
    } else if (strcmp(value, "dispatch_latency") == 0) {
        *metric = METRIC_DISPATCH_LATENCY;
    } else if (strcmp(value, "emulated_irq_response") == 0) {
        *metric = METRIC_EMULATED_IRQ_RESPONSE;
    } else if (strcmp(value, "cpu_stress") == 0) {
        *metric = METRIC_CPU_STRESS;
    } else {
        return EINVAL;
    }
    return 0;
}

static int parse_options(int argc, char **argv, struct options *options)
{
    for (int index = 1; index < argc; index++) {
        if (strcmp(argv[index], "--help") == 0) {
            usage(stdout, argv[0]);
            exit(0);
        }
        if (index + 1 >= argc) {
            return 2;
        }

        const char *name = argv[index];
        const char *value = argv[++index];
        uint64_t parsed = 0;
        int error = 0;
        if (strcmp(name, "--metric") == 0) {
            error = parse_metric(value, &options->metric);
        } else if (strcmp(name, "--iterations") == 0) {
            error = parse_unsigned(value, SIZE_MAX, &parsed);
            if (error == 0) {
                options->iterations = (size_t)parsed;
            }
        } else if (strcmp(name, "--warmup") == 0) {
            error = parse_unsigned(value, SIZE_MAX, &parsed);
            if (error == 0) {
                options->warmup_iterations = (size_t)parsed;
            }
        } else if (strcmp(name, "--period-us") == 0) {
            error = parse_unsigned(value, INT64_MAX / 1000, &parsed);
            if (error == 0) {
                options->period_ns = (int64_t)(parsed * 1000);
            }
        } else if (strcmp(name, "--cpu") == 0) {
            error = parse_unsigned(value, CPU_SETSIZE - 1, &parsed);
            if (error == 0) {
                options->cpu = (int)parsed;
            }
        } else if (strcmp(name, "--fifo-priority") == 0) {
            error = parse_unsigned(value, 98, &parsed);
            if (error == 0) {
                options->fifo_priority = (int)parsed;
            }
        } else {
            return 2;
        }
        if (error != 0) {
            return 2;
        }
    }

    if (options->metric == METRIC_UNSET || options->iterations == 0 ||
        options->period_ns == 0) {
        return 2;
    }
    return 0;
}

static int configure_current_thread(int cpu, int fifo_priority)
{
    cpu_set_t affinity;
    CPU_ZERO(&affinity);
    CPU_SET(cpu, &affinity);
    if (sched_setaffinity(0, sizeof(affinity), &affinity) != 0) {
        return errno;
    }

    if (fifo_priority == 0) {
        return 0;
    }
    struct sched_param parameters = {.sched_priority = fifo_priority};
    if (sched_setscheduler(0, SCHED_FIFO, &parameters) != 0) {
        return errno;
    }
    return 0;
}

static volatile sig_atomic_t stress_stop_requested;

static void request_stress_stop(int signal_number)
{
    (void)signal_number;
    stress_stop_requested = 1;
}

static int run_cpu_stress(const struct options *options)
{
    struct sigaction action = {
        .sa_handler = request_stress_stop,
    };
    if (sigemptyset(&action.sa_mask) != 0 || sigaction(SIGTERM, &action, NULL) != 0 ||
        sigaction(SIGINT, &action, NULL) != 0) {
        return errno;
    }

    printf(
        "AXVISOR_RT_WORKLOAD_READY schema=1 kind=cpu-stress pid=%ld cpu=%d\n",
        (long)getpid(),
        options->cpu
    );
    fflush(stdout);

    volatile uint64_t state = UINT64_C(0x9e3779b97f4a7c15);
    while (!stress_stop_requested) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
    }
    (void)state;

    printf(
        "AXVISOR_RT_WORKLOAD_STOPPED schema=1 kind=cpu-stress pid=%ld cpu=%d\n",
        (long)getpid(),
        options->cpu
    );
    fflush(stdout);
    return 0;
}

static int monotonic_now_ns(int64_t *result)
{
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
        return errno;
    }
    *result = now.tv_sec * INT64_C(1000000000) + now.tv_nsec;
    return 0;
}

static struct timespec timespec_from_ns(int64_t nanoseconds)
{
    return (struct timespec){
        .tv_sec = nanoseconds / INT64_C(1000000000),
        .tv_nsec = nanoseconds % INT64_C(1000000000),
    };
}

static int checked_latency(
    int64_t target_ns,
    int64_t observed_ns,
    struct sample *sample
)
{
    if (observed_ns < target_ns) {
        return EDOM;
    }
    *sample = (struct sample){
        .target_ns = target_ns,
        .observed_ns = observed_ns,
        .latency_ns = observed_ns - target_ns,
    };
    return 0;
}

static int sleep_until(int64_t deadline_ns)
{
    struct timespec deadline = timespec_from_ns(deadline_ns);
    int error;
    do {
        error = clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &deadline, NULL);
    } while (error == EINTR);
    return error;
}

static int run_periodic_jitter(const struct options *options, struct sample *samples)
{
    int64_t deadline_ns;
    int error = monotonic_now_ns(&deadline_ns);
    if (error != 0) {
        return error;
    }

    size_t total = options->warmup_iterations + options->iterations;
    for (size_t index = 0; index < total; index++) {
        deadline_ns += options->period_ns;
        error = sleep_until(deadline_ns);
        if (error != 0) {
            return error;
        }
        int64_t observed_ns = 0;
        error = monotonic_now_ns(&observed_ns);
        if (error != 0) {
            return error;
        }
        if (index >= options->warmup_iterations) {
            error = checked_latency(
                deadline_ns,
                observed_ns,
                &samples[index - options->warmup_iterations]
            );
            if (error != 0) {
                return error;
            }
        }
    }
    return 0;
}

static int eventfd_read_one(int descriptor)
{
    eventfd_t value;
    while (eventfd_read(descriptor, &value) != 0) {
        if (errno != EINTR) {
            return errno;
        }
    }
    return value == 1 ? 0 : EPROTO;
}

static int eventfd_write_one(int descriptor)
{
    while (eventfd_write(descriptor, 1) != 0) {
        if (errno != EINTR) {
            return errno;
        }
    }
    return 0;
}

static void dispatch_fail(struct dispatch_context *context, int error)
{
    atomic_store_explicit(&context->error_code, error, memory_order_release);
    (void)eventfd_write_one(context->acknowledgement_fd);
}

static void *dispatch_worker(void *argument)
{
    struct dispatch_context *context = argument;
    int worker_priority = context->fifo_priority == 0 ? 0 : context->fifo_priority + 1;
    int error = configure_current_thread(context->cpu, worker_priority);
    if (error != 0) {
        dispatch_fail(context, error);
        return NULL;
    }
    error = eventfd_write_one(context->acknowledgement_fd);
    if (error != 0) {
        atomic_store_explicit(&context->error_code, error, memory_order_release);
        return NULL;
    }

    size_t total = context->warmup_iterations + context->iterations;
    for (size_t index = 0; index < total; index++) {
        error = eventfd_read_one(context->request_fd);
        if (error != 0) {
            dispatch_fail(context, error);
            return NULL;
        }
        int64_t target_ns = atomic_load_explicit(
            &context->signal_time_ns,
            memory_order_acquire
        );
        int64_t observed_ns = 0;
        error = monotonic_now_ns(&observed_ns);
        if (error == 0 && index >= context->warmup_iterations) {
            error = checked_latency(
                target_ns,
                observed_ns,
                &context->samples[index - context->warmup_iterations]
            );
        }
        if (error != 0) {
            dispatch_fail(context, error);
            return NULL;
        }
        error = eventfd_write_one(context->acknowledgement_fd);
        if (error != 0) {
            atomic_store_explicit(&context->error_code, error, memory_order_release);
            return NULL;
        }
    }
    return NULL;
}

static int run_dispatch_latency(const struct options *options, struct sample *samples)
{
    int request_fd = eventfd(0, EFD_CLOEXEC);
    if (request_fd < 0) {
        return errno;
    }
    int acknowledgement_fd = eventfd(0, EFD_CLOEXEC);
    if (acknowledgement_fd < 0) {
        int error = errno;
        close(request_fd);
        return error;
    }

    struct dispatch_context context = {
        .request_fd = request_fd,
        .acknowledgement_fd = acknowledgement_fd,
        .iterations = options->iterations,
        .warmup_iterations = options->warmup_iterations,
        .cpu = options->cpu,
        .fifo_priority = options->fifo_priority,
        .samples = samples,
    };
    atomic_init(&context.error_code, 0);
    atomic_init(&context.signal_time_ns, 0);

    pthread_t worker;
    int error = pthread_create(&worker, NULL, dispatch_worker, &context);
    if (error != 0) {
        close(acknowledgement_fd);
        close(request_fd);
        return error;
    }

    error = eventfd_read_one(acknowledgement_fd);
    if (error == 0) {
        error = atomic_load_explicit(&context.error_code, memory_order_acquire);
    }

    size_t total = options->warmup_iterations + options->iterations;
    for (size_t index = 0; error == 0 && index < total; index++) {
        int64_t signal_time_ns;
        error = monotonic_now_ns(&signal_time_ns);
        if (error != 0) {
            break;
        }
        atomic_store_explicit(
            &context.signal_time_ns,
            signal_time_ns,
            memory_order_release
        );
        error = eventfd_write_one(request_fd);
        if (error == 0) {
            error = eventfd_read_one(acknowledgement_fd);
        }
        if (error == 0) {
            error = atomic_load_explicit(&context.error_code, memory_order_acquire);
        }
    }

    if (error != 0) {
        (void)pthread_cancel(worker);
    }
    int join_error = pthread_join(worker, NULL);
    close(acknowledgement_fd);
    close(request_fd);
    if (error != 0) {
        return error;
    }
    return join_error;
}

static int run_emulated_irq_response(
    const struct options *options,
    struct sample *samples
)
{
    int timer_fd = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC);
    if (timer_fd < 0) {
        return errno;
    }

    int64_t deadline_ns;
    int error = monotonic_now_ns(&deadline_ns);
    size_t total = options->warmup_iterations + options->iterations;
    for (size_t index = 0; error == 0 && index < total; index++) {
        deadline_ns += options->period_ns;
        struct itimerspec timer = {
            .it_value = timespec_from_ns(deadline_ns),
        };
        if (timerfd_settime(timer_fd, TFD_TIMER_ABSTIME, &timer, NULL) != 0) {
            error = errno;
            break;
        }

        uint64_t expirations;
        ssize_t bytes;
        do {
            bytes = read(timer_fd, &expirations, sizeof(expirations));
        } while (bytes < 0 && errno == EINTR);
        if (bytes != (ssize_t)sizeof(expirations) || expirations != 1) {
            error = bytes < 0 ? errno : EPROTO;
            break;
        }

        int64_t observed_ns = 0;
        error = monotonic_now_ns(&observed_ns);
        if (error == 0 && index >= options->warmup_iterations) {
            error = checked_latency(
                deadline_ns,
                observed_ns,
                &samples[index - options->warmup_iterations]
            );
        }
    }

    close(timer_fd);
    return error;
}

static void emit_samples(
    const char *metric,
    const struct options *options,
    const struct sample *samples
)
{
    printf(
        "AXVISOR_RT_PROBE schema=1 metric=%s iterations=%zu warmup=%zu cpu=%d\n",
        metric,
        options->iterations,
        options->warmup_iterations,
        options->cpu
    );
    for (size_t index = 0; index < options->iterations; index++) {
        printf(
            "AXVISOR_RT_SAMPLE schema=1 metric=%s iteration=%zu cpu=%d "
            "target_ns=%" PRId64 " observed_ns=%" PRId64 " latency_ns=%" PRId64 "\n",
            metric,
            index,
            options->cpu,
            samples[index].target_ns,
            samples[index].observed_ns,
            samples[index].latency_ns
        );
    }
    printf(
        "AXVISOR_RT_METRIC_COMPLETE schema=1 metric=%s count=%zu\n",
        metric,
        options->iterations
    );
}
