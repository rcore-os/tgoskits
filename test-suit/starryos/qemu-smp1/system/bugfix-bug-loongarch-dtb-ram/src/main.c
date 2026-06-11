#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/utsname.h>

/*
 * loongarch64 qemu-virt 物理内存(RAM)探测回归测试。
 *
 * 触发背景 (为什么写这个测例):
 *   platforms/ax-plat-loongarch64-qemu-virt 此前把物理内存大小硬编码为常量
 *   PHYS_MEMORY_SIZE (512 MiB),无视 QEMU 启动时 `-m` 指定的实际内存。于是无论
 *   `-m 2G` 还是 `-m 512M`,guest 内核都只认 512 MiB,导致较大内存的应用
 *   (JVM / 数据库 / 重型 server) 在 loong 上 OOM。
 *
 * 修复 (platforms/ax-plat-loongarch64-qemu-virt/src/{boot.rs,mem.rs}):
 *   _start 把 QEMU 传入的 FDT 指针 ($a0..$a3) 存下,MMU 建立后用 fdt-raw 解析
 *   设备树 /memory 节点,按真实 RAM 大小初始化分配器;解析失败时回退到 512 MiB
 *   常量,保证 boot 永不回归。
 *
 * 本测例 (smp1/system 组,loong qemu-loongarch64.toml 已设 `-m 2G`):
 *   - 读 /proc/meminfo 的 MemTotal。
 *   - loongarch64: 断言 MemTotal > 1 GiB —— 只有 DTB 探测生效(honor `-m 2G`)才可能
 *     > 1 GiB;旧硬编码 512 MiB -> < 1 GiB -> FAIL。即:修复前 FAIL,修复后 PASS。
 *   - 其它架构 (x86_64/aarch64/riscv64,各自已正确探测 RAM,不在本测例范围内):
 *     仅做 sanity (MemTotal > 256 MiB),确保本测例随全组运行时不误伤。
 *
 * /proc/meminfo 格式 (man 5 proc): "MemTotal:       <kB> kB"。
 */

/* 读 /proc/meminfo 中 MemTotal 的 kB 值;成功返回 0 并写入 *out_kb。 */
static int read_memtotal_kb(unsigned long *out_kb)
{
    FILE *f = fopen("/proc/meminfo", "r");
    if (!f)
        return -1;
    char line[256];
    int rc = -1;
    while (fgets(line, sizeof line, f)) {
        if (strncmp(line, "MemTotal:", 9) == 0) {
            if (sscanf(line + 9, "%lu", out_kb) == 1)
                rc = 0;
            break;
        }
    }
    fclose(f);
    return rc;
}

int main(void)
{
    TEST_START("loongarch64 DTB RAM detection");

    unsigned long memtotal_kb = 0;
    int rc = read_memtotal_kb(&memtotal_kb);
    CHECK(rc == 0, "/proc/meminfo MemTotal 可读");

    struct utsname uts;
    memset(&uts, 0, sizeof uts);
    int urc = uname(&uts);
    CHECK(urc == 0, "uname() 成功");

    char msg[192];
    if (urc == 0 && strcmp(uts.machine, "loongarch64") == 0) {
        /* DTB 探测须 honor `-m 2G`;旧硬编码 512 MiB 会卡在 ~0.5 GiB。阈值 1 GiB 留足余量。 */
        snprintf(msg, sizeof msg,
                 "loongarch64: MemTotal=%lu kB > 1 GiB (DTB 探测 honor -m,非硬编码 512MiB)",
                 memtotal_kb);
        CHECK(memtotal_kb > 1024UL * 1024UL, msg);
    } else {
        /* 其它架构本就正确探测 RAM;仅 sanity,避免随全组运行时误伤。 */
        snprintf(msg, sizeof msg, "%s: MemTotal=%lu kB > 256 MiB (RAM 探测合理)",
                 uts.machine[0] ? uts.machine : "(unknown)", memtotal_kb);
        CHECK(memtotal_kb > 256UL * 1024UL, msg);
    }

    TEST_DONE();
}
