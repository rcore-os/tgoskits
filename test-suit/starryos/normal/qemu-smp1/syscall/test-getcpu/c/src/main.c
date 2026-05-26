/*
 * test_getcpu.c -- getcpu(2) 系统调用测试
 *
 * 验证 sys_getcpu(cpu, node, tcache):
 *   1. getcpu(&cpu, &node, NULL) 返回 0；cpu ∈ [0, nproc)；node == 0(单 NUMA 节点)
 *   2. NULL cpu 指针安全(内核仍写回 node)
 *   3. NULL node 指针安全(内核仍写回 cpu)
 *   4. 三参全 NULL 安全(返回 0,不解引用)
 *   5. 已废弃的 tcache 参数被忽略(传任意值仍成功)
 */
#define _GNU_SOURCE
#include "test_framework.h"
#include <unistd.h>
#include <sys/syscall.h>

int main(void)
{
    TEST_START("getcpu");

    long nproc = sysconf(_SC_NPROCESSORS_ONLN);
    if (nproc < 1) {
        nproc = 1;
    }

    /* getcpu(&cpu, &node, NULL): 成功, cpu 范围有效, node == 0 */
    {
        unsigned cpu = 0xdeadu;
        unsigned node = 0xbeefu;
        CHECK_RET(syscall(SYS_getcpu, &cpu, &node, NULL), 0,
                  "getcpu(&cpu,&node,NULL) 成功");
        CHECK(cpu < (unsigned)nproc, "cpu id 落在 [0, nproc) 范围内");
        CHECK(node == 0u, "node == 0 (单 NUMA 节点)");
    }

    /* NULL cpu 指针安全, 内核仍写回 node */
    {
        unsigned node = 0xbeefu;
        CHECK_RET(syscall(SYS_getcpu, NULL, &node, NULL), 0,
                  "getcpu(NULL,&node,NULL) 成功(NULL cpu 安全)");
        CHECK(node == 0u, "NULL cpu 时 node 仍被写为 0");
    }

    /* NULL node 指针安全, 内核仍写回 cpu */
    {
        unsigned cpu = 0xdeadu;
        CHECK_RET(syscall(SYS_getcpu, &cpu, NULL, NULL), 0,
                  "getcpu(&cpu,NULL,NULL) 成功(NULL node 安全)");
        CHECK(cpu < (unsigned)nproc, "NULL node 时 cpu 范围仍有效");
    }

    /* 三参全 NULL 安全(不解引用空指针) */
    CHECK_RET(syscall(SYS_getcpu, NULL, NULL, NULL), 0,
              "getcpu(NULL,NULL,NULL) 成功(全 NULL 安全)");

    /* 已废弃的 tcache(第三参)应被忽略 */
    {
        unsigned cpu = 0xdeadu;
        unsigned node = 0xbeefu;
        CHECK_RET(syscall(SYS_getcpu, &cpu, &node, (unsigned long)0xdeadbeef), 0,
                  "getcpu 忽略已废弃的 tcache 参数");
    }

    TEST_DONE();
}
