#define _GNU_SOURCE

#include "test_framework.h"
#include <errno.h>
#include <stdbool.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

int __pass = 0;
int __fail = 0;
int __skip = 0;
int __observe = 0;

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

static bool query_advertises(int cmd)
{
    long ret = membarrier(MEMBARRIER_CMD_QUERY, 0, 0);
    if (ret < 0)
        return false;
    if (cmd < 0 || cmd >= (int)(8 * sizeof(long)))
        return false;
    return (ret & (1L << cmd)) != 0;
}

static void part_01_query(void)
{
    long ret = membarrier(MEMBARRIER_CMD_QUERY, 0, 0);
    CHECK(ret >= 0, "QUERY returns non-negative supported command mask");
    if (ret < 0)
        return;

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

static void part_02_flags_are_rejected(void)
{
    int commands[] = {
        MEMBARRIER_CMD_QUERY,
        MEMBARRIER_CMD_GLOBAL,
        MEMBARRIER_CMD_GLOBAL_EXPEDITED,
        MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED,
        MEMBARRIER_CMD_PRIVATE_EXPEDITED,
        MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED,
        999,
    };

    for (size_t i = 0; i < sizeof(commands) / sizeof(commands[0]); i++) {
        char buf[128];
        snprintf(buf, sizeof(buf), "cmd=%d rejects flags=1", commands[i]);
        CHECK_ERR(membarrier(commands[i], 1, 0), EINVAL, buf);
    }

    CHECK_ERR(membarrier(MEMBARRIER_CMD_QUERY, 1, 0), EINVAL,
              "QUERY with flags=1 returns EINVAL");

    CHECK_ERR(membarrier(MEMBARRIER_CMD_QUERY, 0xdeadbeef, 0), EINVAL,
              "QUERY with flags=0xdeadbeef returns EINVAL");
}

static void part_03_global_commands(void)
{
    CHECK_RET(membarrier(MEMBARRIER_CMD_GLOBAL, 0, 0), 0,
              "GLOBAL with flags=0 returns 0");
    CHECK_RET(membarrier(MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED, 0, 0), 0,
              "REGISTER_GLOBAL_EXPEDITED succeeds");
    CHECK_RET(membarrier(MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED, 0, 0), 0,
              "REGISTER_GLOBAL_EXPEDITED is idempotent");
    CHECK_RET(membarrier(MEMBARRIER_CMD_GLOBAL_EXPEDITED, 0, 0), 0,
              "GLOBAL_EXPEDITED succeeds after registration");
}

static void part_04_private_expedited_registration(void)
{
    CHECK_ERR(membarrier(MEMBARRIER_CMD_PRIVATE_EXPEDITED, 0, 0), EPERM,
              "PRIVATE_EXPEDITED before registration returns EPERM");
    CHECK_RET(membarrier(MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED, 0, 0), 0,
              "REGISTER_PRIVATE_EXPEDITED succeeds");
    CHECK_RET(membarrier(MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED, 0, 0), 0,
              "REGISTER_PRIVATE_EXPEDITED is idempotent");
    CHECK_RET(membarrier(MEMBARRIER_CMD_PRIVATE_EXPEDITED, 0, 0), 0,
              "PRIVATE_EXPEDITED succeeds after registration");
}

static void part_05_unsupported_commands(void)
{
    int unsupported[] = {
        MEMBARRIER_CMD_PRIVATE_EXPEDITED_SYNC_CORE,
        MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED_SYNC_CORE,
        MEMBARRIER_CMD_PRIVATE_EXPEDITED_RSEQ,
        MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED_RSEQ,
        999,  /* bogus command */
    };

    for (size_t i = 0; i < sizeof(unsupported) / sizeof(unsupported[0]); i++) {
        char buf[128];
        snprintf(buf, sizeof(buf), "cmd=%d is not advertised by QUERY", unsupported[i]);
        CHECK(!query_advertises(unsupported[i]), buf);

        snprintf(buf, sizeof(buf), "cmd=%d with flags=0 returns EINVAL", unsupported[i]);
        CHECK_ERR(membarrier(unsupported[i], 0, 0), EINVAL, buf);
    }
}

static void part_06_cpu_id_is_ignored_without_cmd_flag(void)
{
    CHECK_RET(membarrier(MEMBARRIER_CMD_GLOBAL, 0, 1234), 0,
              "cpu_id is ignored when flags is zero for GLOBAL");
    CHECK_RET(membarrier(MEMBARRIER_CMD_PRIVATE_EXPEDITED, 0, 1234), 0,
              "cpu_id is ignored when flags is zero for PRIVATE_EXPEDITED");
}

int main(void)
{
    TEST_START("membarrier syscall");

    part_01_query();
    part_02_flags_are_rejected();
    part_03_global_commands();
    part_04_private_expedited_registration();
    part_05_unsupported_commands();
    part_06_cpu_id_is_ignored_without_cmd_flag();

    TEST_DONE();
}
