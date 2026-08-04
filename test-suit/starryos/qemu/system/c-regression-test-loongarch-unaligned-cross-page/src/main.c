#define _GNU_SOURCE

#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#if defined(__loongarch__) || defined(__loongarch64)

static sigjmp_buf fault_jump;
static volatile sig_atomic_t caught_signal;
static void *volatile caught_address;
static void *access_address;
static uint64_t store_value;
static volatile uint64_t loaded_value;

static uint64_t load_unaligned_u64(const void *address) {
    uint64_t value;
    __asm__ volatile("ld.d %0, %1, 0"
                     : "=r"(value)
                     : "r"(address)
                     : "memory");
    return value;
}

static void store_unaligned_u64(void *address, uint64_t value) {
    __asm__ volatile("st.d %0, %1, 0"
                     :
                     : "r"(value), "r"(address)
                     : "memory");
}

static void fault_handler(int signal, siginfo_t *info, void *context) {
    (void)context;
    caught_signal = signal;
    caught_address = info == NULL ? (void *)-1 : info->si_addr;
    siglongjmp(fault_jump, 1);
}

static int install_fault_handlers(void) {
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_sigaction = fault_handler;
    action.sa_flags = SA_SIGINFO | SA_NODEFER;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) != 0 ||
        sigaction(SIGBUS, &action, NULL) != 0) {
        perror("sigaction");
        return -1;
    }
    return 0;
}

static void *map_two_pages(size_t page_size) {
    void *mapping = mmap(NULL, page_size * 2, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (mapping == MAP_FAILED) {
        perror("mmap");
        return NULL;
    }
    return mapping;
}

static void trigger_load(void) {
    loaded_value = load_unaligned_u64(access_address);
}

static void trigger_store(void) {
    store_unaligned_u64(access_address, store_value);
}

static int expect_memory_fault(const char *name, void (*trigger)(void),
                               void *expected_address,
                               const unsigned char *unchanged_address,
                               const unsigned char unchanged[4]) {
    caught_signal = 0;
    caught_address = (void *)-1;
    if (sigsetjmp(fault_jump, 1) == 0) {
        trigger();
        printf("  %s: no fault\n", name);
        return 1;
    }

    int signal_ok = caught_signal == SIGSEGV;
    int address_ok = caught_address == expected_address;
    int unchanged_ok = unchanged_address == NULL ||
                       memcmp(unchanged_address, unchanged, 4) == 0;
    printf("  %s: signal=%d address=%p expected=%p unchanged=%s -> %s\n",
           name, caught_signal, (void *)caught_address, expected_address,
           unchanged_ok ? "yes" : "no",
           signal_ok && address_ok && unchanged_ok ? "OK" : "FAIL");
    return signal_ok && address_ok && unchanged_ok ? 0 : 1;
}

static int test_lazy_load(size_t page_size) {
    unsigned char *mapping = map_two_pages(page_size);
    if (mapping == NULL) {
        return 1;
    }
    unsigned char *crossing = mapping + page_size - 4;
    const unsigned char first_half[4] = {0x11, 0x22, 0x33, 0x44};
    memcpy(crossing, first_half, sizeof(first_half));

    uint64_t expected = UINT64_C(0x0000000044332211);
    uint64_t actual = load_unaligned_u64(crossing);
    int failed = actual != expected;
    printf("  lazy-load: value=%#llx expected=%#llx -> %s\n",
           (unsigned long long)actual, (unsigned long long)expected,
           failed ? "FAIL" : "OK");
    munmap(mapping, page_size * 2);
    return failed;
}

static int test_lazy_store(size_t page_size) {
    unsigned char *mapping = map_two_pages(page_size);
    if (mapping == NULL) {
        return 1;
    }
    unsigned char *crossing = mapping + page_size - 4;
    memset(crossing, 0xa5, 4);

    uint64_t expected = UINT64_C(0x8877665544332211);
    store_unaligned_u64(crossing, expected);
    uint64_t actual;
    memcpy(&actual, crossing, sizeof(actual));
    int failed = actual != expected;
    printf("  lazy-store: value=%#llx expected=%#llx -> %s\n",
           (unsigned long long)actual, (unsigned long long)expected,
           failed ? "FAIL" : "OK");
    munmap(mapping, page_size * 2);
    return failed;
}

static int test_read_only_page(size_t page_size) {
    unsigned char *mapping = map_two_pages(page_size);
    if (mapping == NULL) {
        return 1;
    }
    unsigned char *crossing = mapping + page_size - 4;
    const uint64_t original = UINT64_C(0xa7a6a5a4a3a2a1a0);
    memcpy(crossing, &original, sizeof(original));
    if (mprotect(mapping + page_size, page_size, PROT_READ) != 0) {
        perror("mprotect");
        munmap(mapping, page_size * 2);
        return 1;
    }

    uint64_t actual = load_unaligned_u64(crossing);
    int failed = actual != original;
    printf("  read-only-load: value=%#llx expected=%#llx -> %s\n",
           (unsigned long long)actual, (unsigned long long)original,
           failed ? "FAIL" : "OK");

    unsigned char first_half[4];
    memcpy(first_half, crossing, sizeof(first_half));
    access_address = crossing;
    store_value = UINT64_C(0x8877665544332211);
    failed += expect_memory_fault("read-only-store", trigger_store,
                                  mapping + page_size, crossing, first_half);
    munmap(mapping, page_size * 2);
    return failed;
}

static int test_unmapped_page(size_t page_size) {
    unsigned char *mapping = map_two_pages(page_size);
    if (mapping == NULL) {
        return 1;
    }
    unsigned char *crossing = mapping + page_size - 4;
    const unsigned char first_half[4] = {0xa0, 0xa1, 0xa2, 0xa3};
    memcpy(crossing, first_half, sizeof(first_half));
    if (munmap(mapping + page_size, page_size) != 0) {
        perror("munmap");
        munmap(mapping, page_size);
        return 1;
    }

    access_address = crossing;
    int failed = expect_memory_fault("unmapped-load", trigger_load,
                                     mapping + page_size, NULL, first_half);
    store_value = UINT64_C(0x8877665544332211);
    failed += expect_memory_fault("unmapped-store", trigger_store,
                                  mapping + page_size, crossing, first_half);
    munmap(mapping, page_size);
    return failed;
}

int main(void) {
    if (install_fault_handlers() != 0) {
        return 2;
    }
    long page_size = sysconf(_SC_PAGESIZE);
    if (page_size <= 8) {
        fprintf(stderr, "invalid page size: %ld\n", page_size);
        return 2;
    }

    printf("LoongArch cross-page unaligned access regression\n");
    int failures = 0;
    failures += test_lazy_load((size_t)page_size);
    failures += test_lazy_store((size_t)page_size);
    failures += test_read_only_page((size_t)page_size);
    failures += test_unmapped_page((size_t)page_size);

    if (failures == 0) {
        printf("LOONGARCH_UNALIGNED_CROSS_PAGE_OK\n");
        return 0;
    }
    printf("LOONGARCH_UNALIGNED_CROSS_PAGE_FAIL failures=%d\n", failures);
    return 1;
}

#else

int main(void) {
    printf("LoongArch cross-page unaligned access regression: SKIP non-LoongArch\n");
    return 0;
}

#endif
