#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <unistd.h>

struct open_how {
    uint64_t flags, mode, resolve;
};

static int failures;

static void check(int condition, const char *name)
{
    printf("%s: %s (errno=%d)\n", condition ? "PASS" : "FAIL", name, errno);
    failures += !condition;
}

int main(void)
{
    char path[] = "/tmp/path-fd-validation-XXXXXX";
    int regular = mkstemp(path);
    int directory = open("/tmp", O_RDONLY | O_DIRECTORY);
    if (regular < 0 || directory < 0) {
        perror("setup");
        return 1;
    }
    unlink(path);

    const char *names[] = {".", "./", ".//"};
    for (unsigned i = 0; i < sizeof(names) / sizeof(names[0]); i++) {
        errno = 0;
        check(linkat(-1, names[i], directory, "unused-link", 0) == -1
                  && errno == EBADF, "linkat validates invalid source dirfd");
        errno = 0;
        check(linkat(regular, names[i], directory, "unused-link", 0) == -1
                  && errno == ENOTDIR, "linkat rejects non-directory source dirfd");
        errno = 0;
        check(linkat(directory, names[i], directory, "unused-link", 0) == -1
                  && errno == EPERM, "linkat rejects resolved directory source");

        int fd = openat(directory, names[i], O_PATH | O_CREAT | O_EXCL, 0);
        check(fd >= 0, "openat ignores creation flags with O_PATH");
        if (fd >= 0) close(fd);

        const uint64_t resolves[] = {0, 0x08 | 0x04};
        for (unsigned j = 0; j < sizeof(resolves) / sizeof(resolves[0]); j++) {
            struct open_how how = {
                .flags = O_PATH | O_CREAT | O_EXCL,
                .resolve = resolves[j],
            };
            errno = 0;
            fd = syscall(SYS_openat2, directory, names[i], &how, sizeof(how));
            check(fd == -1 && errno == EINVAL,
                  "openat2 rejects creation flags with O_PATH");
            if (fd >= 0) close(fd);
            how.flags = O_PATH | O_DIRECTORY | O_CLOEXEC;
            fd = syscall(SYS_openat2, directory, names[i], &how, sizeof(how));
            check(fd >= 0, "openat2 accepts valid O_PATH directory flags");
            if (fd >= 0) close(fd);
            how.flags = O_CREAT | O_EXCL | O_RDONLY;
            errno = 0;
            fd = syscall(SYS_openat2, directory, names[i], &how, sizeof(how));
            check(fd == -1 && errno == EEXIST,
                  "openat2 preserves exclusive creation without O_PATH");
            if (fd >= 0) close(fd);
        }
    }
    close(regular);
    close(directory);
    return failures ? 1 : 0;
}
