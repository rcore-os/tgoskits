// Loadable kernel module smoke loader.
//
// Loads the `hello` example module built by `cargo xtask starry kmod build`
// via finit_module(2) and prints a stable marker line. The module's init_fn
// output (printk via the `write_char` shim / ax_println) lands on the same
// serial console, so the test's success_regex can match both the loader marker
// and the module greeting.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

static int load_module(const char *path) {
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        printf("LOAD_FAIL open %s: %s\n", path, strerror(errno));
        return -1;
    }
    long r = syscall(SYS_finit_module, fd, "", 0);
    int saved = errno;
    close(fd);
    if (r != 0) {
        printf("LOAD_FAIL finit_module %s: ret=%ld errno=%s\n", path, r,
               strerror(saved));
        return -1;
    }
    printf("LOADED %s\n", path);
    return 0;
}

int main(void) {
    int rc = 0;
    rc |= load_module("/lib/modules/hello.ko");
    printf("KMOD_SMOKE_DONE rc=%d\n", rc);
    return rc ? 1 : 0;
}
