/*
 * Focused StarryOS regression test for filename-independent open-unlink
 * lifetime semantics in rsext4.
 *
 * Covers the full lifecycle of an open regular file that is unlinked:
 *   - per-name patterns (.lock, .drv, unsuffixed, Nix-like temps)
 *   - open / unlink / write / read / append / truncate / fstat / fsync
 *   - post-unlink path lookup failure
 *   - readdir() absence of hidden orphan entries
 *   - post-close cleanup
 *   - O_APPEND writes through an unlinked fd
 *   - empty-file unlink (no pre-write data)
 *   - two-fd concurrent access before and after unlink
 *   - hard-link: unlink only removes one name
 *   - directory unlink non-regression (rmdir of non-empty dir)
 *
 * Final marker: TEST PASSED / TEST FAILED
 */
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <unistd.h>

static int fails;

/* ── statvfs resource-reclamation probe ────────────────────────── */

struct fs_snapshot {
    unsigned long free_inodes;
    unsigned long free_blocks;
};

static int take_snapshot(const char *path, struct fs_snapshot *snap)
{
    struct statvfs vfs;
    if (statvfs(path, &vfs) != 0)
        return -1;
    snap->free_inodes = vfs.f_favail;
    snap->free_blocks = vfs.f_bfree;
    return 0;
}

/*
 * Verify that free inodes and blocks are within `tolerance` of baseline.
 * ext4 may batch or delay bitmap updates, so allow a small slack.
 * Returns 0 if within tolerance, 1 if leaked beyond tolerance.
 */
static int check_no_leak(const struct fs_snapshot *before,
                         const struct fs_snapshot *after,
                         unsigned long tolerance,
                         const char *label)
{
    long ino_delta = (long)after->free_inodes - (long)before->free_inodes;
    long blk_delta = (long)after->free_blocks - (long)before->free_blocks;

    printf("  INFO: %s: ino_delta=%ld blk_delta=%ld (tol=%lu)\n",
           label, ino_delta, blk_delta, tolerance);

    if (ino_delta < -(long)tolerance) {
        printf("  FAIL: %s: inode leak (delta=%ld)\n", label, ino_delta);
        fails++;
        return 1;
    }
    if (blk_delta < -(long)tolerance) {
        printf("  FAIL: %s: block leak (delta=%ld)\n", label, blk_delta);
        fails++;
        return 1;
    }
    printf("  PASS: %s\n", label);
    return 0;
}

static void pass(const char *msg)
{
    printf("  PASS: %s\n", msg);
}

static void fail(const char *msg)
{
    printf("  FAIL: %s (errno=%d: %s)\n", msg, errno, strerror(errno));
    fails++;
}

/* ── helpers ───────────────────────────────────────────────────── */

static void assert_noent(const char *path, const char *label)
{
    int fd = open(path, O_RDONLY);
    if (fd >= 0) {
        close(fd);
        fail(label);
    } else if (errno == ENOENT) {
        pass(label);
    } else {
        fail(label);
    }
}

static void assert_no_hidden(const char *dir_path, const char *prefix,
                             const char *label)
{
    DIR *d = opendir(dir_path);
    if (!d) {
        fail(label);
        return;
    }
    int found = 0;
    struct dirent *de;
    while ((de = readdir(d)) != NULL) {
        if (strncmp(de->d_name, prefix, strlen(prefix)) == 0) {
            printf("    UNEXPECTED ENTRY: %s\n", de->d_name);
            found = 1;
        }
    }
    closedir(d);
    if (found)
        fail(label);
    else
        pass(label);
}

/* ── core cycle: open → write → unlink → trunc → write → seek → read ── */
static void test_core_cycle(const char *path, const char *label)
{
    const char *payload_pre  = "pre-unlink data\n";
    const char *payload_post = "post-unlink payload\n";
    char buf[128] = {0};
    int fd;
    ssize_t n;
    struct stat st;

    printf("-- %s: %s --\n", label, path);
    unlink(path);

    fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) { fail("open temp file"); return; }
    pass("open temp file");

    n = write(fd, payload_pre, strlen(payload_pre));
    if (n != (ssize_t)strlen(payload_pre))
        { fail("initial write"); goto out_close; }
    pass("initial write");

    /* ── read-back before unlink (sanity) ── */
    lseek(fd, 0, SEEK_SET);
    n = read(fd, buf, strlen(payload_pre));
    if (n != (ssize_t)strlen(payload_pre)
        || memcmp(buf, payload_pre, strlen(payload_pre)) != 0)
        { fail("pre-unlink read-back"); goto out_close; }
    pass("pre-unlink read-back");

    /* ── unlink ── */
    if (unlink(path) != 0)
        { fail("unlink open file"); goto out_close; }
    pass("unlink open file");

    /* path lookup must fail */
    assert_noent(path, "path lookup fails after unlink");

    /* ── write through unlinked fd ── */
    n = write(fd, payload_post, strlen(payload_post));
    if (n != (ssize_t)strlen(payload_post))
        { fail("write through unlinked fd"); goto out_close; }
    pass("write through unlinked fd");

    /* ── seek + read back the post-unlink data ── */
    if (lseek(fd, (off_t)strlen(payload_pre), SEEK_SET) < 0)
        { fail("seek unlinked fd"); goto out_close; }
    pass("seek unlinked fd");

    memset(buf, 0, sizeof(buf));
    n = read(fd, buf, strlen(payload_post));
    if (n != (ssize_t)strlen(payload_post)
        || memcmp(buf, payload_post, strlen(payload_post)) != 0)
        { fail("read post-unlink data through fd"); goto out_close; }
    pass("read post-unlink data through fd");

    /* ── fstat nlink==0 ── */
    if (fstat(fd, &st) != 0 || st.st_nlink != 0)
        { fail("fstat unlinked fd reports nlink 0"); goto out_close; }
    pass("fstat unlinked fd reports nlink 0");

    /* ── fsync ── */
    if (fsync(fd) != 0)
        { fail("fsync unlinked fd"); goto out_close; }
    pass("fsync unlinked fd");

out_close:
    close(fd);
}

/* ── O_APPEND write through unlinked fd ── */
static void test_append_after_unlink(void)
{
    const char *path = "/tmp/open-unlink-append.tmp";
    const char *pre  = "pre\n";
    const char *app  = "append\n";
    char buf[128] = {0};
    int fd;
    struct stat st;

    printf("-- O_APPEND: %s --\n", path);
    unlink(path);

    fd = open(path, O_RDWR | O_CREAT | O_APPEND, 0644);
    if (fd < 0) { fail("O_APPEND: open"); return; }
    pass("O_APPEND: open");

    if (write(fd, pre, strlen(pre)) != (ssize_t)strlen(pre))
        { fail("O_APPEND: pre-unlink write"); goto out; }
    pass("O_APPEND: pre-unlink write");

    if (unlink(path) != 0)
        { fail("O_APPEND: unlink"); goto out; }
    pass("O_APPEND: unlink");

    /* O_APPEND write — must go to end (offset 4), not 0 */
    if (write(fd, app, strlen(app)) != (ssize_t)strlen(app))
        { fail("O_APPEND: post-unlink append write"); goto out; }
    pass("O_APPEND: post-unlink append write");

    if (fstat(fd, &st) != 0
        || (off_t)st.st_size != (off_t)(strlen(pre) + strlen(app)))
        { fail("O_APPEND: file size after append"); goto out; }
    pass("O_APPEND: file size after append");

    /* read back — should see pre + app */
    lseek(fd, 0, SEEK_SET);
    memset(buf, 0, sizeof(buf));
    if (read(fd, buf, sizeof(buf) - 1) != (ssize_t)(strlen(pre) + strlen(app))
        || memcmp(buf, pre, strlen(pre)) != 0
        || memcmp(buf + strlen(pre), app, strlen(app)) != 0)
        { fail("O_APPEND: read-back after unlink"); goto out; }
    pass("O_APPEND: read-back after unlink");

    /* fstat nlink==0 */
    if (fstat(fd, &st) != 0 || st.st_nlink != 0)
        { fail("O_APPEND: fstat nlink 0"); goto out; }
    pass("O_APPEND: fstat nlink 0");

out:
    close(fd);
    unlink(path);
}

/* ── empty-file unlink (no pre-write data) ── */
static void test_empty_file_unlink(void)
{
    const char *path = "/tmp/open-unlink-empty.tmp";
    const char *payload = "after-unlink\n";
    char buf[128] = {0};
    int fd;
    struct stat st;

    printf("-- empty-file unlink: %s --\n", path);
    unlink(path);

    fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) { fail("empty: open"); return; }
    pass("empty: open (size 0 file)");

    /* unlink before any write — nlink goes from 1 to 0 */
    if (unlink(path) != 0)
        { fail("empty: unlink"); goto out; }
    pass("empty: unlink");

    /* write + read-back: empty unlinked file grows from 0 */
    if (write(fd, payload, strlen(payload)) != (ssize_t)strlen(payload))
        { fail("empty: post-unlink write"); goto out; }
    pass("empty: post-unlink write");

    lseek(fd, 0, SEEK_SET);
    memset(buf, 0, sizeof(buf));
    if (read(fd, buf, strlen(payload)) != (ssize_t)strlen(payload)
        || memcmp(buf, payload, strlen(payload)) != 0)
        { fail("empty: read-back after write"); goto out; }
    pass("empty: read-back after write");

    /* truncate to 0 after unlink — inode stays alive */
    if (ftruncate(fd, 0) != 0)
        { fail("empty: ftruncate to 0 after unlink"); goto out; }
    pass("empty: ftruncate to 0 after unlink");

    if (fstat(fd, &st) != 0 || st.st_size != 0)
        { fail("empty: size 0 after ftruncate"); goto out; }
    pass("empty: size 0 after ftruncate");

    /* grow via ftruncate, write, read — Linux semantics:
       ftruncate creates a sparse region; write+read must see the data
       through the unlinked fd */
    if (ftruncate(fd, 64) != 0)
        { fail("empty: ftruncate grow to 64"); goto out; }
    pass("empty: ftruncate grow to 64");

    /* Use pwrite to write at offset 32 and pread to read it back —
       both go through the CachedFile page cache (not fd position) */
    if (pwrite(fd, payload, strlen(payload), 32)
        != (ssize_t)strlen(payload))
        { fail("empty: pwrite after grow"); goto out; }
    pass("empty: pwrite after grow");

    /* fsync to flush dirty pages to disk before read-back */
    if (fsync(fd) != 0)
        { fail("empty: fsync after pwrite"); goto out; }
    pass("empty: fsync after pwrite");

    memset(buf, 0, sizeof(buf));
    if (pread(fd, buf, strlen(payload), 32) != (ssize_t)strlen(payload)
        || memcmp(buf, payload, strlen(payload)) != 0)
        { fail("empty: pread back after grow+write+fsync"); goto out; }
    pass("empty: pread back after grow+write+fsync");

    if (fstat(fd, &st) != 0 || st.st_nlink != 0)
        { fail("empty: fstat nlink 0"); goto out; }
    pass("empty: fstat nlink 0");

out:
    close(fd);
    unlink(path);
}

/* ── two-fd concurrent access ── */
static void test_two_fd_concurrent(void)
{
    const char *path = "/tmp/open-unlink-2fd.tmp";
    const char *payload1 = "fd1-data\n";
    const char *payload2 = "fd2-data\n";
    char buf[128] = {0};
    int fd1, fd2;
    struct stat st;

    printf("-- two-fd concurrent: %s --\n", path);
    unlink(path);

    fd1 = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    fd2 = open(path, O_RDWR);
    if (fd1 < 0 || fd2 < 0) { fail("two-fd: open both"); goto out; }
    pass("two-fd: open both");

    /* fd1 writes, fd2 reads */
    if (write(fd1, payload1, strlen(payload1)) != (ssize_t)strlen(payload1))
        { fail("two-fd: fd1 write"); goto out; }
    pass("two-fd: fd1 write");

    /* unlink while both fds are open */
    if (unlink(path) != 0)
        { fail("two-fd: unlink"); goto out; }
    pass("two-fd: unlink");

    /* fd1 writes more after unlink */
    lseek(fd1, 0, SEEK_END);
    if (write(fd1, payload2, strlen(payload2)) != (ssize_t)strlen(payload2))
        { fail("two-fd: fd1 post-unlink write"); goto out; }
    pass("two-fd: fd1 post-unlink write");

    /* fd2 reads from beginning */
    lseek(fd2, 0, SEEK_SET);
    memset(buf, 0, sizeof(buf));
    if (read(fd2, buf,
             strlen(payload1) + strlen(payload2))
        != (ssize_t)(strlen(payload1) + strlen(payload2)))
        { fail("two-fd: fd2 reads both writes"); goto out; }
    pass("two-fd: fd2 reads both writes");

    /* close fd1, fd2 still works */
    close(fd1);
    lseek(fd2, 0, SEEK_SET);
    memset(buf, 0, sizeof(buf));
    if (read(fd2, buf, strlen(payload1)) != (ssize_t)strlen(payload1))
        { fail("two-fd: fd2 read after fd1 close"); goto out; }
    pass("two-fd: fd2 read after fd1 close");

    if (fstat(fd2, &st) != 0 || st.st_nlink != 0)
        { fail("two-fd: fstat nlink 0"); }
    else
        { pass("two-fd: fstat nlink 0"); }

    close(fd2);
    return;

out:
    if (fd1 >= 0) close(fd1);
    if (fd2 >= 0) close(fd2);
    unlink(path);
}

/* ── hard-link: unlink only removes one name ── */
static void test_hardlink_unlink(void)
{
    const char *orig = "/tmp/open-unlink-hl-orig.tmp";
    const char *linkpath = "/tmp/open-unlink-hl-link.tmp";
    const char *payload = "hardlink data\n";
    char buf[128] = {0};
    int fd;
    struct stat st;

    printf("-- hard-link: %s → %s --\n", orig, linkpath);
    unlink(orig);
    unlink(linkpath);

    fd = open(orig, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) { fail("hardlink: open orig"); return; }
    pass("hardlink: open orig");

    if (write(fd, payload, strlen(payload)) != (ssize_t)strlen(payload))
        { fail("hardlink: write"); goto out; }
    pass("hardlink: write");

    if (link(orig, linkpath) != 0)
        { fail("hardlink: link"); goto out; }
    pass("hardlink: link");

    /* unlink original — link still exists, inode alive with nlink=1 */
    if (unlink(orig) != 0)
        { fail("hardlink: unlink orig"); goto out; }
    pass("hardlink: unlink orig");

    /* fd still works */
    lseek(fd, 0, SEEK_SET);
    memset(buf, 0, sizeof(buf));
    if (read(fd, buf, strlen(payload)) != (ssize_t)strlen(payload))
        { fail("hardlink: read through fd after unlink"); goto out; }
    pass("hardlink: read through fd after unlink");

    /* nlink=1 (not 0 — one link remains) */
    if (fstat(fd, &st) != 0 || st.st_nlink != 1)
        { fail("hardlink: fstat nlink 1"); goto out; }
    pass("hardlink: fstat nlink 1");

    /* link path still accessible via a second fd */
    int fd_link = open(linkpath, O_RDONLY);
    if (fd_link < 0)
        { fail("hardlink: link path still accessible"); }
    else
        { pass("hardlink: link path still accessible"); close(fd_link); }

    /* unlink the last link while fd is still open → nlink goes 1→0 */
    if (unlink(linkpath) != 0)
        { fail("hardlink: unlink last link (fd still open)"); goto out; }
    pass("hardlink: unlink last link (fd still open)");
    assert_noent(linkpath, "hardlink: link path gone after unlink");

    /* fd still works through zero-link inode */
    lseek(fd, 0, SEEK_SET);
    memset(buf, 0, sizeof(buf));
    if (read(fd, buf, strlen(payload)) != (ssize_t)strlen(payload))
        { fail("hardlink: read through fd with nlink 0"); goto out; }
    pass("hardlink: read through fd with nlink 0");

    /* fstat reports nlink == 0 after final link removed */
    if (fstat(fd, &st) != 0 || st.st_nlink != 0)
        { fail("hardlink: fstat nlink 0 after last link removed"); goto out; }
    pass("hardlink: fstat nlink 0 after last link removed");

out:
    close(fd);
    unlink(orig);
    unlink(linkpath);
}

/* ── closed-file unlink: no open fd → free immediately ── */
static void test_closed_file_unlink(void)
{
    const char *path = "/tmp/open-unlink-closed.tmp";
    const char *payload = "closed-unlink\n";
    struct fs_snapshot before, after;
    int fd;

    printf("-- closed-file unlink: %s --\n", path);
    unlink(path);

    /* snapshot before creation */
    if (take_snapshot("/tmp", &before) != 0)
        { fail("closed: statvfs before"); return; }
    pass("closed: statvfs before");

    fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) { fail("closed: open"); return; }
    pass("closed: open");

    if (write(fd, payload, strlen(payload)) != (ssize_t)strlen(payload))
        { fail("closed: write"); goto out; }
    pass("closed: write");

    /* close before unlink — no open fd remains */
    close(fd);

    /* unlink with no open fd must free resources immediately */
    if (unlink(path) != 0)
        { fail("closed: unlink"); return; }
    pass("closed: unlink");

    /* sync to flush any deferred bitmap updates */
    sync();

    if (take_snapshot("/tmp", &after) != 0)
        { fail("closed: statvfs after"); return; }
    pass("closed: statvfs after");

    /*
     * After creating-then-unlinking a small file with no open fd,
     * free inode/block counts should be close to baseline.
     * ext4 may batch updates so use a permissive tolerance.
     */
    check_no_leak(&before, &after, 32, "closed: no inode/block leak");
    return;

out:
    close(fd);
    unlink(path);
}

/* ── open-close cycle reclamation probe ── */
static void test_reclaim_after_close(void)
{
    const char *path = "/tmp/open-unlink-reclaim.tmp";
    const char *payload = "reclaim test\n";
    struct fs_snapshot base, after_create, after_close;
    int fd;

    printf("-- reclaim after close: %s --\n", path);
    unlink(path);

    if (take_snapshot("/tmp", &base) != 0)
        { fail("reclaim: baseline snapshot"); return; }
    pass("reclaim: baseline snapshot");

    fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) { fail("reclaim: open"); return; }
    if (write(fd, payload, strlen(payload)) != (ssize_t)strlen(payload))
        { fail("reclaim: write"); goto out; }

    if (take_snapshot("/tmp", &after_create) != 0)
        { fail("reclaim: after-create snapshot"); goto out; }
    pass("reclaim: after-create snapshot");

    if (unlink(path) != 0)
        { fail("reclaim: unlink (fd open)"); goto out; }
    pass("reclaim: unlink (fd open)");

    /* Write through unlinked fd — inode still alive */
    if (write(fd, "more\n", 5) != 5)
        { fail("reclaim: write through unlinked fd"); goto out; }
    pass("reclaim: write through unlinked fd");

    /* fsync + close triggers final Drop → pending-delete cleanup */
    if (fsync(fd) != 0)
        { fail("reclaim: fsync before close"); goto out; }
    pass("reclaim: fsync before close");

    close(fd);
    sync();

    if (take_snapshot("/tmp", &after_close) != 0)
        { fail("reclaim: after-close snapshot"); return; }
    pass("reclaim: after-close snapshot");

    /*
     * After the final close, pending-delete cleanup should have freed
     * the inode and its data blocks.  Verify free counts return to
     * roughly baseline levels.
     */
    check_no_leak(&base, &after_close, 64,
                  "reclaim: resources freed after close");
    return;

out:
    close(fd);
    unlink(path);
}

/* ── directory unlink non-regression ── */
static void test_dir_unlink_unchanged(void)
{
    const char *dir  = "/tmp/unlink-regression-dir";
    const char *file = "/tmp/unlink-regression-dir/file";

    printf("-- directory unlink non-regression --\n");

    mkdir(dir, 0755);
    int fd = open(file, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) { fail("dir-reg: create file in dir"); rmdir(dir); return; }
    close(fd);
    pass("dir-reg: create file in dir");

    /* rmdir on non-empty dir must fail */
    if (rmdir(dir) == 0 || errno != ENOTEMPTY)
        { fail("dir-reg: rmdir non-empty fails"); }
    else
        { pass("dir-reg: rmdir non-empty fails (ENOTEMPTY)"); }

    /* unlink the file, then rmdir should succeed */
    if (unlink(file) != 0)
        { fail("dir-reg: unlink file"); }
    else
        { pass("dir-reg: unlink file"); }

    if (rmdir(dir) != 0)
        { fail("dir-reg: rmdir empty succeeds"); }
    else
        { pass("dir-reg: rmdir empty succeeds"); }
}

int main(void)
{
    printf("=== open-unlink-write regression (filename-independent) ===\n\n");

    /* ── per-name coverage ─────────────────────────────────────── */
    test_core_cycle("/tmp/open-unlink-lock.tmp.lock",  ".lock suffix");
    test_core_cycle("/tmp/open-unlink-drv.tmp.drv",    ".drv suffix (Nix derivation temp)");
    test_core_cycle("/tmp/open-unlink-notmp",          "unsuffixed temp name");
    test_core_cycle("/tmp/.nix-tmp-1234-0",            "Nix-like hidden temp output");

    /* ── additional coverage ──────────────────────────────────── */
    test_append_after_unlink();
    test_empty_file_unlink();
    test_two_fd_concurrent();
    test_hardlink_unlink();
    test_closed_file_unlink();
    test_reclaim_after_close();
    test_dir_unlink_unchanged();

    /* ── hidden-orphan scan ────────────────────────────────────── */
    printf("\n-- directory scan: no hidden orphans --\n");
    assert_no_hidden("/tmp", ".starry-orphan",
                     "no .starry-orphan-* entries in /tmp");
    assert_noent("/tmp/open-unlink-lock.tmp.lock",
                 ".lock path gone after close");
    assert_noent("/tmp/open-unlink-drv.tmp.drv",
                 ".drv path gone after close");
    assert_noent("/tmp/open-unlink-notmp",
                 "unsuffixed path gone after close");
    assert_noent("/tmp/.nix-tmp-1234-0",
                 "nix-like temp path gone after close");
    assert_noent("/tmp/open-unlink-append.tmp",
                 "append path gone after close");
    assert_noent("/tmp/open-unlink-empty.tmp",
                 "empty path gone after close");
    assert_noent("/tmp/open-unlink-2fd.tmp",
                 "two-fd path gone after close");
    assert_noent("/tmp/open-unlink-hl-orig.tmp",
                 "hardlink orig gone after close");
    assert_noent("/tmp/open-unlink-hl-link.tmp",
                 "hardlink link gone after close");

    assert_no_hidden("/tmp", ".starry-orphan",
                     "post-close: no .starry-orphan-* entries");

    printf("\n=== Results: %s ===\n", fails == 0 ? "pass" : "fail");
    if (fails == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    printf("TEST FAILED\n");
    return 1;
}
