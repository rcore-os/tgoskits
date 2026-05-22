#define _GNU_SOURCE

#include "test_framework.h"
#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

/* membarrier command constants matching the StarryOS implementation */
#define MEMBARRIER_CMD_QUERY                     0
#define MEMBARRIER_CMD_GLOBAL                    1
#define MEMBARRIER_CMD_GLOBAL_EXPEDITED          2
#define MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED 3
#define MEMBARRIER_CMD_PRIVATE_EXPEDITED         4
#define MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED 5

/* These are NOT supported by current StarryOS impl */
#define MEMBARRIER_CMD_PRIVATE_EXPEDITED_SYNC_CORE     32
#define MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED_SYNC_CORE 64
#define MEMBARRIER_CMD_PRIVATE_EXPEDITED_RSEQ          128
#define MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED_RSEQ 256

/*
 * Expected SUPPORTED_COMMANDS bitmask from the StarryOS implementation:
 * (1 << GLOBAL) | (1 << GLOBAL_EXPEDITED) |
 * (1 << REGISTER_GLOBAL_EXPEDITED) | (1 << PRIVATE_EXPEDITED) |
 * (1 << REGISTER_PRIVATE_EXPEDITED) = 0b111110 = 62
 */
#define EXPECTED_SUPPORTED_MASK 62

static int membarrier(int cmd, unsigned flags, int cpu_id)
{
    return syscall(SYS_membarrier, cmd, flags, cpu_id);
}

static void part_01_query(void)
{
    long ret = membarrier(MEMBARRIER_CMD_QUERY, 0, 0);
    if (ret < 0) {
        printf("  FAIL: QUERY returned %ld, errno=%d\n", ret, errno);
        __fail++;
        return;
    }

    CHECK((ret & (1 << MEMBARRIER_CMD_QUERY)) == 0,
          "QUERY does not include QUERY bit itself");

    CHECK((ret & (1 << MEMBARRIER_CMD_GLOBAL)) != 0,
          "QUERY advertises GLOBAL (bit 1)");

    CHECK((ret & (1 << MEMBARRIER_CMD_GLOBAL_EXPEDITED)) != 0,
          "QUERY advertises GLOBAL_EXPEDITED (bit 2)");

    CHECK((ret & (1 << MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED)) != 0,
          "QUERY advertises REGISTER_GLOBAL_EXPEDITED (bit 3)");

    CHECK((ret & (1 << MEMBARRIER_CMD_PRIVATE_EXPEDITED)) != 0,
          "QUERY advertises PRIVATE_EXPEDITED (bit 4)");

    CHECK((ret & (1 << MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED)) != 0,
          "QUERY advertises REGISTER_PRIVATE_EXPEDITED (bit 5)");

    CHECK(ret == EXPECTED_SUPPORTED_MASK,
          "QUERY returns exact expected bitmask (62)");

    CHECK((ret & (1UL << MEMBARRIER_CMD_PRIVATE_EXPEDITED_SYNC_CORE)) == 0,
          "QUERY does NOT advertise PRIVATE_EXPEDITED_SYNC_CORE (not implemented)");
    /* RSEQ=128 cannot be tested with bit shift on 64-bit long;
     * the return value is a 64-bit mask and RSEQ is beyond its range anyway. */
}

static void part_02_query_rejects_flags(void)
{
    CHECK_ERR(membarrier(MEMBARRIER_CMD_QUERY, 1, 0), EINVAL,
              "QUERY with flags=1 returns EINVAL");

    CHECK_ERR(membarrier(MEMBARRIER_CMD_QUERY, 0xdeadbeef, 0), EINVAL,
              "QUERY with flags=0xdeadbeef returns EINVAL");
}

static void part_03_global_basic(void)
{
    CHECK_RET(membarrier(MEMBARRIER_CMD_GLOBAL, 0, 0), 0,
              "GLOBAL with flags=0 returns 0");

    CHECK_ERR(membarrier(MEMBARRIER_CMD_GLOBAL, 1, 0), EINVAL,
              "GLOBAL with flags=1 returns EINVAL");
}

static void part_04_global_expedited(void)
{
    CHECK_RET(membarrier(MEMBARRIER_CMD_GLOBAL_EXPEDITED, 0, 0), 0,
              "GLOBAL_EXPEDITED with flags=0 returns 0");

    CHECK_ERR(membarrier(MEMBARRIER_CMD_GLOBAL_EXPEDITED, 1, 0), EINVAL,
              "GLOBAL_EXPEDITED with flags=1 returns EINVAL");
}

static void part_05_register_global_expedited(void)
{
    CHECK_RET(membarrier(MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED, 0, 0), 0,
              "REGISTER_GLOBAL_EXPEDITED with flags=0 returns 0");

    CHECK_ERR(membarrier(MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED, 1, 0), EINVAL,
              "REGISTER_GLOBAL_EXPEDITED with flags=1 returns EINVAL");
}

static void part_06_private_expedited(void)
{
    CHECK_RET(membarrier(MEMBARRIER_CMD_PRIVATE_EXPEDITED, 0, 0), 0,
              "PRIVATE_EXPEDITED with flags=0 returns 0");

    CHECK_ERR(membarrier(MEMBARRIER_CMD_PRIVATE_EXPEDITED, 1, 0), EINVAL,
              "PRIVATE_EXPEDITED with flags=1 returns EINVAL");
}

static void part_07_register_private_expedited(void)
{
    CHECK_RET(membarrier(MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED, 0, 0), 0,
              "REGISTER_PRIVATE_EXPEDITED with flags=0 returns 0");

    CHECK_ERR(membarrier(MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED, 1, 0), EINVAL,
              "REGISTER_PRIVATE_EXPEDITED with flags=1 returns EINVAL");
}

static void part_08_unsupported_commands_flag_zero(void)
{
    /* Unimplemented commands: current impl treats all non-QUERY the same
     * (compiler_fence + return 0), regardless of whether the command is known */
    int unsupported[] = {
        MEMBARRIER_CMD_PRIVATE_EXPEDITED_SYNC_CORE,
        MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED_SYNC_CORE,
        MEMBARRIER_CMD_PRIVATE_EXPEDITED_RSEQ,
        MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED_RSEQ,
        999,  /* bogus command */
    };

    for (size_t i = 0; i < sizeof(unsupported) / sizeof(unsupported[0]); i++) {
        char buf[128];
        snprintf(buf, sizeof(buf), "cmd=%d with flags=0 returns 0",
                 unsupported[i]);
        CHECK_RET(membarrier(unsupported[i], 0, 0), 0, buf);
    }
}

static void part_09_unsupported_commands_flag_nonzero(void)
{
    int unsupported[] = {
        MEMBARRIER_CMD_PRIVATE_EXPEDITED_SYNC_CORE,
        999,
    };

    for (size_t i = 0; i < sizeof(unsupported) / sizeof(unsupported[0]); i++) {
        char buf[128];
        snprintf(buf, sizeof(buf), "cmd=%d with flags=1 returns EINVAL",
                 unsupported[i]);
        CHECK_ERR(membarrier(unsupported[i], 1, 0), EINVAL, buf);
    }
}

int main(void)
{
    TEST_START("membarrier syscall");

    part_01_query();
    part_02_query_rejects_flags();
    part_03_global_basic();
    part_04_global_expedited();
    part_05_register_global_expedited();
    part_06_private_expedited();
    part_07_register_private_expedited();
    part_08_unsupported_commands_flag_zero();
    part_09_unsupported_commands_flag_nonzero();

    TEST_DONE();
    return __fail > 0 ? 1 : 0;
}
