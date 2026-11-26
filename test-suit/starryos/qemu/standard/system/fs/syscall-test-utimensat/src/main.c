#define _GNU_SOURCE
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
#include <errno.h>

#include "test_framework.h"

#define TMPFILE  "/tmp/test-utimensat-file"
#define TMPFILE2 "/tmp/test-utimensat-file2"
#define TMPLINK  "/tmp/test-utimensat-link"
#define NONFILE  "/tmp/test-utimensat-noent"

/* /tmp is a memory filesystem; /var/tmp lies on the ext4 root. */
#define EXT4DIR  "/var/tmp"
#define EXT4FILE "/var/tmp/test-utimensat-ext4-file"
#define EXT4_SUPER_MAGIC 0xEF53

#ifndef UTIME_NOW
#define UTIME_NOW  ((1L << 30) - 1)
#endif
#ifndef UTIME_OMIT
#define UTIME_OMIT ((1L << 30) - 2)
#endif
#ifndef STATX_ATIME
#define STATX_ATIME 0x00000020U
#endif
#ifndef STATX_MTIME
#define STATX_MTIME 0x00000040U
#endif

struct statx_timestamp_abi {
	int64_t tv_sec;
	uint32_t tv_nsec;
	int32_t reserved;
};

struct statx_abi {
	uint32_t stx_mask;
	uint32_t stx_blksize;
	uint64_t stx_attributes;
	uint32_t stx_nlink;
	uint32_t stx_uid;
	uint32_t stx_gid;
	uint16_t stx_mode;
	uint16_t pad0;
	uint64_t stx_ino;
	uint64_t stx_size;
	uint64_t stx_blocks;
	uint64_t stx_attributes_mask;
	struct statx_timestamp_abi stx_atime;
	struct statx_timestamp_abi stx_btime;
	struct statx_timestamp_abi stx_ctime;
	struct statx_timestamp_abi stx_mtime;
	uint32_t stx_rdev_major;
	uint32_t stx_rdev_minor;
	uint32_t stx_dev_major;
	uint32_t stx_dev_minor;
	uint64_t stx_mnt_id;
	uint64_t spare[13];
};

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

/* Record a failed call. The caller must not read the call's output afterwards.
 * Pass the return value and errno saved right after the call, since fprintf
 * may change errno. */
#define FAIL_CALL(name, ret, saved_errno)                                      \
	do {                                                                   \
		fprintf(stderr,                                                \
			"FAIL:%s: %s returned %ld, errno=%d:%s\n",             \
			_current_test, name, (long)(ret), saved_errno,         \
			strerror(saved_errno));                                \
		_test_fail_count++;                                            \
	} while (0)

/* ================================================================ */

void test_set_specific_times(void)
{
	TEST_START("set_specific_times");
	unlink(TMPFILE);
	create_tmpfile(TMPFILE);

	struct timespec ts[2];
	ts[0].tv_sec  = 1000000000; /* atime: 2001-09-09 */
	ts[0].tv_nsec = 123456789;
	ts[1].tv_sec  = 1100000000; /* mtime: 2004-11-10 */
	ts[1].tv_nsec = 987654321;

	CHECK_RET(utimensat(AT_FDCWD, TMPFILE, ts, 0), 0, "utimensat set times");

	struct stat st;
	CHECK_RET(stat(TMPFILE, &st), 0, "stat file");
	CHECK(st.st_atim.tv_sec == 1000000000, "atime sec mismatch");
	CHECK(st.st_atim.tv_nsec == 123456789, "atime nsec mismatch");
	CHECK(st.st_mtim.tv_sec == 1100000000, "mtime sec mismatch");
	CHECK(st.st_mtim.tv_nsec == 987654321, "mtime nsec mismatch");

	unlink(TMPFILE);
	TEST_DONE();
}

void test_ext4_whole_times(void)
{
	TEST_START("ext4_whole_times");
	if (mkdir(EXT4DIR, 01777) != 0 && errno != EEXIST) {
		perror("mkdir " EXT4DIR);
		exit(1);
	}
	unlink(EXT4FILE);
	create_tmpfile(EXT4FILE);

	/* Read an output only after its call succeeded. On failure record the
	 * call, its return value and errno, then remove the file and fail. */
	struct statfs fs;
	long ret = statfs(EXT4FILE, &fs);
	int saved_errno = errno;
	if (ret != 0) {
		FAIL_CALL("statfs ext4 file", ret, saved_errno);
		goto cleanup;
	}
	CHECK(fs.f_type == EXT4_SUPER_MAGIC, "test file is not on ext4");

	/* The mtime lies past 2106, beyond an unsigned 32-bit seconds field,
	 * so ext4 keeps part of it in the inode's extra time field. */
	struct timespec ts[2];
	ts[0].tv_sec  = 1700000000;
	ts[0].tv_nsec = 123456789;
	ts[1].tv_sec  = 5000000000LL;
	ts[1].tv_nsec = 999999999;
	ret = utimensat(AT_FDCWD, EXT4FILE, ts, 0);
	saved_errno = errno;
	if (ret != 0) {
		FAIL_CALL("utimensat ext4 file", ret, saved_errno);
		goto cleanup;
	}

	struct stat st;
	ret = stat(EXT4FILE, &st);
	saved_errno = errno;
	if (ret != 0) {
		FAIL_CALL("stat ext4 file", ret, saved_errno);
		goto cleanup;
	}
	printf("  stat  atime %lld.%09ld mtime %lld.%09ld\n",
	       (long long)st.st_atim.tv_sec, (long)st.st_atim.tv_nsec,
	       (long long)st.st_mtim.tv_sec, (long)st.st_mtim.tv_nsec);
	CHECK(st.st_atim.tv_sec == 1700000000, "stat atime sec mismatch");
	CHECK(st.st_atim.tv_nsec == 123456789, "stat atime nsec mismatch");
	CHECK(st.st_mtim.tv_sec == 5000000000LL, "stat mtime sec mismatch");
	CHECK(st.st_mtim.tv_nsec == 999999999, "stat mtime nsec mismatch");

	struct statx_abi stx;
	memset(&stx, 0, sizeof(stx));
	ret = syscall(SYS_statx, AT_FDCWD, EXT4FILE, 0,
		      STATX_ATIME | STATX_MTIME, &stx);
	saved_errno = errno;
	if (ret != 0) {
		FAIL_CALL("statx ext4 file", ret, saved_errno);
		goto cleanup;
	}
	printf("  statx atime %lld.%09u mtime %lld.%09u\n",
	       (long long)stx.stx_atime.tv_sec, stx.stx_atime.tv_nsec,
	       (long long)stx.stx_mtime.tv_sec, stx.stx_mtime.tv_nsec);
	CHECK((stx.stx_mask & (STATX_ATIME | STATX_MTIME)) ==
		      (STATX_ATIME | STATX_MTIME),
	      "statx mask lacks atime or mtime");
	CHECK(stx.stx_atime.tv_sec == 1700000000, "statx atime sec mismatch");
	CHECK(stx.stx_atime.tv_nsec == 123456789, "statx atime nsec mismatch");
	CHECK(stx.stx_mtime.tv_sec == 5000000000LL, "statx mtime sec mismatch");
	CHECK(stx.stx_mtime.tv_nsec == 999999999, "statx mtime nsec mismatch");

cleanup:
	unlink(EXT4FILE);
	TEST_DONE();
}

void test_utime_now(void)
{
	TEST_START("utime_now");
	unlink(TMPFILE);

	/* Set old timestamp first */
	create_tmpfile(TMPFILE);
	struct timespec old_ts[2];
	old_ts[0].tv_sec  = 1000000000;
	old_ts[0].tv_nsec = 0;
	old_ts[1].tv_sec  = 1000000000;
	old_ts[1].tv_nsec = 0;
	utimensat(AT_FDCWD, TMPFILE, old_ts, 0);

	time_t before = time(NULL);

	struct timespec ts[2];
	ts[0].tv_nsec = UTIME_NOW;
	ts[1].tv_nsec = UTIME_NOW;
	CHECK_RET(utimensat(AT_FDCWD, TMPFILE, ts, 0), 0, "utimensat UTIME_NOW");

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
	struct timespec ts_set[2];
	ts_set[0].tv_sec  = 1000000000;
	ts_set[0].tv_nsec = 0;
	ts_set[1].tv_sec  = 1100000000;
	ts_set[1].tv_nsec = 0;
	utimensat(AT_FDCWD, TMPFILE, ts_set, 0);

	struct timespec ts[2];
	ts[0].tv_nsec = UTIME_OMIT;      /* leave atime */
	ts[1].tv_sec  = 1200000000;      /* update mtime */
	ts[1].tv_nsec = 555000000;
	CHECK_RET(utimensat(AT_FDCWD, TMPFILE, ts, 0), 0, "utimensat OMIT+set");

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
	struct timespec old_ts[2];
	old_ts[0].tv_sec  = 1000000000;
	old_ts[0].tv_nsec = 0;
	old_ts[1].tv_sec  = 1000000000;
	old_ts[1].tv_nsec = 0;
	utimensat(AT_FDCWD, TMPFILE, old_ts, 0);

	time_t before = time(NULL);
	CHECK_RET(utimensat(AT_FDCWD, TMPFILE, NULL, 0), 0, "utimensat NULL");
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
	struct timespec ts[2];
	ts[0].tv_nsec = UTIME_OMIT;
	ts[1].tv_nsec = UTIME_OMIT;
	CHECK_RET(utimensat(AT_FDCWD, NONFILE, ts, 0), 0,
		  "utimensat both OMIT non-existent");
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
	struct timespec ts[2];
	ts[0].tv_sec  = 1500000000;
	ts[0].tv_nsec = 111000000;
	ts[1].tv_sec  = 1600000000;
	ts[1].tv_nsec = 222000000;
	CHECK_RET(utimensat(AT_FDCWD, TMPLINK, ts, AT_SYMLINK_NOFOLLOW), 0,
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
	CHECK(fd >= 0, "open tmpfile");
	write(fd, "x", 1);

	struct timespec ts[2];
	ts[0].tv_sec  = 1700000000;
	ts[0].tv_nsec = 333000000;
	ts[1].tv_sec  = 1800000000;
	ts[1].tv_nsec = 444000000;

	/* NULL path with valid fd → AT_EMPTY_PATH auto-set in kernel */
	CHECK_RET(utimensat(fd, NULL, ts, 0), 0, "utimensat NULL path");

	struct stat st;
	CHECK_RET(fstat(fd, &st), 0, "fstat");
	CHECK(st.st_atim.tv_sec == 1700000000, "atime sec mismatch (empty path)");
	CHECK(st.st_mtim.tv_sec == 1800000000, "mtime sec mismatch (empty path)");

	close(fd);
	unlink(TMPFILE);
	TEST_DONE();
}

void test_invalid_flags(void)
{
	TEST_START("invalid_flags");
	unlink(TMPFILE);
	create_tmpfile(TMPFILE);

	struct timespec ts[2];
	ts[0].tv_nsec = UTIME_NOW;
	ts[1].tv_nsec = UTIME_NOW;

	/* 0xDEAD is not a valid flag combination */
	CHECK_ERR(utimensat(AT_FDCWD, TMPFILE, ts, 0xDEAD), EINVAL,
		  "invalid flags");

	unlink(TMPFILE);
	TEST_DONE();
}

void test_invalid_nsec(void)
{
	TEST_START("invalid_nsec");
	unlink(TMPFILE);
	create_tmpfile(TMPFILE);

	struct timespec ts[2];
	ts[0].tv_sec  = 0;
	ts[0].tv_nsec = 1000000001; /* > 999999999, not UTIME special */
	ts[1].tv_nsec = UTIME_NOW;
	CHECK_ERR(utimensat(AT_FDCWD, TMPFILE, ts, 0), EINVAL,
		  "tv_nsec > 999999999");

	ts[0].tv_nsec = -1; /* negative */
	CHECK_ERR(utimensat(AT_FDCWD, TMPFILE, ts, 0), EINVAL, "tv_nsec < 0");

	unlink(TMPFILE);
	TEST_DONE();
}

void test_enoent(void)
{
	TEST_START("enoent");
	unlink(NONFILE);

	struct timespec ts[2];
	ts[0].tv_nsec = UTIME_NOW;
	ts[1].tv_nsec = UTIME_NOW;
	CHECK_ERR(utimensat(AT_FDCWD, NONFILE, ts, 0), ENOENT, "ENOENT");

	TEST_DONE();
}

void test_ebadf(void)
{
	TEST_START("ebadf");
	int badfd = 9999;

	struct timespec ts[2];
	ts[0].tv_nsec = UTIME_NOW;
	ts[1].tv_nsec = UTIME_NOW;
	CHECK_ERR(utimensat(badfd, "relative/path", ts, 0), EBADF, "EBADF");

	TEST_DONE();
}

/* ================================================================ */

int main(void)
{
	printf("STARRY_GROUPED_TEST_START\n");

	test_set_specific_times();
	test_ext4_whole_times();
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

	printf("UTIMENSAT_TESTS_PASSED\n");
	return 0;
}
