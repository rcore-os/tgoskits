#include "test_framework.h"

#include <elf.h>
#include <errno.h>
#include <stddef.h>
#include <stdint.h>
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

    TEST_DONE();
}
