/*
 * aarch64 signal frames must carry and restore the full FPSIMD state.
 *
 * Linux ABI invariant: `sigcontext.__reserved` holds 16-byte-aligned TLV
 * records terminated by magic 0 / size 0. Every delivery writes exactly one
 * FPSIMD record (magic FPSIMD_MAGIC, size 528, FPSR, FPCR, V0..V31) and
 * `rt_sigreturn` installs it, so a handler may clobber every live vector
 * register and must still resume with the frame's values - including a value
 * the handler wrote back into the record. A reservation that is malformed or
 * lacks the FPSIMD record is rejected and reported as SIGSEGV
 * (`restore_sigframe`/`badframe` in `arch/arm64/kernel/signal.c`), never by
 * resuming the interrupted code.
 *
 * Delivery is deterministic: the markers are loaded immediately before a raw
 * tgkill syscall inside the same asm block, so SIGURG is delivered on the return
 * to user mode with those registers (and FPSR/FPCR) still live.
 *
 * x86_64 passes fpstate in a separate XSAVE payload, so this regression is
 * aarch64-only; other architectures build the shared arch_skip program.
 */
#define _GNU_SOURCE
#include "test_framework.h"

#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <ucontext.h>
#include <unistd.h>

/* ---- Linux aarch64 FPSIMD signal-record ABI ----------------------------- */

#define FPSIMD_RECORD_SIZE 528u
#define RECORD_HEADER_SIZE 8u
#define RECORD_ALIGN 16u
#define RECORD_FPSR_OFFSET 8u
#define RECORD_FPCR_OFFSET 12u
#define RECORD_VREGS_OFFSET 16u
#define RESERVED_BYTES 4096u
#define VREG_COUNT 32
#define VREG_BYTES 16
#define VREG_HALF_BYTES (VREG_COUNT / 2 * VREG_BYTES)
#define VREG_ALL_BYTES (VREG_COUNT * VREG_BYTES)

_Static_assert(offsetof(mcontext_t, __reserved) == 288,
               "uc_mcontext.__reserved must sit at Linux arm64 offset 288");
_Static_assert(sizeof(((mcontext_t *)0)->__reserved) == RESERVED_BYTES,
               "the sigcontext reservation is 4096 bytes");
_Static_assert(sizeof(mcontext_t) == 4384,
               "aarch64 sigcontext is fault_address/regs/sp/pc/pstate/reserved");
_Static_assert(RECORD_VREGS_OFFSET + VREG_ALL_BYTES == FPSIMD_RECORD_SIZE,
               "the FPSIMD record is header + fpsr + fpcr + 32 vector registers");
/* The guest signal header (mirroring uapi/asm/sigcontext.h) owns FPSIMD_MAGIC;
 * pin its value instead of redefining the macro. */
_Static_assert(FPSIMD_MAGIC == 0x46508001u,
               "the guest signal header must expose the Linux arm64 FPSIMD magic");
_Static_assert(ESR_MAGIC == 0x45535201u,
               "the guest signal header must expose the Linux arm64 ESR magic");

/* FZ (bit 24) and DN (bit 25) are independent, benign FPCR control bits and the
 * cumulative FPSR flags are safe to write. Every marker is distinct so a
 * partially restored control register cannot pass. */
#define FPSR_MAIN 0x10u
#define FPCR_MAIN (1u << 24)
#define FPSR_OUTER 0x02u
#define FPCR_OUTER ((1u << 24) | (1u << 25))
#define FPSR_HANDLER 0x08u
#define FPCR_HANDLER (1u << 25)

/* ---- register plumbing -------------------------------------------------- */

/* `x9` points at a 16-byte-aligned 512-byte block of 128-bit patterns. */
#define LOAD_V0_V31                                 \
    "ldp q0,  q1,  [x9, #0]\n\t"                    \
    "ldp q2,  q3,  [x9, #32]\n\t"                   \
    "ldp q4,  q5,  [x9, #64]\n\t"                   \
    "ldp q6,  q7,  [x9, #96]\n\t"                   \
    "ldp q8,  q9,  [x9, #128]\n\t"                  \
    "ldp q10, q11, [x9, #160]\n\t"                  \
    "ldp q12, q13, [x9, #192]\n\t"                  \
    "ldp q14, q15, [x9, #224]\n\t"                  \
    "ldp q16, q17, [x9, #256]\n\t"                  \
    "ldp q18, q19, [x9, #288]\n\t"                  \
    "ldp q20, q21, [x9, #320]\n\t"                  \
    "ldp q22, q23, [x9, #352]\n\t"                  \
    "ldp q24, q25, [x9, #384]\n\t"                  \
    "ldp q26, q27, [x9, #416]\n\t"                  \
    "ldp q28, q29, [x9, #448]\n\t"                  \
    "ldp q30, q31, [x9, #480]\n\t"

#define STORE_V0_V31                                \
    "stp q0,  q1,  [x9, #0]\n\t"                    \
    "stp q2,  q3,  [x9, #32]\n\t"                   \
    "stp q4,  q5,  [x9, #64]\n\t"                   \
    "stp q6,  q7,  [x9, #96]\n\t"                   \
    "stp q8,  q9,  [x9, #128]\n\t"                  \
    "stp q10, q11, [x9, #160]\n\t"                  \
    "stp q12, q13, [x9, #192]\n\t"                  \
    "stp q14, q15, [x9, #224]\n\t"                  \
    "stp q16, q17, [x9, #256]\n\t"                  \
    "stp q18, q19, [x9, #288]\n\t"                  \
    "stp q20, q21, [x9, #320]\n\t"                  \
    "stp q22, q23, [x9, #352]\n\t"                  \
    "stp q24, q25, [x9, #384]\n\t"                  \
    "stp q26, q27, [x9, #416]\n\t"                  \
    "stp q28, q29, [x9, #448]\n\t"                  \
    "stp q30, q31, [x9, #480]\n\t"

#define VREG_CLOBBERS "v0", "v1", "v2", "v3", "v4", "v5", "v6", "v7",   \
    "v8", "v9", "v10", "v11", "v12", "v13", "v14", "v15",               \
    "v16", "v17", "v18", "v19", "v20", "v21", "v22", "v23",             \
    "v24", "v25", "v26", "v27", "v28", "v29", "v30", "v31"

/* ---- patterns and observations ------------------------------------------ */

static uint8_t marker[VREG_COUNT][VREG_BYTES] __attribute__((aligned(RECORD_ALIGN)));
static uint8_t outer_marker[VREG_COUNT][VREG_BYTES] __attribute__((aligned(RECORD_ALIGN)));
static uint8_t handler_junk[VREG_COUNT][VREG_BYTES] __attribute__((aligned(RECORD_ALIGN)));
static uint8_t inner_junk[VREG_COUNT][VREG_BYTES] __attribute__((aligned(RECORD_ALIGN)));
static uint8_t frame_edit[VREG_COUNT][VREG_BYTES] __attribute__((aligned(RECORD_ALIGN)));
static uint8_t inner_edit[VREG_COUNT][VREG_BYTES] __attribute__((aligned(RECORD_ALIGN)));
static uint8_t observed[VREG_COUNT][VREG_BYTES] __attribute__((aligned(RECORD_ALIGN)));
static uint8_t nested_observed[VREG_COUNT][VREG_BYTES] __attribute__((aligned(RECORD_ALIGN)));

/* Read through `mrs xN, fpsr/fpcr`, so the asm outputs must be 64-bit. */
static uint64_t fpsr_observed;
static uint64_t fpcr_observed;

static volatile int handler_depth;
static volatile int outer_seen;
static volatile int inner_seen;
static volatile int unexpected_handler;
static volatile int frame_magic_ok[2];
static volatile int frame_size_ok[2];
static volatile int frame_layout_ok[2];
static volatile int frame_vregs_ok[2];
static volatile int frame_control_ok[2];
static volatile int nested_vregs_ok;

static void fill_pattern(uint8_t (*pattern)[VREG_BYTES], uint8_t tag)
{
    for (unsigned int reg = 0; reg < VREG_COUNT; ++reg) {
        for (unsigned int byte = 0; byte < VREG_BYTES; ++byte) {
            pattern[reg][byte] = (uint8_t)(tag ^ (reg * 17u) ^ (byte * 31u) ^ (reg << 4));
        }
    }
}

static uint32_t read_u32(const uint8_t *bytes)
{
    uint32_t value;
    memcpy(&value, bytes, sizeof(value));
    return value;
}

static void write_u32(uint8_t *bytes, uint32_t value)
{
    memcpy(bytes, &value, sizeof(value));
}

static uint64_t read_fpsr(void)
{
    uint64_t value;
    __asm__ volatile("mrs %0, fpsr" : "=r"(value));
    return value;
}

static uint64_t read_fpcr(void)
{
    uint64_t value;
    __asm__ volatile("mrs %0, fpcr" : "=r"(value));
    return value;
}

static void write_fp_control(uint64_t fpsr, uint64_t fpcr)
{
    __asm__ volatile("msr fpsr, %0\n\tmsr fpcr, %1\n\tisb" ::"r"(fpsr), "r"(fpcr));
}

/* Walks the reservation the way Linux parses it: 16-byte-aligned TLV records
 * terminated by a magic == 0 / size == 0 record. Returns non-zero when exactly
 * one well-formed FPSIMD record was found and reports the terminator. */
static int find_fpsimd_record(uint8_t *reserved, size_t *record_offset, int *terminator)
{
    size_t offset = 0;
    int found = 0;

    *record_offset = 0;
    *terminator = 0;
    while (offset + RECORD_HEADER_SIZE <= RESERVED_BYTES) {
        uint32_t magic = read_u32(reserved + offset);
        uint32_t size = read_u32(reserved + offset + 4);

        if (magic == 0 && size == 0) {
            *terminator = 1;
            break;
        }
        if (size < RECORD_HEADER_SIZE || size % RECORD_ALIGN != 0 ||
            size > RESERVED_BYTES - offset) {
            return 0;
        }
        if (magic == FPSIMD_MAGIC) {
            if (found || size != FPSIMD_RECORD_SIZE) {
                return 0;
            }
            *record_offset = offset;
            found = 1;
        }
        offset += size;
    }
    return found;
}

static void signal_handler(int signo, siginfo_t *info, void *opaque)
{
    int level = handler_depth + 1;
    int slot;
    uint8_t *reserved;
    size_t record_offset = 0;
    int terminator = 0;
    int found;
    const uint8_t *expected_vregs;
    uint32_t expected_fpsr;
    uint32_t expected_fpcr;
    const void *junk;
    ucontext_t *context = opaque;

    (void)info;
    handler_depth = level;
    if (signo != SIGURG || level > 2) {
        unexpected_handler = 1;
        handler_depth = level - 1;
        return;
    }
    slot = level - 1;

    reserved = (uint8_t *)(void *)&context->uc_mcontext.__reserved[0];
    found = find_fpsimd_record(reserved, &record_offset, &terminator);

    /* Clobber every live vector register inline, so any value surviving
     * rt_sigreturn can only have come from the frame the kernel installed. A
     * helper call is deliberately avoided: its AAPCS d8-d15 callee-save
     * restore would put part of the clobbered state back. */
    junk = slot == 0 ? (const void *)handler_junk : (const void *)inner_junk;
    __asm__ volatile("mov x9, %[src]\n\t" LOAD_V0_V31
                     :
                     : [src] "r"(junk)
                     : "x9", VREG_CLOBBERS, "memory");
    if (slot == 0) {
        outer_seen = 1;
    } else {
        inner_seen = 1;
    }

    frame_magic_ok[slot] = found;
    frame_size_ok[slot] = found && read_u32(reserved + record_offset + 4) == FPSIMD_RECORD_SIZE;
    frame_layout_ok[slot] = found && terminator == 1;
    if (!found) {
        /* Nothing sane to verify or edit; the outer level still reports the
         * missing record through the live-register comparison. */
        handler_depth = level - 1;
        return;
    }

    expected_vregs = slot == 0 ? &marker[0][0] : &outer_marker[0][0];
    expected_fpsr = slot == 0 ? FPSR_MAIN : FPSR_OUTER;
    expected_fpcr = slot == 0 ? FPCR_MAIN : FPCR_OUTER;
    frame_vregs_ok[slot] =
        memcmp(reserved + record_offset + RECORD_VREGS_OFFSET, expected_vregs, VREG_ALL_BYTES) == 0;
    frame_control_ok[slot] =
        read_u32(reserved + record_offset + RECORD_FPSR_OFFSET) == expected_fpsr &&
        read_u32(reserved + record_offset + RECORD_FPCR_OFFSET) == expected_fpcr;

    if (slot == 0) {
        pid_t pid = getpid();
        pid_t tid = gettid();

        /* Nested delivery: a second SIGURG (SA_NODEFER keeps it unblocked) must
         * capture this handler's live state into its own record, and returning
         * from it must install that record without disturbing the pending outer
         * frame. */
        __asm__ volatile("mov x9, %[src]\n\t" LOAD_V0_V31
                         "msr fpsr, %[fpsr]\n\t"
                         "msr fpcr, %[fpcr]\n\t"
                         "isb\n\t"
                         "mov x0, %[pid]\n\t"
                         "mov x1, %[tid]\n\t"
                         "mov x2, %[signo]\n\t"
                         "mov x8, %[tgkill]\n\t"
                         "svc #0\n\t"
                         "mov x9, %[dst]\n\t" STORE_V0_V31
                         :
                         : [src] "r"(outer_marker), [dst] "r"(nested_observed),
                           [pid] "r"((uint64_t)pid), [tid] "r"((uint64_t)tid),
                           [signo] "r"((uint64_t)SIGURG), [tgkill] "r"((uint64_t)SYS_tgkill),
                           [fpsr] "r"((uint64_t)FPSR_OUTER), [fpcr] "r"((uint64_t)FPCR_OUTER)
                         : "x0", "x1", "x2", "x8", "x9", VREG_CLOBBERS, "memory");

        nested_vregs_ok = memcmp(&nested_observed[0][0], inner_edit, VREG_HALF_BYTES) == 0 &&
                          memcmp(&nested_observed[VREG_COUNT / 2][0],
                                 &outer_marker[VREG_COUNT / 2][0],
                                 VREG_HALF_BYTES) == 0;

        /* Publish the edits the outer rt_sigreturn must install. */
        memcpy(reserved + record_offset + RECORD_VREGS_OFFSET + VREG_HALF_BYTES,
               &frame_edit[VREG_COUNT / 2][0], VREG_HALF_BYTES);
        write_u32(reserved + record_offset + RECORD_FPSR_OFFSET, FPSR_HANDLER);
        write_u32(reserved + record_offset + RECORD_FPCR_OFFSET, FPCR_HANDLER);
    } else {
        /* Only the low half is edited: the untouched high half of the inner
         * record must still return this handler's marker values. */
        memcpy(reserved + record_offset + RECORD_VREGS_OFFSET, inner_edit, VREG_HALF_BYTES);
    }
    handler_depth = level - 1;
}

/* ---- rejected frames ---------------------------------------------------- */

/* Linux rejects a reservation that is malformed or has no FPSIMD record and
 * `rt_sigreturn` then injects SIGSEGV instead of resuming the interrupted code.
 * Every case below corrupts the handler's own frame in a forked child, so the
 * parent observes the kill and a normal resume shows up as a distinct exit. */
enum bad_frame_case {
    BAD_FRAME_MISSING,
    BAD_FRAME_DUPLICATE,
    BAD_FRAME_SIZE,
    BAD_FRAME_OVERRUN,
    BAD_FRAME_UNTERMINATED,
};

/* Distinct exit code for a child that resumed past a rejected frame. */
#define BAD_FRAME_RESUMED_EXIT 42

/* Distinctive control markers: one cumulative FPSR flag and the FPCR default-NaN
 * bit, both benign and never touched by the integer code in this child. The
 * architectural initial state is zero, so reading them back proves the
 * interrupted state stayed live across the rejection. */
#define BADFRAME_FPSR 0x10u
#define BADFRAME_FPCR (1u << 25)

static int active_bad_frame_case;

static void bad_frame_handler(int signo, siginfo_t *info, void *opaque)
{
    size_t record_offset = 0;
    int terminator = 0;
    uint8_t *reserved;
    ucontext_t *context = opaque;

    (void)info;
    if (signo != SIGURG) {
        return;
    }
    reserved = (uint8_t *)(void *)&context->uc_mcontext.__reserved[0];
    if (!find_fpsimd_record(reserved, &record_offset, &terminator) || !terminator) {
        /* Only a well-formed list is worth corrupting; otherwise the child
         * falls back to its resume marker. */
        return;
    }
    switch (active_bad_frame_case) {
    case BAD_FRAME_MISSING:
        /* Drop the record so the list is just its terminator. */
        write_u32(reserved, 0);
        write_u32(reserved + 4, 0);
        break;
    case BAD_FRAME_DUPLICATE:
        /* A second FPSIMD record, with the terminator moved behind it. */
        memcpy(reserved + FPSIMD_RECORD_SIZE, reserved, FPSIMD_RECORD_SIZE);
        write_u32(reserved + 2 * FPSIMD_RECORD_SIZE, 0);
        write_u32(reserved + 2 * FPSIMD_RECORD_SIZE + 4, 0);
        break;
    case BAD_FRAME_SIZE:
        /* In-range size that is not the ABI record size. */
        write_u32(reserved + 4, 2 * FPSIMD_RECORD_SIZE);
        break;
    case BAD_FRAME_OVERRUN:
        /* Declared size runs past the reservation. */
        write_u32(reserved + 4, RESERVED_BYTES + 16);
        break;
    case BAD_FRAME_UNTERMINATED:
        /* Fill the rest with ignored ESR records: the list never terminates. */
        for (size_t offset = record_offset + FPSIMD_RECORD_SIZE;
             offset + RECORD_HEADER_SIZE <= RESERVED_BYTES;
             offset += RECORD_ALIGN) {
            write_u32(reserved + offset, ESR_MAGIC);
            write_u32(reserved + offset + 4, RECORD_ALIGN);
        }
        break;
    }
}

static void check_bad_frame_case(const char *description, int case_index)
{
    char message[192];
    pid_t child = fork();
    int status = 0;
    pid_t waited;
    int observed;

    if (child == 0) {
        struct sigaction action;

        active_bad_frame_case = case_index;
        memset(&action, 0, sizeof(action));
        action.sa_sigaction = bad_frame_handler;
        action.sa_flags = SA_SIGINFO | SA_NODEFER;
        sigemptyset(&action.sa_mask);
        if (sigaction(SIGURG, &action, NULL) != 0) {
            _exit(BAD_FRAME_RESUMED_EXIT);
        }
        (void)syscall(SYS_tgkill, getpid(), gettid(), SIGURG);
        fprintf(stderr, "  RESUMED | %s | rejected frame resumed\n", description);
        _exit(BAD_FRAME_RESUMED_EXIT);
    }
    if (child < 0) {
        CHECK(0, "fork a bad-frame child");
        return;
    }

    waited = waitpid(child, &status, 0);
    observed = waited == child ? status : -1;
    snprintf(message, sizeof(message), "%s (wait status=0x%x)", description,
             (unsigned int)observed);
    CHECK(waited == child && WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV, message);
}

/* A rejected frame must leave the interrupted FP state live: Linux only drops
 * the CPU ownership of the saved state (`fpsimd_flush_task_state`) and never
 * clears register contents. The interrupted code publishes a distinctive
 * control state before self-delivering, and the SIGSEGV handler reads it before
 * any other work, so "left live" and "reset to the initial state" differ. */
static void badframe_segv_handler(int signo, siginfo_t *info, void *opaque)
{
    uint64_t fpsr;
    uint64_t fpcr;
    int kept;

    (void)signo;
    (void)info;
    (void)opaque;
    __asm__ volatile("mrs %0, fpsr\n\tmrs %1, fpcr" : "=r"(fpsr), "=r"(fpcr));
    kept = ((uint32_t)fpsr & BADFRAME_FPSR) == BADFRAME_FPSR &&
           ((uint32_t)fpcr & BADFRAME_FPCR) == BADFRAME_FPCR;
    _exit(kept ? 0 : 1);
}

static void check_rejected_frame_keeps_live_fp(void)
{
    char message[192];
    pid_t child = fork();
    int status = 0;
    pid_t waited;

    if (child == 0) {
        struct sigaction action;

        write_fp_control(BADFRAME_FPSR, BADFRAME_FPCR);
        active_bad_frame_case = BAD_FRAME_MISSING;
        memset(&action, 0, sizeof(action));
        action.sa_sigaction = bad_frame_handler;
        action.sa_flags = SA_SIGINFO | SA_NODEFER;
        sigemptyset(&action.sa_mask);
        if (sigaction(SIGURG, &action, NULL) != 0) {
            _exit(BAD_FRAME_RESUMED_EXIT);
        }
        memset(&action, 0, sizeof(action));
        action.sa_sigaction = badframe_segv_handler;
        action.sa_flags = SA_SIGINFO;
        sigemptyset(&action.sa_mask);
        if (sigaction(SIGSEGV, &action, NULL) != 0) {
            _exit(BAD_FRAME_RESUMED_EXIT);
        }
        (void)syscall(SYS_tgkill, getpid(), gettid(), SIGURG);
        fprintf(stderr, "  RESUMED | rejected frame resumed\n");
        _exit(BAD_FRAME_RESUMED_EXIT);
    }
    if (child < 0) {
        CHECK(0, "fork a bad-frame child");
        return;
    }

    waited = waitpid(child, &status, 0);
    snprintf(message, sizeof(message),
             "rejected frame keeps the live FP control state (wait status=0x%x)",
             (unsigned int)(waited == child ? status : -1));
    CHECK(waited == child && WIFEXITED(status) && WEXITSTATUS(status) == 0, message);
}

/* Sets the markers live, self-delivers SIGURG through a raw tgkill syscall, and
 * copies the live vector registers right after rt_sigreturn returns. */
static void run_signal_round(pid_t pid, pid_t tid)
{
    uint64_t fpsr_before = read_fpsr();
    uint64_t fpcr_before = read_fpcr();

    __asm__ volatile("mov x9, %[src]\n\t" LOAD_V0_V31
                     "msr fpsr, %[fpsr]\n\t"
                     "msr fpcr, %[fpcr]\n\t"
                     "isb\n\t"
                     "mov x0, %[pid]\n\t"
                     "mov x1, %[tid]\n\t"
                     "mov x2, %[signo]\n\t"
                     "mov x8, %[tgkill]\n\t"
                     "svc #0\n\t"
                     "mov x9, %[dst]\n\t" STORE_V0_V31
                     "mrs %[fpsr_out], fpsr\n\t"
                     "mrs %[fpcr_out], fpcr\n\t"
                     : [fpsr_out] "=r"(fpsr_observed), [fpcr_out] "=r"(fpcr_observed)
                     : [src] "r"(marker), [dst] "r"(observed),
                       [pid] "r"((uint64_t)pid), [tid] "r"((uint64_t)tid),
                       [signo] "r"((uint64_t)SIGURG), [tgkill] "r"((uint64_t)SYS_tgkill),
                       [fpsr] "r"((uint64_t)FPSR_MAIN), [fpcr] "r"((uint64_t)FPCR_MAIN)
                     : "x0", "x1", "x2", "x8", "x9", VREG_CLOBBERS, "memory");

    write_fp_control(fpsr_before, fpcr_before);
}

int main(void)
{
    struct sigaction action;
    sigset_t unblock;
    pid_t pid;
    pid_t tid;

    TEST_START("aarch64 signal frames carry and restore FPSIMD state");

    fill_pattern(marker, 0x11);
    fill_pattern(outer_marker, 0x27);
    fill_pattern(handler_junk, 0x5a);
    fill_pattern(inner_junk, 0x3c);
    fill_pattern(frame_edit, 0x6d);
    fill_pattern(inner_edit, 0x84);

    memset(&action, 0, sizeof(action));
    action.sa_sigaction = signal_handler;
    action.sa_flags = SA_SIGINFO | SA_NODEFER;
    sigemptyset(&action.sa_mask);
    CHECK(sigaction(SIGURG, &action, NULL) == 0,
          "install SA_SIGINFO|SA_NODEFER SIGURG handler");

    sigemptyset(&unblock);
    sigaddset(&unblock, SIGURG);
    CHECK(sigprocmask(SIG_UNBLOCK, &unblock, NULL) == 0, "SIGURG is unblocked");

    pid = getpid();
    tid = gettid();
    run_signal_round(pid, tid);

    CHECK(outer_seen == 1, "self-delivered SIGURG reached the handler");
    CHECK(inner_seen == 1, "nested SIGURG (SA_NODEFER) reached the handler");
    CHECK(unexpected_handler == 0, "the handler only observed the expected deliveries");
    CHECK(handler_depth == 0, "the handler returned through ordinary nesting");

    CHECK(frame_magic_ok[0], "outer frame carries an FPSIMD record (magic 0x46508001)");
    CHECK(frame_size_ok[0], "outer FPSIMD record declares the Linux 528-byte size");
    CHECK(frame_layout_ok[0], "outer reservation ends with a null terminator record");
    CHECK(frame_vregs_ok[0], "outer FPSIMD record mirrors the interrupted V0..V31");
    CHECK(frame_control_ok[0], "outer FPSIMD record mirrors the interrupted FPSR/FPCR");

    CHECK(frame_magic_ok[1], "nested frame carries its own FPSIMD record");
    CHECK(frame_size_ok[1], "nested FPSIMD record declares the Linux 528-byte size");
    CHECK(frame_layout_ok[1], "nested reservation ends with a null terminator record");
    CHECK(frame_vregs_ok[1], "nested FPSIMD record mirrors the outer handler's V0..V31");
    CHECK(frame_control_ok[1], "nested FPSIMD record mirrors the outer handler's FPSR/FPCR");
    CHECK(nested_vregs_ok, "inner rt_sigreturn installs only the inner record");

    CHECK(memcmp(&observed[0][0], marker, VREG_HALF_BYTES) == 0,
          "rt_sigreturn restores the interrupted V0..V15");
    CHECK(memcmp(&observed[VREG_COUNT / 2][0], &frame_edit[VREG_COUNT / 2][0],
                 VREG_HALF_BYTES) == 0,
          "rt_sigreturn installs the handler's V16..V31 frame edit");
    CHECK((uint32_t)fpcr_observed == FPCR_HANDLER, "rt_sigreturn installs the handler's FPCR");
    CHECK((uint32_t)fpsr_observed == FPSR_HANDLER, "rt_sigreturn installs the handler's FPSR");

    check_bad_frame_case("frame without an FPSIMD record is reported as SIGSEGV",
                         BAD_FRAME_MISSING);
    check_bad_frame_case("frame with a duplicated FPSIMD record is reported as SIGSEGV",
                         BAD_FRAME_DUPLICATE);
    check_bad_frame_case("frame with a non-ABI FPSIMD record size is reported as SIGSEGV",
                         BAD_FRAME_SIZE);
    check_bad_frame_case("frame whose record overruns the reservation is reported as SIGSEGV",
                         BAD_FRAME_OVERRUN);
    check_bad_frame_case("frame without a terminator record is reported as SIGSEGV",
                         BAD_FRAME_UNTERMINATED);
    check_rejected_frame_keeps_live_fp();

    TEST_DONE();
}
