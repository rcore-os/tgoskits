#ifdef QCZ1_HOST_SELFTEST
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#endif

typedef unsigned long usize;
typedef unsigned long u64;
typedef long i64;
typedef int i32;
typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned char u8;

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

enum {
    QCZ1_MAGIC = 0x51435a31U,
    QCZ1_VERSION = 1,
    QCZ1_HEADER_LEN = 28,
    QCZ1_CHECKSUM_OFFSET = 24,
    QCZ1_MSG_CONTROL_SET = 1,
    QCZ1_MSG_STATE_REQ = 2,
    QCZ1_MSG_ACK = 3,
    QCZ1_MSG_STATUS = 4,
    QCZ1_MSG_ERROR = 5,
    QCZ1_FLAG_DUPLICATE = 1,
    QCZ1_STATUS_OK = 0,
    QCZ1_STATUS_DUPLICATE = 1,
    QCZ1_STATUS_BAD_CHECKSUM = 102,
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

struct parsed_frame {
    u8 msg_type;
    u16 flags;
    u32 seq;
    u64 timestamp_ns;
    u16 payload_len;
    const u8 *payload;
};

struct ack_info {
    u32 ack_seq;
    u32 status;
    u32 applied_count;
    i32 output_milli;
    u16 flags;
};

#ifdef QCZ1_HOST_SELFTEST
static void put_be32(u8 *output, u32 value);
static u32 get_be32(const u8 *input);
static i32 get_be_i32(const u8 *input);
static usize qcz1_build_frame(
    u8 *frame,
    u8 msg_type,
    u32 seq,
    const u8 *payload,
    u16 payload_len
);

enum {
    QCZ1_SELFTEST_STATUS_OK = 0,
    QCZ1_SELFTEST_STATUS_TIMEOUT = 1,
    QCZ1_SELFTEST_STATUS_MALFORMED = 2,
    QCZ1_SELFTEST_STATUS_STALE_FRAME_SEQ = 3,
    QCZ1_SELFTEST_STATUS_STALE_LAST_SEQ = 4,
    QCZ1_SELFTEST_STATUS_UNHEALTHY = 5,
    QCZ1_SELFTEST_STATUS_ERROR_COUNT = 6,
};

static int qcz1_selftest_status_mode = QCZ1_SELFTEST_STATUS_OK;
static u8 qcz1_selftest_last_frame[256];
static usize qcz1_selftest_last_frame_len;
static u64 qcz1_selftest_time_ns = 1000000000UL;
static u32 qcz1_selftest_last_seq;
static u32 qcz1_selftest_applied_count;
static u32 qcz1_selftest_duplicate_count;
static u32 qcz1_selftest_error_count;
static i32 qcz1_selftest_setpoint_milli;
static i32 qcz1_selftest_ai_score_milli;
static i32 qcz1_selftest_output_milli;

static void qcz1_selftest_put_status_payload(u8 *payload, u32 status) {
    put_be32(payload, qcz1_selftest_last_seq);
    put_be32(payload + 4, status);
    put_be32(payload + 8, (u32)qcz1_selftest_setpoint_milli);
    put_be32(payload + 12, (u32)qcz1_selftest_ai_score_milli);
    put_be32(payload + 16, (u32)qcz1_selftest_output_milli);
    put_be32(payload + 20, qcz1_selftest_applied_count);
    put_be32(payload + 24, qcz1_selftest_duplicate_count);
    put_be32(payload + 28, qcz1_selftest_error_count);
}

static long qcz1_selftest_recvfrom(u8 *response, usize response_len) {
    u8 payload[32];
    u8 msg_type;
    u32 seq;
    u32 status = QCZ1_STATUS_OK;
    u32 saved_last_seq;

    if (qcz1_selftest_last_frame_len < QCZ1_HEADER_LEN || response_len < 64) {
        return -1;
    }

    msg_type = qcz1_selftest_last_frame[5];
    seq = get_be32(qcz1_selftest_last_frame + 12);
    if (msg_type == QCZ1_MSG_CONTROL_SET) {
        if (seq == qcz1_selftest_last_seq) {
            status = QCZ1_STATUS_DUPLICATE;
            qcz1_selftest_duplicate_count++;
        } else {
            qcz1_selftest_last_seq = seq;
            qcz1_selftest_applied_count++;
            qcz1_selftest_setpoint_milli = get_be_i32(qcz1_selftest_last_frame + QCZ1_HEADER_LEN);
            qcz1_selftest_ai_score_milli = get_be_i32(qcz1_selftest_last_frame + QCZ1_HEADER_LEN + 4);
            qcz1_selftest_output_milli =
                (qcz1_selftest_setpoint_milli * qcz1_selftest_ai_score_milli) / 1000;
        }
        put_be32(payload, seq);
        put_be32(payload + 4, status);
        put_be32(payload + 8, qcz1_selftest_applied_count);
        put_be32(payload + 12, (u32)qcz1_selftest_output_milli);
        return (long)qcz1_build_frame(response, QCZ1_MSG_ACK, seq, payload, 16);
    }

    if (msg_type == QCZ1_MSG_STATE_REQ) {
        if (qcz1_selftest_status_mode == QCZ1_SELFTEST_STATUS_TIMEOUT) {
            return -1;
        }
        if (qcz1_selftest_status_mode == QCZ1_SELFTEST_STATUS_MALFORMED) {
            response[0] = 0;
            response[1] = 1;
            response[2] = 2;
            response[3] = 3;
            return 4;
        }
        if (qcz1_selftest_status_mode == QCZ1_SELFTEST_STATUS_STALE_FRAME_SEQ) {
            seq--;
        }
        if (qcz1_selftest_status_mode == QCZ1_SELFTEST_STATUS_UNHEALTHY) {
            status = QCZ1_STATUS_BAD_CHECKSUM;
        }
        if (qcz1_selftest_status_mode == QCZ1_SELFTEST_STATUS_ERROR_COUNT) {
            qcz1_selftest_error_count++;
        }

        saved_last_seq = qcz1_selftest_last_seq;
        if (qcz1_selftest_status_mode == QCZ1_SELFTEST_STATUS_STALE_LAST_SEQ
            && qcz1_selftest_last_seq > 0) {
            qcz1_selftest_last_seq--;
        }
        qcz1_selftest_put_status_payload(payload, status);
        qcz1_selftest_last_seq = saved_last_seq;

        return (long)qcz1_build_frame(response, QCZ1_MSG_STATUS, seq, payload, 32);
    }

    qcz1_selftest_error_count++;
    return -1;
}

static long syscall6(
    long number,
    long arg0,
    long arg1,
    long arg2,
    long arg3,
    long arg4,
    long arg5
) {
    (void)arg3;
    (void)arg4;
    (void)arg5;

    switch (number) {
    case SYS_CLOSE:
    case SYS_SETSOCKOPT:
    case SYS_NANOSLEEP:
        return 0;
    case SYS_WRITE:
        return (long)fwrite((const void *)arg1, 1, (usize)arg2, stdout);
    case SYS_EXIT:
        exit((int)arg0);
    case SYS_CLOCK_GETTIME:
        ((struct timespec *)arg1)->tv_sec = (long)(qcz1_selftest_time_ns / 1000000000UL);
        ((struct timespec *)arg1)->tv_nsec = (long)(qcz1_selftest_time_ns % 1000000000UL);
        qcz1_selftest_time_ns += 1000000UL;
        return 0;
    case SYS_SOCKET:
        return 3;
    case SYS_SENDTO:
        if ((usize)arg2 > sizeof(qcz1_selftest_last_frame)) {
            return -1;
        }
        memcpy(qcz1_selftest_last_frame, (const void *)arg1, (usize)arg2);
        qcz1_selftest_last_frame_len = (usize)arg2;
        return arg2;
    case SYS_RECVFROM:
        return qcz1_selftest_recvfrom((u8 *)arg1, (usize)arg2);
    default:
        return -1;
    }
}
#else
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
#endif

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

static void write_name_u64(const char *name, u64 value) {
    char line[128];
    char *cursor = line;

    cursor = append_text(cursor, name);
    cursor = append_u64(cursor, value);
    *cursor++ = '\n';
    write_bytes(line, (usize)(cursor - line));
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

static void put_be16(u8 *output, u16 value) {
    output[0] = (u8)(value >> 8);
    output[1] = (u8)value;
}

static void put_be32(u8 *output, u32 value) {
    output[0] = (u8)(value >> 24);
    output[1] = (u8)(value >> 16);
    output[2] = (u8)(value >> 8);
    output[3] = (u8)value;
}

static void put_be64(u8 *output, u64 value) {
    put_be32(output, (u32)(value >> 32));
    put_be32(output + 4, (u32)value);
}

static u16 get_be16(const u8 *input) {
    return ((u16)input[0] << 8) | (u16)input[1];
}

static u32 get_be32(const u8 *input) {
    return ((u32)input[0] << 24)
        | ((u32)input[1] << 16)
        | ((u32)input[2] << 8)
        | (u32)input[3];
}

static i32 get_be_i32(const u8 *input) {
    return (i32)get_be32(input);
}

static u64 get_be64(const u8 *input) {
    return ((u64)get_be32(input) << 32) | (u64)get_be32(input + 4);
}

static u32 qcz1_checksum(const u8 *frame, usize length) {
    u32 value = 2166136261U;
    usize index;

    for (index = 0; index < length; index++) {
        u8 byte = frame[index];

        if (index >= QCZ1_CHECKSUM_OFFSET && index < QCZ1_CHECKSUM_OFFSET + 4) {
            byte = 0;
        }
        value ^= byte;
        value *= 16777619U;
    }
    return value;
}

static usize qcz1_build_frame(
    u8 *frame,
    u8 msg_type,
    u32 seq,
    const u8 *payload,
    u16 payload_len
) {
    usize index;
    usize total_len = QCZ1_HEADER_LEN + payload_len;

    put_be32(frame, QCZ1_MAGIC);
    frame[4] = QCZ1_VERSION;
    frame[5] = msg_type;
    put_be16(frame + 6, QCZ1_HEADER_LEN);
    put_be16(frame + 8, payload_len);
    put_be16(frame + 10, 0);
    put_be32(frame + 12, seq);
    put_be64(frame + 16, monotonic_ns());
    put_be32(frame + 24, 0);

    for (index = 0; index < payload_len; index++) {
        frame[QCZ1_HEADER_LEN + index] = payload[index];
    }
    put_be32(frame + QCZ1_CHECKSUM_OFFSET, qcz1_checksum(frame, total_len));
    return total_len;
}

static int qcz1_parse_frame(const u8 *frame, usize length, struct parsed_frame *parsed) {
    u16 header_len;
    u16 payload_len;
    u32 expected;
    u32 actual;

    if (length < QCZ1_HEADER_LEN) {
        return 1;
    }
    if (get_be32(frame) != QCZ1_MAGIC || frame[4] != QCZ1_VERSION) {
        return 2;
    }
    header_len = get_be16(frame + 6);
    payload_len = get_be16(frame + 8);
    if (header_len != QCZ1_HEADER_LEN || length != (usize)header_len + payload_len) {
        return 3;
    }
    expected = get_be32(frame + QCZ1_CHECKSUM_OFFSET);
    actual = qcz1_checksum(frame, length);
    if (expected != actual) {
        return 4;
    }

    parsed->msg_type = frame[5];
    parsed->flags = get_be16(frame + 10);
    parsed->seq = get_be32(frame + 12);
    parsed->timestamp_ns = get_be64(frame + 16);
    parsed->payload_len = payload_len;
    parsed->payload = frame + header_len;
    return 0;
}

static int qcz1_parse_ack(const struct parsed_frame *parsed, u32 seq, struct ack_info *ack) {
    if (parsed->msg_type != QCZ1_MSG_ACK || parsed->seq != seq || parsed->payload_len != 16) {
        return 1;
    }

    ack->ack_seq = get_be32(parsed->payload);
    ack->status = get_be32(parsed->payload + 4);
    ack->applied_count = get_be32(parsed->payload + 8);
    ack->output_milli = get_be_i32(parsed->payload + 12);
    ack->flags = parsed->flags;
    if (ack->ack_seq != seq) {
        return 2;
    }
    return 0;
}

static void qcz1_build_control_payload(
    u8 *payload,
    i32 setpoint_milli,
    i32 ai_score_milli,
    u32 sequence
) {
    put_be32(payload, (u32)setpoint_milli);
    put_be32(payload + 4, (u32)ai_score_milli);
    put_be32(payload + 8, sequence);
}

static int udp_send_recv(
    long socket_fd,
    const struct sockaddr_in *target,
    const u8 *frame,
    usize frame_len,
    u8 *response,
    usize response_len,
    long *received_len
) {
    long sent = syscall6(
        SYS_SENDTO,
        socket_fd,
        (long)frame,
        (long)frame_len,
        0,
        (long)target,
        sizeof(*target)
    );

    if (sent != (long)frame_len) {
        *received_len = sent;
        return 1;
    }

    *received_len = syscall6(
        SYS_RECVFROM,
        socket_fd,
        (long)response,
        (long)response_len,
        0,
        0,
        0
    );
    return *received_len < 0 ? 2 : 0;
}

static int send_control_with_retry(
    long socket_fd,
    const struct sockaddr_in *target,
    u32 seq,
    i32 setpoint_milli,
    i32 ai_score_milli,
    u64 *latency_us,
    struct ack_info *ack,
    u64 *attempts_used
) {
    u8 payload[12];
    u8 frame[128];
    u8 response[256];
    usize frame_len;
    u64 attempt;

    qcz1_build_control_payload(payload, setpoint_milli, ai_score_milli, seq);
    frame_len = qcz1_build_frame(frame, QCZ1_MSG_CONTROL_SET, seq, payload, sizeof(payload));

    for (attempt = 1; attempt <= 4; attempt++) {
        long received_len = 0;
        u64 started_ns = monotonic_ns();
        int io_result = udp_send_recv(
            socket_fd,
            target,
            frame,
            frame_len,
            response,
            sizeof(response),
            &received_len
        );

        *latency_us = (monotonic_ns() - started_ns) / 1000UL;
        *attempts_used = attempt;
        if (io_result != 0) {
            continue;
        }

        {
            struct parsed_frame parsed;
            int parse_result = qcz1_parse_frame(response, (usize)received_len, &parsed);

            if (parse_result == 0 && qcz1_parse_ack(&parsed, seq, ack) == 0) {
                return 0;
            }
        }
    }
    return 1;
}

static int request_status(
    long socket_fd,
    const struct sockaddr_in *target,
    u32 seq,
    u32 expected_last_seq
) {
    u8 frame[64];
    u8 response[256];
    long received_len = 0;
    usize frame_len = qcz1_build_frame(frame, QCZ1_MSG_STATE_REQ, seq, 0, 0);
    int io_result = udp_send_recv(
        socket_fd,
        target,
        frame,
        frame_len,
        response,
        sizeof(response),
        &received_len
    );

    if (io_result != 0) {
        write_text("QC_QCZ1_STATUS_RESULT=IO_ERROR\n");
        return 1;
    }

    {
        struct parsed_frame parsed;
        int parse_result = qcz1_parse_frame(response, (usize)received_len, &parsed);

        if (parse_result != 0 || parsed.msg_type != QCZ1_MSG_STATUS || parsed.payload_len != 32) {
            write_text("QC_QCZ1_STATUS_RESULT=BAD_FRAME\n");
            return 1;
        }

        {
            u32 last_seq = get_be32(parsed.payload);
            u32 status = get_be32(parsed.payload + 4);
            i32 setpoint = get_be_i32(parsed.payload + 8);
            i32 score = get_be_i32(parsed.payload + 12);
            i32 output = get_be_i32(parsed.payload + 16);
            u32 applied = get_be32(parsed.payload + 20);
            u32 duplicates = get_be32(parsed.payload + 24);
            u32 errors = get_be32(parsed.payload + 28);
            char line[320];
            char *cursor = line;

            cursor = append_text(cursor, "QC_QCZ1_STATUS_RESULT=STATUS last_seq=");
            cursor = append_u64(cursor, last_seq);
            cursor = append_text(cursor, " status=");
            cursor = append_u64(cursor, status);
            cursor = append_text(cursor, " setpoint_milli=");
            cursor = append_i64(cursor, setpoint);
            cursor = append_text(cursor, " ai_score_milli=");
            cursor = append_i64(cursor, score);
            cursor = append_text(cursor, " output_milli=");
            cursor = append_i64(cursor, output);
            cursor = append_text(cursor, " applied_count=");
            cursor = append_u64(cursor, applied);
            cursor = append_text(cursor, " duplicate_count=");
            cursor = append_u64(cursor, duplicates);
            cursor = append_text(cursor, " error_count=");
            cursor = append_u64(cursor, errors);
            *cursor++ = '\n';
            write_bytes(line, (usize)(cursor - line));

            if (parsed.seq != seq) {
                write_text("QC_QCZ1_STATUS_VALIDATION=SEQ_MISMATCH\n");
                return 1;
            }
            if (last_seq != expected_last_seq) {
                write_text("QC_QCZ1_STATUS_VALIDATION=LAST_SEQ_MISMATCH\n");
                return 1;
            }
            if (status != QCZ1_STATUS_OK) {
                write_text("QC_QCZ1_STATUS_VALIDATION=STATUS_UNHEALTHY\n");
                return 1;
            }
            if (errors != 0) {
                write_text("QC_QCZ1_STATUS_VALIDATION=ERROR_COUNT_NONZERO\n");
                return 1;
            }
        }
    }
    write_text("QC_QCZ1_STATUS_VALIDATION=OK\n");
    return 0;
}

static void write_ack_line(
    const char *prefix,
    u32 seq,
    u64 attempts,
    int ok,
    u64 latency_us,
    const struct ack_info *ack
) {
    char line[320];
    char *cursor = line;
    int duplicate = (ack->flags & QCZ1_FLAG_DUPLICATE) != 0
        || ack->status == QCZ1_STATUS_DUPLICATE;

    cursor = append_text(cursor, prefix);
    cursor = append_text(cursor, " seq=");
    cursor = append_u64(cursor, seq);
    cursor = append_text(cursor, " attempts=");
    cursor = append_u64(cursor, attempts);
    cursor = append_text(cursor, " result=");
    cursor = append_text(cursor, ok ? "ACK" : "FAIL");
    cursor = append_text(cursor, " status=");
    cursor = append_u64(cursor, ack->status);
    cursor = append_text(cursor, " duplicate=");
    cursor = append_u64(cursor, duplicate ? 1 : 0);
    cursor = append_text(cursor, " applied_count=");
    cursor = append_u64(cursor, ack->applied_count);
    cursor = append_text(cursor, " output_milli=");
    cursor = append_i64(cursor, ack->output_milli);
    cursor = append_text(cursor, " latency_us=");
    cursor = append_u64(cursor, latency_us);
    *cursor++ = '\n';
    write_bytes(line, (usize)(cursor - line));
}

static i32 relu(i32 value) {
    return value > 0 ? value : 0;
}

static i32 clamp_i32(i32 value, i32 min_value, i32 max_value) {
    if (value < min_value) {
        return min_value;
    }
    if (value > max_value) {
        return max_value;
    }
    return value;
}

static i32 qcz1_ai_infer_milli(i32 error_milli, i32 velocity_milli, i32 load_milli) {
    i32 h0 = relu((900 * error_milli - 350 * velocity_milli + 150 * load_milli) / 1000 + 50);
    i32 h1 = relu((-200 * error_milli + 800 * velocity_milli + 300 * load_milli) / 1000 - 100);
    i32 h2 = relu((450 * error_milli + 250 * velocity_milli - 550 * load_milli) / 1000);
    i32 h3 = relu((-600 * error_milli + 100 * velocity_milli + 750 * load_milli) / 1000 + 120);
    i32 output = 720 + (700 * h0 - 450 * h1 + 550 * h2 + 350 * h3) / 4000;

    return clamp_i32(output, 650, 990);
}

static void write_ai_line(
    u32 seq,
    i32 error_milli,
    i32 velocity_milli,
    i32 load_milli,
    i32 setpoint_milli,
    i32 ai_score_milli,
    u64 infer_us,
    u64 e2e_us,
    int ok,
    const struct ack_info *ack
) {
    char line[384];
    char *cursor = line;

    cursor = append_text(cursor, "QC_AI_SEQ=");
    cursor = append_u64(cursor, seq);
    cursor = append_text(cursor, " error_milli=");
    cursor = append_i64(cursor, error_milli);
    cursor = append_text(cursor, " velocity_milli=");
    cursor = append_i64(cursor, velocity_milli);
    cursor = append_text(cursor, " load_milli=");
    cursor = append_i64(cursor, load_milli);
    cursor = append_text(cursor, " setpoint_milli=");
    cursor = append_i64(cursor, setpoint_milli);
    cursor = append_text(cursor, " ai_score_milli=");
    cursor = append_i64(cursor, ai_score_milli);
    cursor = append_text(cursor, " infer_us=");
    cursor = append_u64(cursor, infer_us);
    cursor = append_text(cursor, " e2e_us=");
    cursor = append_u64(cursor, e2e_us);
    cursor = append_text(cursor, " output_milli=");
    cursor = append_i64(cursor, ack->output_milli);
    cursor = append_text(cursor, " result=");
    cursor = append_text(cursor, ok ? "PASS" : "FAIL");
    *cursor++ = '\n';
    write_bytes(line, (usize)(cursor - line));
}

static int run_demo(void) {
    static const i32 error_table[10] = {198, 383, 545, 673, 759, 798, 789, 727, 622, 479};
    static const i32 velocity_table[10] = {492, 470, 435, 388, 330, 265, 193, 118, 42, -34};
    static const i32 load_table[10] = {369, 397, 423, 446, 466, 482, 493, 499, 500, 495};
    struct sockaddr_in target = {
        AF_INET,
        0,
        0,
        {0, 0, 0, 0, 0, 0, 0, 0},
    };
    struct timeval timeout = {0, 500000};
    long socket_fd;
    u64 reliable_success = 0;
    u64 reliable_failure = 0;
    u64 reliable_retransmits = 0;
    u64 duplicate_acks = 0;
    u64 latency_sum = 0;
    u64 latency_min = ~0UL;
    u64 latency_max = 0;
    u64 ai_success = 0;
    u64 ai_failure = 0;
    u64 ai_infer_sum = 0;
    u64 ai_e2e_sum = 0;
    u64 ai_e2e_max = 0;
    u64 ai_error_sum = 0;
    u64 manual_error_sum = 0;
    int reliable_status_ok = 0;
    int ai_status_ok = 0;
    u32 seq;

    target.sin_port = host_to_network_u16(4242);
    target.sin_addr = 0x140200c0U;

    write_text("QC_QCZ1_GUEST_DEMO=START\n");
    write_text("QC_QCZ1_TARGET=192.0.2.20:4242\n");
    write_text("QC_QCZ1_PROTOCOL=MAGIC_QCZ1_VERSION_1_HEADER28_FNV1A\n");
    write_text("QC_AI_MODEL=FIXED_POINT_MLP_RELU_3X4X1\n");

    socket_fd = syscall6(SYS_SOCKET, AF_INET, SOCK_DGRAM, IPPROTO_UDP, 0, 0, 0);
    if (socket_fd < 0) {
        write_name_u64("QC_QCZ1_SOCKET_ERROR=", (u64)(-socket_fd));
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
        write_text("QC_QCZ1_WARNING=SO_RCVTIMEO_FAILED\n");
    }

    write_text("QC_QCZ1_RELIABLE_START\n");
    for (seq = 1; seq <= 10; seq++) {
        struct ack_info ack = {0, 0xffffffffU, 0, 0, 0};
        u64 latency_us = 0;
        u64 attempts = 0;
        int ok = send_control_with_retry(
            socket_fd,
            &target,
            seq,
            1000 + (i32)seq * 10,
            800 + (i32)(seq % 5) * 25,
            &latency_us,
            &ack,
            &attempts
        ) == 0;

        write_ack_line("QC_QCZ1_RELIABLE_ACK", seq, attempts, ok, latency_us, &ack);
        reliable_retransmits += attempts > 0 ? attempts - 1 : 0;
        if (ok && (ack.status == QCZ1_STATUS_OK || ack.status == QCZ1_STATUS_DUPLICATE)) {
            reliable_success++;
            latency_sum += latency_us;
            if (latency_us < latency_min) {
                latency_min = latency_us;
            }
            if (latency_us > latency_max) {
                latency_max = latency_us;
            }
        } else {
            reliable_failure++;
        }

        if (seq == 5 || seq == 10) {
            struct ack_info dup_ack = {0, 0xffffffffU, 0, 0, 0};
            u64 dup_latency_us = 0;
            u64 dup_attempts = 0;
            int dup_ok = send_control_with_retry(
                socket_fd,
                &target,
                seq,
                1000 + (i32)seq * 10,
                800 + (i32)(seq % 5) * 25,
                &dup_latency_us,
                &dup_ack,
                &dup_attempts
            ) == 0;
            int duplicate = (dup_ack.flags & QCZ1_FLAG_DUPLICATE) != 0
                || dup_ack.status == QCZ1_STATUS_DUPLICATE;

            write_ack_line("QC_QCZ1_DUPLICATE_ACK", seq, dup_attempts, dup_ok, dup_latency_us, &dup_ack);
            reliable_retransmits += dup_attempts > 0 ? dup_attempts - 1 : 0;
            if (dup_ok && duplicate) {
                duplicate_acks++;
            }
        }
        sleep_ms(25);
    }

    write_name_u64("QC_QCZ1_RELIABLE_REQUESTS=", 10);
    write_name_u64("QC_QCZ1_RELIABLE_SUCCESSES=", reliable_success);
    write_name_u64("QC_QCZ1_RELIABLE_FAILURES=", reliable_failure);
    write_name_u64("QC_QCZ1_DUPLICATE_ACKS=", duplicate_acks);
    write_name_u64("QC_QCZ1_RETRANSMITS=", reliable_retransmits);
    if (reliable_success > 0) {
        write_name_u64("QC_QCZ1_LATENCY_MIN_US=", latency_min);
        write_name_u64("QC_QCZ1_LATENCY_MEAN_US=", latency_sum / reliable_success);
        write_name_u64("QC_QCZ1_LATENCY_MAX_US=", latency_max);
    }
    reliable_status_ok = request_status(socket_fd, &target, 1010, 10) == 0;
    write_name_u64("QC_QCZ1_RELIABLE_STATUS_OK=", reliable_status_ok ? 1 : 0);
    write_text(
        reliable_success == 10 && reliable_failure == 0 && duplicate_acks == 2 && reliable_status_ok
            ? "QC_QCZ1_RELIABLE_RESULT=PASS\n"
            : "QC_QCZ1_RELIABLE_RESULT=FAIL\n"
    );

    write_text("QC_AI_CONTROL_START\n");
    for (seq = 1001; seq <= 1010; seq++) {
        usize index = (usize)(seq - 1001);
        i32 error_milli = error_table[index];
        i32 velocity_milli = velocity_table[index];
        i32 load_milli = load_table[index];
        i32 setpoint_milli = 1000 + error_milli / 3;
        i32 ai_score_milli;
        i32 ai_output;
        i32 manual_output;
        i32 ai_error;
        i32 manual_error;
        u64 infer_start_ns = monotonic_ns();
        u64 infer_us;
        u64 e2e_us = 0;
        u64 attempts = 0;
        struct ack_info ack = {0, 0xffffffffU, 0, 0, 0};
        int ok;

        ai_score_milli = qcz1_ai_infer_milli(error_milli, velocity_milli, load_milli);
        infer_us = (monotonic_ns() - infer_start_ns) / 1000UL;
        ok = send_control_with_retry(
            socket_fd,
            &target,
            seq,
            setpoint_milli,
            ai_score_milli,
            &e2e_us,
            &ack,
            &attempts
        ) == 0;

        ai_output = (setpoint_milli * ai_score_milli) / 1000;
        manual_output = (setpoint_milli * 800) / 1000;
        ai_error = setpoint_milli - ai_output;
        manual_error = setpoint_milli - manual_output;
        if (ai_error < 0) {
            ai_error = -ai_error;
        }
        if (manual_error < 0) {
            manual_error = -manual_error;
        }
        ai_infer_sum += infer_us;
        ai_e2e_sum += e2e_us;
        if (e2e_us > ai_e2e_max) {
            ai_e2e_max = e2e_us;
        }
        ai_error_sum += (u64)ai_error;
        manual_error_sum += (u64)manual_error;

        write_ai_line(
            seq,
            error_milli,
            velocity_milli,
            load_milli,
            setpoint_milli,
            ai_score_milli,
            infer_us,
            e2e_us,
            ok,
            &ack
        );
        if (ok) {
            ai_success++;
        } else {
            ai_failure++;
        }
        sleep_ms(25);
    }

    write_name_u64("QC_AI_REQUESTS=", 10);
    write_name_u64("QC_AI_SUCCESSES=", ai_success);
    write_name_u64("QC_AI_FAILURES=", ai_failure);
    write_name_u64("QC_AI_INFER_MEAN_US=", ai_infer_sum / 10);
    write_name_u64("QC_AI_E2E_MEAN_US=", ai_e2e_sum / 10);
    write_name_u64("QC_AI_E2E_MAX_US=", ai_e2e_max);
    write_name_u64("QC_AI_CONTROL_ERROR_MEAN=", ai_error_sum / 10);
    write_name_u64("QC_MANUAL_CONTROL_ERROR_MEAN=", manual_error_sum / 10);
    ai_status_ok = request_status(socket_fd, &target, 2010, 1010) == 0;
    write_name_u64("QC_AI_STATUS_OK=", ai_status_ok ? 1 : 0);
    write_text(
        ai_success == 10 && ai_failure == 0 && ai_status_ok
            ? "QC_AI_CONTROL_RESULT=PASS\n"
            : "QC_AI_CONTROL_RESULT=FAIL\n"
    );

    syscall6(SYS_CLOSE, socket_fd, 0, 0, 0, 0, 0);
    if (reliable_success == 10 && reliable_failure == 0 && duplicate_acks == 2
        && reliable_status_ok && ai_success == 10 && ai_failure == 0 && ai_status_ok) {
        write_text("QC_QCZ1_GUEST_DEMO=PASS\n");
        return 0;
    }

    write_text("QC_QCZ1_GUEST_DEMO=FAIL\n");
    return 20;
}

#ifndef QCZ1_HOST_SELFTEST
__attribute__((noreturn)) void _start(void) {
    int status = run_demo();

    syscall6(SYS_EXIT, status, 0, 0, 0, 0, 0);
    for (;;) {
    }
}
#endif
