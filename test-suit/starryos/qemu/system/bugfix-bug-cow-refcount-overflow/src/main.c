/*
 * bug-cow-refcount-overflow: a read-only frame shared COW by the parent plus
 * many forked children must not overflow the per-frame reference count. With a
 * u8 count the ~255th concurrent sharer overflowed and fork() failed with
 * EFAULT (seen on `hackbench -P g10`, ~400 tasks). A u32 count fixes it.
 *
 * Strategy: fork CHILDREN children that each only READ a shared .rodata frame
 * (so COW is never broken and the single frame's refcount climbs to
 * CHILDREN+1), keeping them all alive on a pipe until every fork has happened.
 */
#define _GNU_SOURCE

#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

/* > 255 concurrent sharers so a u8 refcount would overflow; the parent is one
 * more sharer on top, giving CHILDREN + 1. */
enum { CHILDREN = 300 };

/* A page-sized read-only blob in .rodata, shared COW across every fork. */
static const volatile unsigned char shared_ro_blob[4096] = {0xC0, 0x1D};

int main(void)
{
    printf("=== bug-cow-refcount-overflow ===\n");
    printf("Expected: %d concurrent COW sharers of one read-only frame do not\n",
           CHILDREN);
    printf("          overflow the reference count (fork never returns EFAULT).\n\n");

    int block_pipe[2];
    if (pipe(block_pipe) != 0) {
        printf("FAIL: pipe: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }

    /*
     * Fault the shared read-only blob into THIS process before forking, so the
     * frame is resident with refcount 1 in the parent. Each of the CHILDREN
     * forks then COW-shares exactly this frame (clone_map does
     * frame.count.checked_add(1)), driving it to CHILDREN+1 and reliably
     * overflowing a u8 count past 255 — without the pre-fault the blob's first
     * access is inside a child and demand paging would not exercise the intended
     * single-frame overflow.
     */
    volatile unsigned char probe = shared_ro_blob[0];
    if (probe != 0xC0) {
        printf("FAIL: unexpected shared blob byte 0x%02x\n", probe);
        printf("TEST FAILED\n");
        return 1;
    }

    pid_t children[CHILDREN];
    int forked = 0;
    for (int i = 0; i < CHILDREN; i++) {
        pid_t pid = fork();
        if (pid < 0) {
            printf("FAIL: fork #%d failed: %s (errno %d)\n", i, strerror(errno),
                   errno);
            printf("      a u8 COW refcount overflows here around the 255th "
                   "sharer\n");
            /* Release whatever children we managed to create, then fail. */
            close(block_pipe[1]);
            for (int j = 0; j < forked; j++) {
                waitpid(children[j], NULL, 0);
            }
            close(block_pipe[0]);
            printf("TEST FAILED\n");
            return 1;
        }
        if (pid == 0) {
            /* Child: only READ the shared frame (never break COW), then block
             * on the pipe until the parent closes it. */
            close(block_pipe[1]);
            volatile unsigned char sink = shared_ro_blob[0];
            (void)sink;
            char b;
            while (read(block_pipe[0], &b, 1) > 0) {
                /* wait for EOF */
            }
            _exit(0);
        }
        children[forked++] = pid;
    }

    printf("PASS: all %d forks succeeded (no EFAULT from refcount overflow)\n",
           forked);

    /* Release the children (EOF on the pipe) and reap them. */
    close(block_pipe[1]);
    close(block_pipe[0]);
    int failures = 0;
    for (int i = 0; i < forked; i++) {
        int status = 0;
        if (waitpid(children[i], &status, 0) != children[i] ||
            !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            failures++;
        }
    }
    if (failures != 0) {
        printf("FAIL: %d children exited abnormally\n", failures);
        printf("TEST FAILED\n");
        return 1;
    }
    printf("PASS: all %d children reaped cleanly\n", forked);

    printf("TEST PASSED\n");
    return 0;
}
