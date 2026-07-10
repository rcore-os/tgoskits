#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define REUSE_MAX_TRIES 1024
#define PREALLOC_COUNT  512

static int fails;

static void pass(const char *msg)
{
    printf("  PASS: %s\n", msg);
}

static void fail(const char *msg)
{
    printf("  FAIL: %s (errno=%d: %s)\n", msg, errno, strerror(errno));
    fails++;
}

/*
 * Regression: verify that when an inode is freed after unlink+close,
 * its page-cache key is cleaned up so a new file reusing the same
 * inode number does not see stale cached data.
 *
 * Strategy:
 *   1. pre-allocate a batch of files in /tmp to consume free inodes
 *      in the same block group, making our target inode more likely
 *      to be at the front of the free list.
 *   2. create target file, write old payload, stat to get st_ino
 *   3. open a second fd (keep inode alive after unlink)
 *   4. unlink
 *   5. close the second fd (inode freed, page-cache must be evicted)
 *   6. loop-create temp files until same st_ino is reused (max 1024 tries)
 *   7. write a DIFFERENT payload to the reused-inode file
 *   8. read back — must match the new payload, NOT the old one
 *
 * If inode reuse is not triggered after max tries, the test FAILS
 * with a diagnostic message — it does NOT silently pass.
 */
int main(void)
{
    const char *path = "/tmp/ext4-unlink-pagecache.tmp";
    const char *old_payload = "OLD STALE DATA FROM UNLINKED INODE";
    const char *new_payload = "FRESH DATA — page-cache must be clean";
    char buf[256] = {0};
    int fd = -1;
    ino_t old_ino = 0;

    printf("=== ext4 unlink page-cache regression ===\n");
    unlink(path);

    /*
     * Phase 0: pre-allocate files to consume free inodes in the same
     * block group.  This pushes the inode allocator to recycle recently
     * freed inodes more aggressively in the reuse loop below.
     */
    {
        int prealloc_fds[PREALLOC_COUNT];
        int i, kept = 0;
        printf("  INFO: pre-allocating %d files to warm inode allocator\n",
               PREALLOC_COUNT);
        for (i = 0; i < PREALLOC_COUNT; i++) {
            char tmp_path[64];
            snprintf(tmp_path, sizeof(tmp_path),
                     "/tmp/ext4-pre-%d.tmp", i);
            int pf = open(tmp_path, O_RDWR | O_CREAT | O_TRUNC, 0644);
            if (pf < 0) {
                printf("  INFO: pre-alloc stopped at %d (errno=%d)\n", i, errno);
                break;
            }
            prealloc_fds[kept++] = pf;
        }
        printf("  INFO: pre-allocated %d files\n", kept);
        pass("pre-allocate inode pool");

        /* Now create the target file while pre-alloc files still hold inodes */
        fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
        if (fd < 0) {
            for (i = 0; i < kept; i++) {
                char tmp_path[64];
                snprintf(tmp_path, sizeof(tmp_path),
                         "/tmp/ext4-pre-%d.tmp", i);
                close(prealloc_fds[i]);
                unlink(tmp_path);
            }
            fail("open temp file for write");
            goto out;
        }
        pass("open temp file for write");

        ssize_t n = write(fd, old_payload, strlen(old_payload));
        if (n != (ssize_t)strlen(old_payload)) {
            for (i = 0; i < kept; i++) {
                char tmp_path[64];
                snprintf(tmp_path, sizeof(tmp_path),
                         "/tmp/ext4-pre-%d.tmp", i);
                close(prealloc_fds[i]);
                unlink(tmp_path);
            }
            fail("write old payload");
            goto out_close_fd;
        }
        pass("write old payload");

        {
            struct stat st;
            if (fstat(fd, &st) != 0) {
                for (i = 0; i < kept; i++) {
                    char tmp_path[64];
                    snprintf(tmp_path, sizeof(tmp_path),
                             "/tmp/ext4-pre-%d.tmp", i);
                    close(prealloc_fds[i]);
                    unlink(tmp_path);
                }
                fail("fstat to get inode");
                goto out_close_fd;
            }
            old_ino = st.st_ino;
        }
        printf("  INFO: old file inode=%lu\n", (unsigned long)old_ino);
        close(fd);

        /* Release pre-alloc files — their inodes go back to the free list */
        for (i = 0; i < kept; i++) {
            close(prealloc_fds[i]);
        }
        for (i = 0; i < PREALLOC_COUNT; i++) {
            char tmp_path[64];
            snprintf(tmp_path, sizeof(tmp_path),
                     "/tmp/ext4-pre-%d.tmp", i);
            unlink(tmp_path);
        }
        pass("release pre-alloc files (inodes back to free list)");
    }

    /* Step 2: open a second fd to keep the inode alive */
    fd = open(path, O_RDONLY);
    if (fd < 0) {
        fail("open file for read (keep inode alive)");
        goto out;
    }
    pass("open file for read (keep inode alive)");

    /* Step 3: unlink while fd is still open */
    if (unlink(path) != 0) {
        fail("unlink while fd open");
        goto out_close_fd;
    }
    pass("unlink while fd open");

    /* Step 4: close the fd — inode freed, page-cache key evicted */
    close(fd);
    fd = -1;
    pass("close fd (inode freed, page-cache must be evicted)");

    /*
     * Step 5: loop-create temp files until the same inode number is reused.
     *
     * With the pre-allocated inode pool released above, the ext4 inode
     * allocator has many recently freed inodes in the same block group.
     * It should recycle our target inode within a few allocations.
     */
    {
        int reused = 0;
        int tries;
        for (tries = 0; tries < REUSE_MAX_TRIES; tries++) {
            char tmp_path[64];
            snprintf(tmp_path, sizeof(tmp_path),
                     "/tmp/ext4-reuse-%d.tmp", tries);

            int tmp_fd = open(tmp_path, O_RDWR | O_CREAT | O_TRUNC, 0644);
            if (tmp_fd < 0) {
                printf("  INFO: cannot create temp file %d (errno=%d), stop loop\n",
                       tries, errno);
                break;
            }

            struct stat tmp_st;
            if (fstat(tmp_fd, &tmp_st) != 0) {
                close(tmp_fd);
                unlink(tmp_path);
                continue;
            }

            if (tmp_st.st_ino == old_ino) {
                reused = 1;
                printf("  INFO: inode %lu reused at attempt %d\n",
                       (unsigned long)old_ino, tries + 1);
                fd = tmp_fd;
                break;
            }

            close(tmp_fd);
            unlink(tmp_path);
        }

        if (!reused) {
            printf("  FAIL: inode %lu not reused after %d tries\n",
                   (unsigned long)old_ino, tries);
            printf("  The page-cache-key cleanup cannot be verified\n");
            printf("  without inode reuse. This may indicate an inode\n");
            printf("  allocator change or an unusually sparse filesystem.\n");
            fails++;
            goto out;
        }
    }

    /* Step 6: write a DIFFERENT payload to the reused-inode file */
    {
        ssize_t n = write(fd, new_payload, strlen(new_payload));
        if (n != (ssize_t)strlen(new_payload)) {
            fail("write new payload to reused-inode file");
            goto out_close_fd;
        }
    }
    pass("write new payload to reused-inode file");

    /* Step 7: seek to beginning and read back */
    if (lseek(fd, 0, SEEK_SET) != 0) {
        fail("seek reused-inode file");
        goto out_close_fd;
    }
    pass("seek reused-inode file");

    memset(buf, 0, sizeof(buf));
    {
        ssize_t n = read(fd, buf, sizeof(buf) - 1);
        if (n < 0) {
            fail("read reused-inode file");
            goto out_close_fd;
        }
        buf[n] = '\0';

        if (n != (ssize_t)strlen(new_payload)) {
            printf("  FAIL: expected %zu bytes, got %zd\n",
                   strlen(new_payload), n);
            fails++;
            goto out_close_fd;
        }
    }
    pass("read reused-inode file");

    /* Step 8: verify new payload, not old */
    if (strcmp(buf, new_payload) != 0) {
        printf("  FAIL: payload mismatch\n");
        printf("  expected: %s\n", new_payload);
        printf("  got:      %s\n", buf);
        if (strstr(buf, old_payload) != NULL) {
            fail("reused-inode file contains STALE DATA — page-cache leak");
        } else {
            fail("reused-inode file has unexpected content");
        }
        goto out_close_fd;
    }
    pass("reused-inode file has correct new payload (no stale data)");

out_close_fd:
    if (fd >= 0)
        close(fd);
out:
    /* Cleanup loop temp files */
    {
        int tries;
        for (tries = 0; tries < REUSE_MAX_TRIES; tries++) {
            char tmp_path[64];
            snprintf(tmp_path, sizeof(tmp_path),
                     "/tmp/ext4-reuse-%d.tmp", tries);
            unlink(tmp_path);
        }
    }
    unlink(path);

    printf("\n=== Results: %s ===\n", fails == 0 ? "pass" : "fail");
    if (fails == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    printf("TEST FAILED\n");
    return 1;
}
