#include "test_framework.h"

#include <elf.h>
#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/wait.h>
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

#ifndef AT_FLAGS
#define AT_FLAGS 8
#endif

#ifndef AT_CLKTCK
#define AT_CLKTCK 17
#endif

#ifndef AT_RANDOM
#define AT_RANDOM 25
#endif

static const char PRINT_AT_RANDOM[] = "--print-at-random";

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

/* Executes this binary again and collects the AT_RANDOM bytes it received. */
static int child_at_random(const char *self, unsigned char *out)
{
    int fds[2];
    if (pipe(fds) != 0) {
        return 0;
    }

    pid_t pid = fork();
    if (pid == 0) {
        close(fds[0]);
        dup2(fds[1], STDOUT_FILENO);
        execl(self, self, PRINT_AT_RANDOM, (char *)NULL);
        _exit(127);
    }
    close(fds[1]);

    size_t got = 0;
    while (pid > 0 && got < 16) {
        ssize_t n = read(fds[0], out + got, 16 - got);
        if (n <= 0) {
            break;
        }
        got += (size_t)n;
    }
    close(fds[0]);

    int status = 0;
    int reaped = pid > 0 && waitpid(pid, &status, 0) == pid;
    return reaped && WIFEXITED(status) && WEXITSTATUS(status) == 0 && got == 16;
}

static void check_at_random(const char *self)
{
    const unsigned char *own = (const unsigned char *)getauxval(AT_RANDOM);
    CHECK(own != NULL, "AT_RANDOM is present");
    if (own == NULL) {
        return;
    }

    unsigned char first[16];
    unsigned char second[16];
    int first_ok = child_at_random(self, first);
    int second_ok = child_at_random(self, second);
    CHECK(first_ok, "first exec reports its AT_RANDOM bytes");
    CHECK(second_ok, "second exec reports its AT_RANDOM bytes");
    if (!first_ok || !second_ok) {
        return;
    }
    /* libc derives the stack protector canary and pointer guard from AT_RANDOM. */
    CHECK(memcmp(first, second, 16) != 0, "AT_RANDOM differs between execs");
    CHECK(memcmp(own, first, 16) != 0, "AT_RANDOM differs from the parent's");
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], PRINT_AT_RANDOM) == 0) {
        const unsigned char *bytes = (const unsigned char *)getauxval(AT_RANDOM);
        return bytes != NULL && write(STDOUT_FILENO, bytes, 16) == 16 ? 0 : 1;
    }

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

    unsigned long clktck = 0;
    CHECK(find_auxv_value(AT_CLKTCK, &clktck) == 1, "AT_CLKTCK is present");
    CHECK(clktck == 100, "AT_CLKTCK is USER_HZ");
    CHECK(sysconf(_SC_CLK_TCK) == 100, "sysconf(_SC_CLK_TCK) is USER_HZ");

    unsigned long flags = 1;
    CHECK(find_auxv_value(AT_FLAGS, &flags) == 1, "AT_FLAGS is present");
    CHECK(flags == 0, "AT_FLAGS is zero for a normal exec");

    check_at_random(argv[0]);

    TEST_DONE();
}
