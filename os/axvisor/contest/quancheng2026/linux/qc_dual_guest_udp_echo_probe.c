typedef unsigned long usize;
typedef unsigned long u64;

enum {
    SYS_CLOSE = 57,
    SYS_WRITE = 64,
    SYS_EXIT = 93,
    SYS_NANOSLEEP = 101,
    SYS_CLOCK_GETTIME = 113,
    SYS_SOCKET = 198,
    SYS_SENDTO = 206,
    SYS_RECVFROM = 207,
    SYS_SETSOCKOPT = 208,
};

enum {
    AF_INET = 2,
    SOCK_DGRAM = 2,
    IPPROTO_UDP = 17,
    SOL_SOCKET = 1,
    SO_RCVTIMEO = 20,
    CLOCK_MONOTONIC = 1,
};

struct timespec {
    long tv_sec;
    long tv_nsec;
};

struct timeval {
    long tv_sec;
    long tv_usec;
};

struct sockaddr_in {
    unsigned short sin_family;
    unsigned short sin_port;
    unsigned int sin_addr;
    unsigned char sin_zero[8];
};

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

static int memory_equal(const char *left, const char *right, usize length) {
    usize index;

    for (index = 0; index < length; index++) {
        if (left[index] != right[index]) {
            return 0;
        }
    }
    return 1;
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

static unsigned short host_to_network_u16(unsigned short value) {
    return (unsigned short)((value >> 8) | (value << 8));
}

static u64 monotonic_ns(void) {
    struct timespec value;
    long result = syscall6(
        SYS_CLOCK_GETTIME,
        CLOCK_MONOTONIC,
        (long)&value,
        0,
        0,
        0,
        0
    );

    if (result < 0) {
        return 0;
    }
    return (u64)value.tv_sec * 1000000000UL + (u64)value.tv_nsec;
}

static void sleep_ms(long milliseconds) {
    struct timespec delay;

    delay.tv_sec = milliseconds / 1000;
    delay.tv_nsec = (milliseconds % 1000) * 1000000L;
    syscall6(SYS_NANOSLEEP, (long)&delay, 0, 0, 0, 0, 0);
}

static void write_attempt(
    u64 sequence,
    u64 attempt,
    const char *status,
    u64 rtt_us,
    u64 detail
) {
    char line[192];
    char *cursor = line;

    cursor = append_text(cursor, "QC_UDP_SEQUENCE=");
    cursor = append_u64(cursor, sequence);
    cursor = append_text(cursor, " ATTEMPT=");
    cursor = append_u64(cursor, attempt);
    cursor = append_text(cursor, " STATUS=");
    cursor = append_text(cursor, status);
    if (rtt_us > 0) {
        cursor = append_text(cursor, " RTT_US=");
        cursor = append_u64(cursor, rtt_us);
    }
    if (detail > 0) {
        cursor = append_text(cursor, " DETAIL=");
        cursor = append_u64(cursor, detail);
    }
    *cursor++ = '\n';
    write_bytes(line, (usize)(cursor - line));
}

static void write_metric(const char *name, u64 value) {
    char line[96];
    char *cursor = line;

    cursor = append_text(cursor, name);
    cursor = append_u64(cursor, value);
    *cursor++ = '\n';
    write_bytes(line, (usize)(cursor - line));
}

static int run_probe(void) {
    static char payload[] = "QC_DUAL_GUEST_UDP_ECHO sequence=0000";
    static char response[256];
    const usize payload_length = sizeof(payload) - 1;
    struct sockaddr_in target = {
        AF_INET,
        0,
        0,
        {0, 0, 0, 0, 0, 0, 0, 0},
    };
    struct timeval timeout = {2, 0};
    long socket_fd;
    u64 successes = 0;
    u64 failures = 0;
    u64 retries = 0;
    u64 rtt_sum_us = 0;
    u64 rtt_min_us = ~0UL;
    u64 rtt_max_us = 0;
    u64 sequence;

    target.sin_port = host_to_network_u16(4242);
    target.sin_addr = 0x140200c0U;

    write_text("QC_UDP_TARGET=192.0.2.20:4242\n");
    write_text("QC_UDP_TRANSPORT=IPv4/UDP\n");
    write_text("QC_UDP_PAYLOAD_VALIDATION=BYTE_EXACT\n");

    socket_fd = syscall6(SYS_SOCKET, AF_INET, SOCK_DGRAM, IPPROTO_UDP, 0, 0, 0);
    if (socket_fd < 0) {
        write_attempt(0, 0, "SOCKET_ERROR", 0, (u64)(-socket_fd));
        return 10;
    }

    if (syscall6(
            SYS_SETSOCKOPT,
            socket_fd,
            SOL_SOCKET,
            SO_RCVTIMEO,
            (long)&timeout,
            sizeof(timeout),
            0
        ) < 0) {
        write_text("QC_UDP_WARNING=SO_RCVTIMEO_FAILED\n");
    }

    for (sequence = 0; sequence < 20; sequence++) {
        u64 attempt;
        int sequence_passed = 0;
        usize digit_offset = payload_length - 4;

        payload[digit_offset] = (char)('0' + ((sequence / 1000) % 10));
        payload[digit_offset + 1] = (char)('0' + ((sequence / 100) % 10));
        payload[digit_offset + 2] = (char)('0' + ((sequence / 10) % 10));
        payload[digit_offset + 3] = (char)('0' + (sequence % 10));

        for (attempt = 1; attempt <= 3; attempt++) {
            u64 started_ns = monotonic_ns();
            long sent = syscall6(
                SYS_SENDTO,
                socket_fd,
                (long)payload,
                (long)payload_length,
                0,
                (long)&target,
                sizeof(target)
            );
            long received;
            u64 elapsed_us;

            if (sent != (long)payload_length) {
                write_attempt(
                    sequence,
                    attempt,
                    "SEND_ERROR",
                    0,
                    sent < 0 ? (u64)(-sent) : (u64)sent
                );
                if (attempt < 3) {
                    retries++;
                }
                continue;
            }

            received = syscall6(
                SYS_RECVFROM,
                socket_fd,
                (long)response,
                sizeof(response),
                0,
                0,
                0
            );
            elapsed_us = (monotonic_ns() - started_ns) / 1000UL;

            if (received == (long)payload_length
                && memory_equal(payload, response, payload_length)) {
                write_attempt(sequence, attempt, "PASS", elapsed_us, (u64)received);
                successes++;
                rtt_sum_us += elapsed_us;
                if (elapsed_us < rtt_min_us) {
                    rtt_min_us = elapsed_us;
                }
                if (elapsed_us > rtt_max_us) {
                    rtt_max_us = elapsed_us;
                }
                sequence_passed = 1;
                break;
            }

            if (received < 0) {
                write_attempt(
                    sequence,
                    attempt,
                    "RECV_ERROR",
                    elapsed_us,
                    (u64)(-received)
                );
            } else {
                write_attempt(
                    sequence,
                    attempt,
                    "MISMATCH",
                    elapsed_us,
                    (u64)received
                );
            }
            if (attempt < 3) {
                retries++;
            }
        }

        if (!sequence_passed) {
            failures++;
        }
        sleep_ms(50);
    }

    syscall6(SYS_CLOSE, socket_fd, 0, 0, 0, 0, 0);

    write_metric("QC_UDP_REQUESTS=", 20);
    write_metric("QC_UDP_SUCCESSES=", successes);
    write_metric("QC_UDP_FAILURES=", failures);
    write_metric("QC_UDP_RETRIES=", retries);
    if (successes > 0) {
        write_metric("QC_UDP_RTT_MIN_US=", rtt_min_us);
        write_metric("QC_UDP_RTT_MEAN_US=", rtt_sum_us / successes);
        write_metric("QC_UDP_RTT_MAX_US=", rtt_max_us);
    }

    if (successes == 20 && failures == 0) {
        write_text("QC_DUAL_GUEST_UDP_ECHO_RESULT=PASS\n");
        return 0;
    }

    write_text("QC_DUAL_GUEST_UDP_ECHO_RESULT=FAIL\n");
    return 20;
}

__attribute__((noreturn)) void _start(void) {
    int status = run_probe();

    syscall6(SYS_EXIT, status, 0, 0, 0, 0, 0);
    for (;;) {
    }
}
