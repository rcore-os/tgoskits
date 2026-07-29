typedef unsigned long usize;
typedef unsigned long u64;
typedef long i64;

#ifndef QC_PERIOD_NS
#define QC_PERIOD_NS 1000000UL
#endif

#ifndef QC_SAMPLES
#define QC_SAMPLES 2000UL
#endif

enum {
    SYS_WRITE = 64,
    SYS_EXIT = 93,
    SYS_CLOCK_GETTIME = 113,
    SYS_CLOCK_NANOSLEEP = 115,
};

enum {
    CLOCK_MONOTONIC = 1,
    TIMER_ABSTIME = 1,
};

struct timespec {
    long tv_sec;
    long tv_nsec;
};

static u64 samples_ns[QC_SAMPLES];

static long syscall6(
    long number,
    long arg0,
    long arg1,
    long arg2,
    long arg3,
    long arg4,
    long arg5
) {
    register long x0 asm("x0") = arg0;
    register long x1 asm("x1") = arg1;
    register long x2 asm("x2") = arg2;
    register long x3 asm("x3") = arg3;
    register long x4 asm("x4") = arg4;
    register long x5 asm("x5") = arg5;
    register long x8 asm("x8") = number;

    asm volatile(
        "svc #0"
        : "+r"(x0)
        : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5), "r"(x8)
        : "memory"
    );
    return x0;
}

static usize string_length(const char *text) {
    usize length = 0;

    while (text[length] != '\0') {
        length++;
    }
    return length;
}

static void write_bytes(const char *data, usize length) {
    while (length > 0) {
        long written = syscall6(SYS_WRITE, 1, (long)data, (long)length, 0, 0, 0);

        if (written <= 0) {
            return;
        }
        data += written;
        length -= (usize)written;
    }
}

static void write_text(const char *text) {
    write_bytes(text, string_length(text));
}

static char *append_text(char *output, const char *text) {
    while (*text != '\0') {
        *output++ = *text++;
    }
    return output;
}

static char *append_u64(char *output, u64 value) {
    char reverse[24];
    usize count = 0;

    if (value == 0) {
        *output++ = '0';
        return output;
    }

    while (value > 0) {
        reverse[count++] = (char)('0' + (value % 10));
        value /= 10;
    }
    while (count > 0) {
        *output++ = reverse[--count];
    }
    return output;
}

static char *append_i64(char *output, i64 value) {
    if (value < 0) {
        *output++ = '-';
        value = -value;
    }
    return append_u64(output, (u64)value);
}

static void write_kv_u64(const char *key, u64 value) {
    char line[128];
    char *out = line;

    out = append_text(out, key);
    *out++ = '=';
    out = append_u64(out, value);
    *out++ = '\n';
    write_bytes(line, (usize)(out - line));
}

static void write_kv_i64(const char *key, i64 value) {
    char line[128];
    char *out = line;

    out = append_text(out, key);
    *out++ = '=';
    out = append_i64(out, value);
    *out++ = '\n';
    write_bytes(line, (usize)(out - line));
}

static u64 timespec_to_ns(const struct timespec *ts) {
    return ((u64)ts->tv_sec * 1000000000UL) + (u64)ts->tv_nsec;
}

static void ns_to_timespec(u64 ns, struct timespec *ts) {
    ts->tv_sec = (long)(ns / 1000000000UL);
    ts->tv_nsec = (long)(ns % 1000000000UL);
}

static u64 monotonic_ns(void) {
    struct timespec ts;
    long ret = syscall6(SYS_CLOCK_GETTIME, CLOCK_MONOTONIC, (long)&ts, 0, 0, 0, 0);

    if (ret < 0) {
        return 0;
    }
    return timespec_to_ns(&ts);
}

static long sleep_until_ns(u64 target_ns) {
    struct timespec ts;
    long ret;

    ns_to_timespec(target_ns, &ts);
    do {
        ret = syscall6(
            SYS_CLOCK_NANOSLEEP,
            CLOCK_MONOTONIC,
            TIMER_ABSTIME,
            (long)&ts,
            0,
            0,
            0
        );
    } while (ret == -4);

    return ret;
}

static void sort_samples(void) {
    for (usize i = 1; i < QC_SAMPLES; i++) {
        u64 value = samples_ns[i];
        usize j = i;

        while (j > 0 && samples_ns[j - 1] > value) {
            samples_ns[j] = samples_ns[j - 1];
            j--;
        }
        samples_ns[j] = value;
    }
}

static int should_report_sample(usize index) {
    usize sample_no = index + 1;

    return sample_no == 1 ||
           sample_no == QC_SAMPLES ||
           sample_no == (QC_SAMPLES / 4) ||
           sample_no == (QC_SAMPLES / 2) ||
           sample_no == ((QC_SAMPLES * 3) / 4);
}

static int run_probe(void) {
    u64 start_ns;
    u64 target_ns;
    u64 sum_ns = 0;
    u64 over_100us = 0;
    u64 over_500us = 0;
    u64 over_1000us = 0;
    long sleep_ret = 0;

    write_text("QC_RT_PERIODIC_START\n");
    write_kv_u64("QC_RT_PERIOD_SAMPLES", QC_SAMPLES);
    write_kv_u64("QC_RT_PERIOD_NS", QC_PERIOD_NS);

    start_ns = monotonic_ns();
    if (start_ns == 0) {
        write_text("QC_RT_PERIODIC_RESULT=FAIL_CLOCK\n");
        return 1;
    }

    target_ns = start_ns + 10000000UL;
    for (usize i = 0; i < QC_SAMPLES; i++) {
        u64 now_ns;
        u64 late_ns;

        target_ns += QC_PERIOD_NS;
        sleep_ret = sleep_until_ns(target_ns);
        if (sleep_ret < 0) {
            write_kv_i64("QC_RT_SLEEP_ERROR", sleep_ret);
            write_text("QC_RT_PERIODIC_RESULT=FAIL_SLEEP\n");
            return 1;
        }

        now_ns = monotonic_ns();
        late_ns = now_ns > target_ns ? now_ns - target_ns : 0;
        samples_ns[i] = late_ns;
        sum_ns += late_ns;

        if (late_ns > 100000UL) {
            over_100us++;
        }
        if (late_ns > 500000UL) {
            over_500us++;
        }
        if (late_ns > 1000000UL) {
            over_1000us++;
        }

        if (should_report_sample(i)) {
            char line[160];
            char *out = line;

            out = append_text(out, "QC_RT_SAMPLE index=");
            out = append_u64(out, i + 1);
            out = append_text(out, " late_ns=");
            out = append_u64(out, late_ns);
            *out++ = '\n';
            write_bytes(line, (usize)(out - line));
        }
    }

    sort_samples();
    write_kv_u64("QC_RT_LATENCY_MIN_NS", samples_ns[0]);
    write_kv_u64("QC_RT_LATENCY_MEAN_NS", sum_ns / QC_SAMPLES);
    write_kv_u64("QC_RT_LATENCY_P50_NS", samples_ns[(QC_SAMPLES * 50) / 100]);
    write_kv_u64("QC_RT_LATENCY_P95_NS", samples_ns[(QC_SAMPLES * 95) / 100]);
    write_kv_u64("QC_RT_LATENCY_P99_NS", samples_ns[(QC_SAMPLES * 99) / 100]);
    write_kv_u64("QC_RT_LATENCY_MAX_NS", samples_ns[QC_SAMPLES - 1]);
    write_kv_u64("QC_RT_OVERRUN_GT_100US", over_100us);
    write_kv_u64("QC_RT_OVERRUN_GT_500US", over_500us);
    write_kv_u64("QC_RT_OVERRUN_GT_1000US", over_1000us);
    write_text("QC_RT_PERIODIC_RESULT=PASS\n");

    return 0;
}

__attribute__((noreturn)) void _start(void) {
    int status = run_probe();

    syscall6(SYS_EXIT, status, 0, 0, 0, 0, 0);
    for (;;) {
    }
}
