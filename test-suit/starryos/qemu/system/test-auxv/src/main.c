#include "test_framework.h"

#include <elf.h>
#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/auxv.h>
#include <unistd.h>

#ifndef AT_SECURE
#define AT_SECURE 23
#endif

#ifndef AT_UID
#define AT_UID 11
#endif

#ifndef AT_EUID
#define AT_EUID 12
#endif

#ifndef AT_GID
#define AT_GID 13
#endif

#ifndef AT_EGID
#define AT_EGID 14
#endif

#ifndef AT_RANDOM
#define AT_RANDOM 25
#endif

#ifndef AT_CLKTCK
#define AT_CLKTCK 17
#endif

#ifndef AT_FLAGS
#define AT_FLAGS 8
#endif

extern char **environ;

static const Elf64_auxv_t *initial_auxv(void)
{
    char **envp = environ;
    while (*envp != NULL) {
        envp++;
    }
    return (const Elf64_auxv_t *)(envp + 1);
}

static int find_auxv_value(unsigned long key, unsigned long *value)
{
    const Elf64_auxv_t *auxv = initial_auxv();

    for (size_t i = 0; i < 128; i++) {
        if (auxv[i].a_type == AT_NULL) {
            return 0;
        }
        if (auxv[i].a_type == key) {
            *value = auxv[i].a_un.a_val;
            return 1;
        }
    }

    return -1;
}

static void check_auxv_terminator(void)
{
    const Elf64_auxv_t *auxv = initial_auxv();
    int found = 0;

    for (size_t i = 0; i < 128; i++) {
        if (auxv[i].a_type == AT_NULL) {
            found = 1;
            break;
        }
    }

    CHECK(found, "initial auxv contains AT_NULL terminator within 128 entries");
}

static void check_getauxval_entry(unsigned long key, unsigned long expected,
                                  const char *msg)
{
    errno = 0;
    unsigned long value = getauxval(key);
    CHECK(errno == 0, "getauxval reports existing auxv entry");
    CHECK(value == expected, msg);
}

int main(void)
{
    TEST_START("ELF auxiliary vector process ABI");

    check_auxv_terminator();

    unsigned long secure = 1;
    int secure_found = find_auxv_value(AT_SECURE, &secure);
    CHECK(secure_found == 1, "AT_SECURE is present in initial auxv");
    if (secure_found == 1) {
        CHECK(secure == 0, "normal non-setuid exec has AT_SECURE=0");
        check_getauxval_entry(AT_SECURE, 0, "getauxval(AT_SECURE) == 0");
    }

    unsigned long uid = 0;
    unsigned long euid = 0;
    unsigned long gid = 0;
    unsigned long egid = 0;
    CHECK(find_auxv_value(AT_UID, &uid) == 1, "AT_UID is present");
    CHECK(find_auxv_value(AT_EUID, &euid) == 1, "AT_EUID is present");
    CHECK(find_auxv_value(AT_GID, &gid) == 1, "AT_GID is present");
    CHECK(find_auxv_value(AT_EGID, &egid) == 1, "AT_EGID is present");

    CHECK(uid == (unsigned long)getuid(), "AT_UID matches getuid()");
    CHECK(euid == (unsigned long)geteuid(), "AT_EUID matches geteuid()");
    CHECK(gid == (unsigned long)getgid(), "AT_GID matches getgid()");
    CHECK(egid == (unsigned long)getegid(), "AT_EGID matches getegid()");

    check_getauxval_entry(AT_UID, (unsigned long)getuid(), "getauxval(AT_UID) matches getuid()");
    check_getauxval_entry(AT_EUID, (unsigned long)geteuid(), "getauxval(AT_EUID) matches geteuid()");
    check_getauxval_entry(AT_GID, (unsigned long)getgid(), "getauxval(AT_GID) matches getgid()");
    check_getauxval_entry(AT_EGID, (unsigned long)getegid(), "getauxval(AT_EGID) matches getegid()");

    /* AT_CLKTCK: Linux fills CLOCKS_PER_SEC (USER_HZ = 100). glibc/musl read it
     * for sysconf(_SC_CLK_TCK) and times(2) accounting; absence makes them fall
     * back to a wrong default. */
    unsigned long clktck = 0;
    CHECK(find_auxv_value(AT_CLKTCK, &clktck) == 1, "AT_CLKTCK is present");
    CHECK(clktck == 100, "AT_CLKTCK == 100 (USER_HZ)");
    check_getauxval_entry(AT_CLKTCK, 100, "getauxval(AT_CLKTCK) == 100");
    CHECK(sysconf(_SC_CLK_TCK) == 100, "sysconf(_SC_CLK_TCK) == 100");

    /* AT_FLAGS: 0 for a normal (non-MMAP_PAGE_ZERO) load, like Linux. */
    unsigned long at_flags = 1;
    CHECK(find_auxv_value(AT_FLAGS, &at_flags) == 1, "AT_FLAGS is present");
    CHECK(at_flags == 0, "AT_FLAGS == 0 for a normal load");

    /* AT_RANDOM: 16 CSPRNG bytes that seed the userspace stack canary and
     * pointer guard. A fixed constant makes the canary predictable and defeats
     * SSP/PIE hardening, so the bytes must not be the old placeholder or zero. */
    unsigned long rand_ptr = getauxval(AT_RANDOM);
    CHECK(rand_ptr != 0, "AT_RANDOM is present and non-NULL");
    if (rand_ptr != 0) {
        const unsigned char *rb = (const unsigned char *)rand_ptr;
        CHECK(memcmp(rb, "0123456789abcdef", 16) != 0,
              "AT_RANDOM is not the fixed placeholder string");
        int all_zero = 1;
        for (int i = 0; i < 16; i++) {
            if (rb[i] != 0) {
                all_zero = 0;
                break;
            }
        }
        CHECK(!all_zero, "AT_RANDOM bytes are not all zero");
    }

    TEST_DONE();
}
