#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <stdint.h>

static int __pass = 0;
static int __fail = 0;

#define CHECK(cond, msg) do {                                           \
    if (cond) {                                                         \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);       \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | errno=%d (%s)\n",                 \
               __FILE__, __LINE__, msg, errno, strerror(errno));        \
        __fail++;                                                       \
    }                                                                   \
} while(0)

#define MODULE_START(name) printf("\n--- MODULE: %s ---\n", name)
#define SUMMARY()                                                       \
    printf("\n=== SUMMARY: %d passed, %d failed ===\n", __pass, __fail); \
    return __fail > 0 ? 1 : 0

static long raw_init_module(void *data, unsigned long len, const char *args) {
#ifdef SYS_init_module
    return syscall(SYS_init_module, data, len, args);
#else
    errno = ENOSYS; return -1;
#endif
}

static long raw_finit_module(int fd, const char *args, unsigned int flags) {
#ifdef SYS_finit_module
    return syscall(SYS_finit_module, fd, args, flags);
#else
    errno = ENOSYS; return -1;
#endif
}

static long raw_delete_module(const char *name, unsigned int flags) {
#ifdef SYS_delete_module
    return syscall(SYS_delete_module, name, flags);
#else
    errno = ENOSYS; return -1;
#endif
}

int main(void) {
    printf("=== kmod Loader Test Suite ===\n");

    MODULE_START("init_module_null");
    CHECK(raw_init_module(NULL, 0, "") < 0, "init_module(NULL,0) returns error");

    MODULE_START("init_module_invalid_elf");
    uint8_t bad[64]; memset(bad, 0xFF, sizeof(bad));
    CHECK(raw_init_module(bad, sizeof(bad), "") < 0, "init_module(non-ELF) returns error");

    MODULE_START("init_module_truncated_elf");
    uint8_t hdr[16] = {0x7f,'E','L','F', 2,1,1, 0,0,0,0,0,0,0,0,0};
    CHECK(raw_init_module(hdr, sizeof(hdr), "") < 0, "init_module(truncated) returns error");

    MODULE_START("finit_module_bad_fd");
    CHECK(raw_finit_module(-1, "", 0) < 0, "finit_module(-1) returns error");

    MODULE_START("delete_module_nonexistent");
    CHECK(raw_delete_module("no_such_module", 0) < 0, "delete_module(nonexistent) returns error");

    MODULE_START("init_module_zero_len");
    uint8_t dummy = 0;
    CHECK(raw_init_module(&dummy, 0, "") < 0, "init_module(,0,) returns error");

    MODULE_START("init_module_with_args");
    uint8_t buf[64]; memset(buf, 0, sizeof(buf));
    CHECK(raw_init_module(buf, sizeof(buf), "key=val") < 0,
          "init_module(invalid ELF with args) returns error, no crash");

    MODULE_START("init_module_valid_et_rel_header");
    uint8_t et_rel[64] = {
        0x7f,'E','L','F', 2,1,1,0, 0,0,0,0,0,0,0,0,
        1,0, 0x3e,0, 1,0,0,0,
        0,0,0,0,0,0,0,0,
        0,0,0,0,0,0,0,0,
        0,0,0,0, 64,0, 0,0, 0,0, 64,0, 0,0, 0,0
    };
    CHECK(raw_init_module(et_rel, sizeof(et_rel), "") < 0,
          "init_module(valid ET_REL header, no sections) returns error");

    MODULE_START("finit_module_fd_zero");
    CHECK(raw_finit_module(0, "", 0) < 0,
          "finit_module(fd=0 stdin) returns error");

    MODULE_START("init_module_large_junk");
    uint8_t big[256]; memset(big, 0xAA, sizeof(big));
    CHECK(raw_init_module(big, sizeof(big), "") < 0,
          "init_module(256-byte junk) returns error, no crash");

    SUMMARY();
}
