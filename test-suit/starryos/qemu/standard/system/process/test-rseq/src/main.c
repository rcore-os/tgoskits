#define _GNU_SOURCE

#include "test_framework.h"
#include <errno.h>
#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_rseq
#ifdef __NR_rseq
#define SYS_rseq __NR_rseq
#elif defined(__x86_64__)
#define SYS_rseq 334
#else
#error "SYS_rseq is not defined for this architecture"
#endif
#endif

#define RSEQ_FLAG_UNREGISTER 1U
#define RSEQ_SIG 0x53053053U

#define RSEQ_CPU_ID_UNINITIALIZED ((uint32_t)-1)

int __pass = 0;
int __fail = 0;
int __skip = 0;
int __observe = 0;

struct rseq_area {
    uint32_t cpu_id_start;
    uint32_t cpu_id;
    uint64_t rseq_cs;
    uint32_t flags;
    uint32_t padding[3];
};

static _Alignas(32) struct rseq_area primary_area;
static _Alignas(32) struct rseq_area second_area;
static _Alignas(32) unsigned char misaligned_storage[sizeof(struct rseq_area) + 1];

static long rseq_call(void *addr, uint32_t len, int flags, uint32_t sig)
{
    return syscall(SYS_rseq, addr, len, flags, sig);
}

static void reset_area(struct rseq_area *area)
{
    memset(area, 0, sizeof(*area));
    area->cpu_id = RSEQ_CPU_ID_UNINITIALIZED;
}

static void part_01_invalid_arguments_before_registration(void)
{
    reset_area(&primary_area);

    CHECK_ERR(rseq_call(NULL, sizeof(primary_area), 0, RSEQ_SIG), EINVAL,
              "register rejects NULL addr with non-zero length");
    CHECK_ERR(rseq_call(&primary_area, 0, 0, RSEQ_SIG), EINVAL,
              "register rejects non-NULL addr with zero length");
    CHECK_ERR(rseq_call(&primary_area, sizeof(primary_area), 2, RSEQ_SIG), EINVAL,
              "register rejects unknown flags");
    CHECK_ERR(rseq_call(&primary_area, sizeof(primary_area) - 1, 0, RSEQ_SIG), EINVAL,
              "register rejects incorrect rseq area length");

    void *misaligned = misaligned_storage + 1;
    CHECK(((uintptr_t)misaligned % 32) != 0,
          "test fixture produces a deliberately misaligned rseq pointer");
    CHECK_ERR(rseq_call(misaligned, sizeof(primary_area), 0, RSEQ_SIG), EINVAL,
              "register rejects misaligned rseq area pointer");
}

static void part_02_bad_user_memory(void)
{
    void *page = mmap(NULL, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(page != MAP_FAILED, "mmap(PROT_NONE) test page succeeds");
    if (page == MAP_FAILED)
        return;

    CHECK_ERR(rseq_call(page, sizeof(primary_area), 0, RSEQ_SIG), EFAULT,
              "register rejects inaccessible rseq area with EFAULT");
    CHECK_RET(munmap(page, 4096), 0, "munmap(PROT_NONE) test page succeeds");
}

static void part_03_registration_lifecycle(void)
{
    reset_area(&primary_area);
    reset_area(&second_area);

    CHECK_RET(rseq_call(&primary_area, sizeof(primary_area), 0, RSEQ_SIG), 0,
              "register valid rseq area succeeds");
    CHECK_ERR(rseq_call(&primary_area, sizeof(primary_area), 0, RSEQ_SIG), EBUSY,
              "duplicate registration in same thread fails with EBUSY");
    CHECK_ERR(rseq_call(&second_area, sizeof(second_area), RSEQ_FLAG_UNREGISTER, RSEQ_SIG), EINVAL,
              "unregister rejects a different rseq area pointer");
    CHECK_ERR(rseq_call(&primary_area, sizeof(primary_area), RSEQ_FLAG_UNREGISTER, RSEQ_SIG + 1),
              EINVAL, "unregister rejects mismatched signature");
    CHECK_RET(rseq_call(&primary_area, sizeof(primary_area), RSEQ_FLAG_UNREGISTER, RSEQ_SIG), 0,
              "unregister matching rseq area succeeds");
    CHECK_ERR(rseq_call(&primary_area, sizeof(primary_area), RSEQ_FLAG_UNREGISTER, RSEQ_SIG),
              EINVAL, "second unregister fails when no area is registered");
}

static void part_04_register_after_unregister(void)
{
    reset_area(&primary_area);

    CHECK_RET(rseq_call(&primary_area, sizeof(primary_area), 0, RSEQ_SIG), 0,
              "register after prior unregister succeeds");
    CHECK_RET(rseq_call(&primary_area, sizeof(primary_area), RSEQ_FLAG_UNREGISTER, RSEQ_SIG), 0,
              "cleanup unregister succeeds");
}

int main(void)
{
    TEST_START("rseq syscall");

    CHECK(sizeof(struct rseq_area) == 32, "test rseq area size matches Linux ABI");
    CHECK(_Alignof(struct rseq_area) <= 32, "test rseq area type alignment is compatible");

    part_01_invalid_arguments_before_registration();
    part_02_bad_user_memory();
    part_03_registration_lifecycle();
    part_04_register_after_unregister();

    TEST_DONE();
}
