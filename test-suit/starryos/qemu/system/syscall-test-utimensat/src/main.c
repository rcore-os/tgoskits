#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
#include <errno.h>

#include "test_framework.h"

static char test_dir[] = "/tmp/test-utimensat-XXXXXX";
static char test_file_path[sizeof(test_dir) + sizeof("/test-utimensat-file")];
static char test_file2_path[sizeof(test_dir) + sizeof("/test-utimensat-file2")];
static char test_link_path[sizeof(test_dir) + sizeof("/test-utimensat-link")];
static char test_missing_path[sizeof(test_dir) + sizeof("/test-utimensat-noent")];
#define TMPFILE test_file_path
#define TMPFILE2 test_file2_path
#define TMPLINK test_link_path
#define NONFILE test_missing_path

/* Pass raw arguments, including NULL, without libc's pathname checks. */
static int raw_utimensat(int dirfd, const char *path,
                        const struct timespec times[2], int flags)
{
    return (int)syscall(SYS_utimensat, dirfd, path, times, flags);
}

#ifndef UTIME_NOW
#define UTIME_NOW  ((1L << 30) - 1)
#endif
#ifndef UTIME_OMIT
#define UTIME_OMIT ((1L << 30) - 2)
#endif

static int create_tmpfile(const char *path)
{
	int fd = creat(path, 0644);
	if (fd < 0) {
		perror("creat");
		exit(1);
	}
	write(fd, "x", 1);
	close(fd);
	return fd;
}

/* ================================================================ */

void test_set_specific_times(void)
{
	TEST_START("set_specific_times");
	unlink(TMPFILE);
	create_tmpfile(TMPFILE);

	struct timespec ts[2] = {{0}, {0}};
	ts[0].tv_sec  = 1000000000; /* atime: 2001-09-09 */
	ts[0].tv_nsec = 123456789;
	ts[1].tv_sec  = 1100000000; /* mtime: 2004-11-10 */
	ts[1].tv_nsec = 987654321;

	CHECK_RET(raw_utimensat(AT_FDCWD, TMPFILE, ts, 0), 0, "utimensat set times");

	struct stat st;
	CHECK_RET(stat(TMPFILE, &st), 0, "stat file");
	CHECK(st.st_atim.tv_sec == 1000000000, "atime sec mismatch");
	CHECK(st.st_atim.tv_nsec == 123456789, "atime nsec mismatch");
	CHECK(st.st_mtim.tv_sec == 1100000000, "mtime sec mismatch");
	CHECK(st.st_mtim.tv_nsec == 987654321, "mtime nsec mismatch");

	unlink(TMPFILE);
	TEST_DONE();
}

void test_utime_now(void)
{
	TEST_START("utime_now");
	unlink(TMPFILE);

	/* Set old timestamp first */
	create_tmpfile(TMPFILE);
	struct timespec old_ts[2] = {{0}, {0}};
	old_ts[0].tv_sec  = 1000000000;
	old_ts[0].tv_nsec = 0;
	old_ts[1].tv_sec  = 1000000000;
	old_ts[1].tv_nsec = 0;
	raw_utimensat(AT_FDCWD, TMPFILE, old_ts, 0);

	time_t before = time(NULL);

	struct timespec ts[2] = {{0}, {0}};
	ts[0].tv_nsec = UTIME_NOW;
	ts[1].tv_nsec = UTIME_NOW;
	CHECK_RET(raw_utimensat(AT_FDCWD, TMPFILE, ts, 0), 0, "utimensat UTIME_NOW");

	time_t after = time(NULL);

	struct stat st;
	CHECK_RET(stat(TMPFILE, &st), 0, "stat");
	/* Timestamps should be within [before, after] window */
	CHECK(st.st_atim.tv_sec >= before, "atime before window");
	CHECK(st.st_atim.tv_sec <= after, "atime after window");
	CHECK(st.st_mtim.tv_sec >= before, "mtime before window");
	CHECK(st.st_mtim.tv_sec <= after, "mtime after window");

	unlink(TMPFILE);
	TEST_DONE();
}

void test_utime_omit(void)
{
	TEST_START("utime_omit");
	unlink(TMPFILE);
	create_tmpfile(TMPFILE);

	/* Set known atime, then omit mtime */
	struct timespec ts_set[2] = {{0}, {0}};
	ts_set[0].tv_sec  = 1000000000;
	ts_set[0].tv_nsec = 0;
	ts_set[1].tv_sec  = 1100000000;
	ts_set[1].tv_nsec = 0;
	raw_utimensat(AT_FDCWD, TMPFILE, ts_set, 0);

	struct timespec ts[2] = {{0}, {0}};
	ts[0].tv_nsec = UTIME_OMIT;      /* leave atime */
	ts[1].tv_sec  = 1200000000;      /* update mtime */
	ts[1].tv_nsec = 555000000;
	CHECK_RET(raw_utimensat(AT_FDCWD, TMPFILE, ts, 0), 0, "utimensat OMIT+set");

	struct stat st;
	CHECK_RET(stat(TMPFILE, &st), 0, "stat");
	CHECK(st.st_atim.tv_sec == 1000000000, "atime should be unchanged");
	CHECK(st.st_mtim.tv_sec == 1200000000, "mtime should be updated");

	unlink(TMPFILE);
	TEST_DONE();
}

void test_null_times(void)
{
	TEST_START("null_times");
	unlink(TMPFILE);
	create_tmpfile(TMPFILE);

	/* Set old time first */
	struct timespec old_ts[2] = {{0}, {0}};
	old_ts[0].tv_sec  = 1000000000;
	old_ts[0].tv_nsec = 0;
	old_ts[1].tv_sec  = 1000000000;
	old_ts[1].tv_nsec = 0;
	raw_utimensat(AT_FDCWD, TMPFILE, old_ts, 0);

	time_t before = time(NULL);
	CHECK_RET(raw_utimensat(AT_FDCWD, TMPFILE, NULL, 0), 0, "utimensat NULL");
	time_t after = time(NULL);

	struct stat st;
	CHECK_RET(stat(TMPFILE, &st), 0, "stat");
	CHECK(st.st_atim.tv_sec >= before, "atime before");
	CHECK(st.st_atim.tv_sec <= after, "atime after");
	CHECK(st.st_mtim.tv_sec >= before, "mtime before");
	CHECK(st.st_mtim.tv_sec <= after, "mtime after");

	unlink(TMPFILE);
	TEST_DONE();
}

void test_both_omit(void)
{
	TEST_START("both_omit");
	unlink(NONFILE);

	/* Both OMIT on non-existent file should succeed (POSIX) */
	struct timespec ts[2] = {{0}, {0}};
	ts[0].tv_nsec = UTIME_OMIT;
	ts[1].tv_nsec = UTIME_OMIT;
	CHECK_RET(raw_utimensat(AT_FDCWD, NONFILE, ts, 0), 0,
		  "utimensat both OMIT non-existent");
    CHECK_RET(raw_utimensat(-1, (const char *)1, ts, 0x40000000), 0,
              "both OMIT bypasses fd, pathname and flag checks");
	TEST_DONE();
}

void test_symlink_nofollow(void)
{
	TEST_START("symlink_nofollow");
	unlink(TMPFILE);
	unlink(TMPFILE2);
	unlink(TMPLINK);
	create_tmpfile(TMPFILE2);

	/* Create symlink pointing to TMPFILE2 */
	CHECK_RET(symlink("test-utimensat-file2", TMPLINK), 0, "create symlink");

	/* Set times on the symlink itself with AT_SYMLINK_NOFOLLOW */
	struct timespec ts[2] = {{0}, {0}};
	ts[0].tv_sec  = 1500000000;
	ts[0].tv_nsec = 111000000;
	ts[1].tv_sec  = 1600000000;
	ts[1].tv_nsec = 222000000;
	CHECK_RET(raw_utimensat(AT_FDCWD, TMPLINK, ts, AT_SYMLINK_NOFOLLOW), 0,
		  "utimensat symlink nofollow");

	/* Verify target file is UNCHANGED */
	struct stat st_target;
	CHECK_RET(stat(TMPFILE2, &st_target), 0, "stat target");
	CHECK(st_target.st_atim.tv_sec != 1500000000, "target atime changed!");

	unlink(TMPFILE);
	unlink(TMPFILE2);
	unlink(TMPLINK);
	TEST_DONE();
}

void test_empty_path(void)
{
	TEST_START("empty_path");
	unlink(TMPFILE);
	int fd = open(TMPFILE, O_CREAT | O_RDWR, 0644);
	CHECK(fd >= 0, "open test_file_path");
	write(fd, "x", 1);

	struct timespec ts[2] = {{0}, {0}};
	ts[0].tv_sec  = 1700000000;
	ts[0].tv_nsec = 333000000;
	ts[1].tv_sec  = 1800000000;
	ts[1].tv_nsec = 444000000;

	/* NULL with a real fd selects the ordinary fd entry, with flags zero. */
	CHECK_RET(raw_utimensat(fd, NULL, ts, 0), 0, "utimensat NULL path");

	struct stat st;
	CHECK_RET(fstat(fd, &st), 0, "fstat");
	CHECK(st.st_atim.tv_sec == 1700000000, "atime sec mismatch (empty path)");
	CHECK(st.st_mtim.tv_sec == 1800000000, "mtime sec mismatch (empty path)");

    int pathfd = open(TMPFILE, O_PATH);
    CHECK(pathfd >= 0, "open O_PATH timestamp target");
    ts[0].tv_sec += 10;
    ts[1].tv_sec += 20;
    CHECK_RET(raw_utimensat(pathfd, "", ts, AT_EMPTY_PATH), 0,
              "AT_EMPTY_PATH accepts the retained O_PATH inode");
    struct timespec denied[2] = {{1900000000, 0}, {1900000001, 0}};
    CHECK_ERR(raw_utimensat(pathfd, NULL, denied, 0), EBADF,
              "NULL path uses ordinary fd admission and rejects O_PATH");
    CHECK_ERR(raw_utimensat(fd, NULL, denied, AT_EMPTY_PATH), EINVAL,
              "ordinary fd entry rejects AT_EMPTY_PATH");
    CHECK_ERR(raw_utimensat(-1, NULL, denied, AT_SYMLINK_NOFOLLOW), EINVAL,
              "fd-entry flags precede invalid fd lookup");
    CHECK_RET(fstat(fd, &st), 0, "fstat after rejected timestamp changes");
    CHECK(st.st_atim.tv_sec == ts[0].tv_sec &&
          st.st_mtim.tv_sec == ts[1].tv_sec,
          "rejected timestamp changes preserved both times");
    close(pathfd);

	close(fd);
	unlink(TMPFILE);
	TEST_DONE();
}

void test_invalid_flags(void)
{
	TEST_START("invalid_flags");
	unlink(TMPFILE);
	create_tmpfile(TMPFILE);

	struct timespec ts[2] = {{0}, {0}};
	ts[0].tv_nsec = UTIME_NOW;
	ts[1].tv_nsec = UTIME_NOW;

	/* 0xDEAD is not a valid flag combination */
	CHECK_ERR(raw_utimensat(AT_FDCWD, TMPFILE, ts, 0xDEAD), EINVAL,
		  "invalid flags");

	unlink(TMPFILE);
	TEST_DONE();
}

void test_invalid_nsec(void)
{
	TEST_START("invalid_nsec");
	unlink(TMPFILE);
	create_tmpfile(TMPFILE);

	struct timespec ts[2] = {{0}, {0}};
	ts[0].tv_sec  = 0;
	ts[0].tv_nsec = 1000000001; /* > 999999999, not UTIME special */
	ts[1].tv_nsec = UTIME_NOW;
	CHECK_ERR(raw_utimensat(AT_FDCWD, TMPFILE, ts, 0), EINVAL,
		  "tv_nsec > 999999999");

	ts[0].tv_nsec = -1; /* negative */
	CHECK_ERR(raw_utimensat(AT_FDCWD, TMPFILE, ts, 0), EINVAL, "tv_nsec < 0");
    unlink(NONFILE);
    CHECK_ERR(raw_utimensat(AT_FDCWD, NONFILE, ts, 0), ENOENT,
              "path lookup precedes nanosecond validation");
    CHECK_ERR(raw_utimensat(-1, NULL, ts, 0), EBADF,
              "fd lookup precedes nanosecond validation");

	unlink(TMPFILE);
	TEST_DONE();
}

void test_enoent(void)
{
	TEST_START("enoent");
	unlink(NONFILE);

	struct timespec ts[2] = {{0}, {0}};
	ts[0].tv_nsec = UTIME_NOW;
	ts[1].tv_nsec = UTIME_NOW;
	CHECK_ERR(raw_utimensat(AT_FDCWD, NONFILE, ts, 0), ENOENT, "ENOENT");

	TEST_DONE();
}

void test_ebadf(void)
{
	TEST_START("ebadf");
	int badfd = 9999;

	struct timespec ts[2] = {{0}, {0}};
	ts[0].tv_nsec = UTIME_NOW;
	ts[1].tv_nsec = UTIME_NOW;
	CHECK_ERR(raw_utimensat(badfd, "relative/path", ts, 0), EBADF, "EBADF");

	TEST_DONE();
}

/* ================================================================ */

int main(void)
{
    if (!mkdtemp(test_dir)) {
        perror("mkdtemp utimensat");
        return 1;
    }
    /* Each buffer includes the complete bounded template and its suffix. */
    snprintf(TMPFILE, sizeof(TMPFILE), "%s/test-utimensat-file", test_dir);
    snprintf(TMPFILE2, sizeof(TMPFILE2), "%s/test-utimensat-file2", test_dir);
    snprintf(TMPLINK, sizeof(TMPLINK), "%s/test-utimensat-link", test_dir);
    snprintf(NONFILE, sizeof(NONFILE), "%s/test-utimensat-noent", test_dir);
	printf("STARRY_GROUPED_TEST_START\n");

	test_set_specific_times();
	test_utime_now();
	test_utime_omit();
	test_null_times();
	test_both_omit();
	test_symlink_nofollow();
	test_empty_path();
	test_invalid_flags();
	test_invalid_nsec();
	test_enoent();
	test_ebadf();

    if (rmdir(test_dir) != 0) {
        perror("rmdir utimensat");
        return 1;
    }
	printf("UTIMENSAT_TESTS_PASSED\n");
	return 0;
}
