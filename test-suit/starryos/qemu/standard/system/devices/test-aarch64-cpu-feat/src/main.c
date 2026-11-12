#define _GNU_SOURCE

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
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#if defined(__aarch64__)
static int unaligned_ldp_after_mremap(void)
{
    long page_size = sysconf(_SC_PAGESIZE);
    if (page_size <= 0) {
        return 0;
    }

    uint8_t *source = mmap(NULL, (size_t)page_size, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    uint8_t *target = mmap(NULL, (size_t)page_size, PROT_NONE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (source == MAP_FAILED || target == MAP_FAILED) {
        if (source != MAP_FAILED) {
            munmap(source, (size_t)page_size);
        }
        if (target != MAP_FAILED) {
            munmap(target, (size_t)page_size);
        }
        return 0;
    }

    for (size_t index = 0; index < 64; ++index) {
        source[index] = (uint8_t)index;
    }
    void *moved = (void *)syscall(SYS_mremap, source, (size_t)page_size,
                                  (size_t)page_size,
                                  MREMAP_MAYMOVE | MREMAP_FIXED, target);
    if (moved == MAP_FAILED) {
        munmap(source, (size_t)page_size);
        munmap(target, (size_t)page_size);
        return 0;
    }
    if (moved != target) {
        munmap(moved, (size_t)page_size);
        munmap(target, (size_t)page_size);
        return 0;
    }

    _Alignas(16) uint8_t actual[32];
    const uint8_t *unaligned_end = (const uint8_t *)moved + 65;
    __asm__ volatile(
        "ldp q0, q1, [%1, #-64]\n\t"
        "stp q0, q1, [%0]"
        :
        : "r"(actual), "r"(unaligned_end)
        : "v0", "v1", "memory");

    int matches = 1;
    for (size_t index = 0; index < sizeof(actual); ++index) {
        if (actual[index] != (uint8_t)(index + 1)) {
            matches = 0;
            break;
        }
    }
    if (munmap(moved, (size_t)page_size) != 0) {
        matches = 0;
    }
    return matches;
}
#endif

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

    /* Linux-compatible EL0 permits ordinary unaligned accesses to Normal
     * memory. Some firmware leaves SCTLR_EL1.A set, so this must be configured
     * explicitly instead of inherited at boot. This checks observable EL0
     * behavior only: it does not force A=1 before kernel entry, so a pass with
     * QEMU's default firmware cannot prove that the kernel cleared A. */
    _Alignas(uint64_t) const uint8_t bytes[] = {
        0xa5, 0x78, 0x56, 0x34, 0x12, 0x00, 0x00, 0x00, 0x00
    };
    const uint8_t *unaligned = bytes + 1;
    uint64_t unaligned_value = 0;
    __asm__ volatile("ldr %0, [%1]"
                     : "=r"(unaligned_value)
                     : "r"(unaligned)
                     : "memory");
    CHECK(unaligned_value == UINT64_C(0x12345678),
          "ordinary unaligned LDR returned without an alignment fault");

    /* mremap relocates populated PTEs using their queried flags. The relocated
     * page must remain Normal memory so glibc-style vector copies stay legal. */
    CHECK(unaligned_ldp_after_mremap(),
          "unaligned LDP Q pair works after MREMAP_FIXED relocation");

    /* DC ZVA 块大小由 DCZID_EL0.BS 给出, 单位是 4 字节字。
     * DCZID_EL0.DZP 置位时, 用户态不应执行 DC ZVA。 */
    uint64_t dczid = 0;
    __asm__ volatile("mrs %0, dczid_el0" : "=r"(dczid));
    CHECK((dczid & (1u << 4)) == 0, "DCZID_EL0 permits DC ZVA at EL0");
    if ((dczid & (1u << 4)) != 0) {
        TEST_DONE();
    }

    unsigned dczid_log2_words = (unsigned)(dczid & 0xf);
    size_t dczid_bytes = (size_t)4u << dczid_log2_words;
    CHECK(dczid_bytes >= 16 && dczid_bytes <= 2048,
          "DCZID_EL0 reports a supported DC ZVA block size");
    if (dczid_bytes < 16 || dczid_bytes > 2048) {
        TEST_DONE();
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
