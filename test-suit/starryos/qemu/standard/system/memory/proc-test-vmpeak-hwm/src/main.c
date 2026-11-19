#define _GNU_SOURCE
#include "test_framework.h"
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef MREMAP_MAYMOVE
#define MREMAP_MAYMOVE 1
#endif

static long read_status_kb(const char *key)
{
    FILE *fp = fopen("/proc/self/status", "r");
    CHECK(fp != NULL, "open /proc/self/status");

    char line[256];
    char prefix[64];
    long value = -1;
    int found = 0;

    snprintf(prefix, sizeof(prefix), "%s:\t", key);
    while (fgets(line, sizeof(line), fp) != NULL) {
        if (strncmp(line, prefix, strlen(prefix)) != 0) {
            continue;
        }
        CHECK(sscanf(line + strlen(prefix), "%ld kB", &value) == 1, key);
        found = 1;
        break;
    }

    fclose(fp);
    CHECK(found, "missing Vm field");
    return value;
}

static void touch_pages(unsigned char *ptr, size_t len, size_t page_size)
{
    for (size_t off = 0; off < len; off += page_size) {
        ptr[off] = (unsigned char)(off / page_size);
    }
}

static void test_mmap_highwater(size_t page_size)
{
    const size_t len = 32 * 1024 * 1024;
    unsigned long before_peak = (unsigned long)read_status_kb("VmPeak");
    unsigned long before_hwm = (unsigned long)read_status_kb("VmHWM");
    unsigned char *map = mmap(NULL, len, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(map != MAP_FAILED, "mmap anonymous region");
    if (map == MAP_FAILED) {
        return;
    }

    touch_pages(map, len, page_size);

    unsigned long after_peak = (unsigned long)read_status_kb("VmPeak");
    unsigned long after_hwm = (unsigned long)read_status_kb("VmHWM");
    CHECK(after_peak > before_peak, "VmPeak grows after mmap");
    CHECK(after_hwm > before_hwm, "VmHWM grows after mmap");

    CHECK(munmap(map, len) == 0, "munmap mmap region");
    unsigned long post_peak = (unsigned long)read_status_kb("VmPeak");
    unsigned long post_hwm = (unsigned long)read_status_kb("VmHWM");
    CHECK(post_peak == after_peak, "VmPeak does not fall after munmap");
    CHECK(post_hwm == after_hwm, "VmHWM does not fall after munmap");
}

static void test_brk_highwater(size_t page_size)
{
    unsigned long before_peak = (unsigned long)read_status_kb("VmPeak");
    unsigned long before_hwm = (unsigned long)read_status_kb("VmHWM");
    unsigned long base = (unsigned long)syscall(SYS_brk, 0);
    unsigned long target = base + 16 * (unsigned long)page_size;
    long ret = syscall(SYS_brk, (void *)target);
    CHECK(ret == (long)target, "raw brk expands heap");
    if (ret != (long)target) {
        return;
    }

    touch_pages((unsigned char *)base, target - base, page_size);

    unsigned long after_peak = (unsigned long)read_status_kb("VmPeak");
    unsigned long after_hwm = (unsigned long)read_status_kb("VmHWM");
    CHECK(after_peak >= before_peak, "VmPeak does not fall after brk");
    CHECK(after_hwm >= before_hwm, "VmHWM does not fall after brk");

    CHECK(syscall(SYS_brk, (void *)base) == (long)base,
          "raw brk restores original break");
}

static void test_mremap_highwater(size_t page_size)
{
    const size_t from_len = page_size;
    const size_t to_len = 16 * 1024 * 1024;
    unsigned long before_peak = (unsigned long)read_status_kb("VmPeak");
    unsigned long before_hwm = (unsigned long)read_status_kb("VmHWM");
    unsigned char *map = mmap(NULL, from_len, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(map != MAP_FAILED, "mmap base page for mremap");
    if (map == MAP_FAILED) {
        return;
    }

    touch_pages(map, from_len, page_size);

    unsigned char *grown = mremap(map, from_len, to_len, MREMAP_MAYMOVE);
    CHECK(grown != MAP_FAILED, "mremap grows mapping");
    if (grown == MAP_FAILED) {
        munmap(map, from_len);
        return;
    }

    touch_pages(grown, to_len, page_size);

    unsigned long after_peak = (unsigned long)read_status_kb("VmPeak");
    unsigned long after_hwm = (unsigned long)read_status_kb("VmHWM");
    CHECK(after_peak > before_peak, "VmPeak grows after mremap");
    CHECK(after_hwm > before_hwm, "VmHWM grows after mremap");

    CHECK(munmap(grown, to_len) == 0, "munmap mremap region");
    unsigned long post_peak = (unsigned long)read_status_kb("VmPeak");
    unsigned long post_hwm = (unsigned long)read_status_kb("VmHWM");
    CHECK(post_peak == after_peak, "VmPeak does not fall after mremap munmap");
    CHECK(post_hwm == after_hwm, "VmHWM does not fall after mremap munmap");
}

int main(void)
{
    long page_size = sysconf(_SC_PAGESIZE);
    CHECK(page_size > 0, "sysconf(_SC_PAGESIZE) must be positive");

    TEST_START("vmpeak-hwm");
    CHECK(read_status_kb("VmPeak") > 0, "VmPeak must be positive");
    CHECK(read_status_kb("VmHWM") > 0, "VmHWM must be positive");

    test_mremap_highwater((size_t)page_size);
    test_mmap_highwater((size_t)page_size);
    test_brk_highwater((size_t)page_size);

    TEST_DONE();
}
