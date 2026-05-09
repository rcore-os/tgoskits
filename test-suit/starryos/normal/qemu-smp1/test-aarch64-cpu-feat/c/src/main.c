/*
 * test-aarch64-cpu-feat — 验证 EL0 可访问 CTR_EL0 / DC ZVA / IC IVAU
 *
 * 在 aarch64 上, SCTLR_EL1 的 UCT (bit 15)、DZE (bit 14)、UCI (bit 26)
 * 控制 EL0 是否可以执行 MRS CTR_EL0、DC ZVA、DC CVAU/IC IVAU 指令。
 * 任何位未置位时, EL0 执行对应指令会触发 EC=0x18 同步异常,
 * 内核交付 SIGTRAP/SIGILL 杀掉进程。
 *
 * musl 和 glibc 启动时都会先读 CTR_EL0 拿 cache line 大小, 这之前
 * 进程就会死掉, 所以根本到不了 main()。本用例反过来: 既然能进 main(),
 * 直接在 main() 里再次执行三条指令, 没拿到信号就算通过。
 *
 * 非 aarch64 架构原样跳过。
 */

#include "test_framework.h"
#include <stdint.h>

int main(void)
{
    TEST_START("aarch64-cpu-feat");

#if !defined(__aarch64__)
    printf("  SKIP | non-aarch64 target\n");
    TEST_DONE();
#else
    /* 1. MRS CTR_EL0: 读 cache 拓扑寄存器, UCT=1 才允许。 */
    uint64_t ctr = 0;
    __asm__ volatile("mrs %0, ctr_el0" : "=r"(ctr));
    CHECK(ctr != 0, "MRS CTR_EL0 returned a non-zero value");

    /* DC ZVA 块大小: CTR_EL0[3:0] 的 4-bit 字段, 单位是 4 字节字。
     * QEMU 上常见 64 字节, 但保险起见按字段算。 */
    unsigned dczid_log2_words = (unsigned)(ctr & 0xf);
    size_t dczid_bytes = (size_t)4u << dczid_log2_words;
    if (dczid_bytes < 16 || dczid_bytes > 2048) {
        dczid_bytes = 64; /* 兜底, 不让奇怪数值打挂下一步 */
    }

    /* 2. DC ZVA: 把一段对齐到 dczid_bytes 的缓冲区清零, DZE=1 才允许。 */
    /* 多分配一个块大小再向上对齐, 避免栈对齐误差。 */
    unsigned char raw[4096 + 2048];
    uintptr_t aligned = ((uintptr_t)raw + dczid_bytes - 1) & ~(uintptr_t)(dczid_bytes - 1);
    /* 先写入非零数据, 让 DC ZVA 的清零效果可观察。 */
    for (size_t i = 0; i < dczid_bytes; i++) {
        ((unsigned char *)aligned)[i] = 0xa5;
    }
    __asm__ volatile("dc zva, %0" : : "r"(aligned) : "memory");
    int all_zero = 1;
    for (size_t i = 0; i < dczid_bytes; i++) {
        if (((unsigned char *)aligned)[i] != 0) {
            all_zero = 0;
            break;
        }
    }
    CHECK(all_zero, "DC ZVA cleared the aligned cache line");

    /* 3. IC IVAU: 失效该地址在 PoU 的 I-cache 行, UCI=1 才允许。
     *  发完一个 ISB 走完同步, 没异常就算成功。 */
    __asm__ volatile(
        "ic ivau, %0\n\t"
        "dsb ish\n\t"
        "isb\n\t"
        : : "r"(aligned) : "memory");
    CHECK(1, "IC IVAU + DSB ISH + ISB returned without trap");

    TEST_DONE();
#endif
}
