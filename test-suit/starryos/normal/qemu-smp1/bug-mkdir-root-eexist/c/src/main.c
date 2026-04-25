/*
 * bug-mkdir-root-eexist: mkdir("/") must report EEXIST, not EINVAL.
 *
 * BusyBox `mkdir -p /tmp/...` walks path components from the root. Linux
 * returns EEXIST when mkdir("/") is attempted, allowing mkdir -p to continue.
 * Returning EINVAL aborts that chain.
 */
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int fail(const char *msg)
{
    printf("FAIL: %s\n", msg);
    printf("TEST FAILED\n");
    return 1;
}

int main(void)
{
    printf("=== bug-mkdir-root-eexist ===\n");

    errno = 0;
    int rc = mkdir("/", 0755);
    if (rc != -1) {
        return fail("mkdir(\"/\") unexpectedly succeeded");
    }
    if (errno != EEXIST) {
        printf("FAIL: mkdir(\"/\") errno=%d (%s), expected EEXIST (%d)\n", errno,
               strerror(errno), EEXIST);
        printf("TEST FAILED\n");
        return 1;
    }
    printf("PASS: mkdir(\"/\") returns EEXIST\n");

    unlink("/tmp/bug_mkdir_root_eexist");
    rmdir("/tmp/bug_mkdir_root_eexist");

    rc = mkdir("/tmp/bug_mkdir_root_eexist", 0755);
    if (rc != 0) {
        printf("FAIL: mkdir regular directory errno=%d (%s)\n", errno, strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }

    errno = 0;
    rc = mkdir("/tmp/bug_mkdir_root_eexist", 0755);
    if (rc != -1 || errno != EEXIST) {
        printf("FAIL: mkdir existing directory rc=%d errno=%d (%s), expected EEXIST\n", rc,
               errno, strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }

    rmdir("/tmp/bug_mkdir_root_eexist");
    printf("TEST PASSED\n");
    return 0;
}
