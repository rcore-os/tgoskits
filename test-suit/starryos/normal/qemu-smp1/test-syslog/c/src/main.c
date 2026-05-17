#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"
#include <errno.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef SYS_syslog
#define SYS_syslog 116
#endif

#define SYSLOG_ACTION_CLOSE 0
#define SYSLOG_ACTION_OPEN 1
#define SYSLOG_ACTION_READ 2
#define SYSLOG_ACTION_READ_ALL 3
#define SYSLOG_ACTION_READ_CLEAR 4
#define SYSLOG_ACTION_CLEAR 5
#define SYSLOG_ACTION_CONSOLE_OFF 6
#define SYSLOG_ACTION_CONSOLE_ON 7
#define SYSLOG_ACTION_CONSOLE_LEVEL 8
#define SYSLOG_ACTION_SIZE_UNREAD 9
#define SYSLOG_ACTION_SIZE_BUFFER 10

#ifndef SYS_setresuid
#define SYS_setresuid 147
#endif

static int run_in_child(void (*func)(void)) {
    pid_t pid = fork();
    if (pid < 0)
        return 0;
    if (pid == 0) {
        func();
        _exit(__fail > 0 ? 1 : 0);
    }
    int status = 0;
    pid_t waited;
    do {
        waited = waitpid(pid, &status, 0);
    } while (waited == -1 && errno == EINTR);
    if (waited == -1)
        return 0;
    return WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

static void child_syslog_eperm(void) {
    char tmp[16];
    syscall(SYS_setresuid, 1000, 1000, 1000);

    CHECK_ERR(syscall(SYS_syslog, SYSLOG_ACTION_READ, tmp, (int)sizeof(tmp)),
              EPERM, "non-root READ returns EPERM");
    CHECK_ERR(syscall(SYS_syslog, SYSLOG_ACTION_READ_ALL, tmp, (int)sizeof(tmp)),
              EPERM, "non-root READ_ALL returns EPERM");
    CHECK_ERR(syscall(SYS_syslog, SYSLOG_ACTION_READ_CLEAR, tmp, (int)sizeof(tmp)),
              EPERM, "non-root READ_CLEAR returns EPERM");
    CHECK_ERR(syscall(SYS_syslog, SYSLOG_ACTION_CLEAR, NULL, 0),
              EPERM, "non-root CLEAR returns EPERM");
    CHECK_ERR(syscall(SYS_syslog, SYSLOG_ACTION_CONSOLE_OFF, NULL, 0),
              EPERM, "non-root CONSOLE_OFF returns EPERM");
    CHECK_ERR(syscall(SYS_syslog, SYSLOG_ACTION_CONSOLE_ON, NULL, 0),
              EPERM, "non-root CONSOLE_ON returns EPERM");
    CHECK_ERR(syscall(SYS_syslog, SYSLOG_ACTION_CONSOLE_LEVEL, NULL, 3),
              EPERM, "non-root CONSOLE_LEVEL returns EPERM");
    CHECK_ERR(syscall(SYS_syslog, SYSLOG_ACTION_SIZE_UNREAD, NULL, 0),
              EPERM, "non-root SIZE_UNREAD returns EPERM");

    long sb = syscall(SYS_syslog, SYSLOG_ACTION_SIZE_BUFFER, NULL, 0);
    CHECK(sb > 0, "non-root SIZE_BUFFER succeeds (no privilege required)");
}

int main(void) {
    TEST_START("syslog");

    char buf[128];
    char buf2[128];
    char buf3[128];
    memset(buf, 0xA5, sizeof(buf));
    memset(buf2, 0x5A, sizeof(buf2));
    memset(buf3, 0x3C, sizeof(buf3));

    CHECK_RET(syscall(SYS_syslog, SYSLOG_ACTION_OPEN, NULL, 0), 0,
              "OPEN returns 0");
    CHECK_RET(syscall(SYS_syslog, SYSLOG_ACTION_CLOSE, NULL, 0), 0,
              "CLOSE returns 0");

    long size_buffer = syscall(SYS_syslog, SYSLOG_ACTION_SIZE_BUFFER, NULL, 0);
    CHECK(size_buffer > 0, "SIZE_BUFFER returns a positive capacity");

    long size_unread = syscall(SYS_syslog, SYSLOG_ACTION_SIZE_UNREAD, NULL, 0);
    CHECK(size_unread >= 0, "SIZE_UNREAD is non-negative");
    CHECK(size_unread <= size_buffer, "SIZE_UNREAD does not exceed SIZE_BUFFER");

    long read_all = syscall(SYS_syslog, SYSLOG_ACTION_READ_ALL, buf, (int)sizeof(buf));
    CHECK(read_all >= 0, "READ_ALL returns a non-negative length");
    CHECK(read_all <= (long)sizeof(buf), "READ_ALL respects destination buffer length");
    CHECK(read_all <= size_unread, "READ_ALL does not exceed unread bytes");
    if (read_all == 0) {
        CHECK(memcmp(buf, (unsigned char[128]){ [0 ... 127] = 0xA5 }, sizeof(buf)) == 0,
              "READ_ALL leaves buffer unchanged when nothing is copied");
    }

    long size_unread_after_read_all = syscall(SYS_syslog, SYSLOG_ACTION_SIZE_UNREAD, NULL, 0);
    CHECK(size_unread_after_read_all == size_unread,
          "READ_ALL does not consume unread bytes");

    long read = syscall(SYS_syslog, SYSLOG_ACTION_READ, buf2, (int)sizeof(buf2));
    CHECK(read >= 0, "READ returns a non-negative length");
    CHECK(read <= (long)sizeof(buf2), "READ respects destination buffer length");
    CHECK(read == read_all, "READ consumes the same unread bytes exposed by READ_ALL");
    if (read > 0) {
        CHECK(memcmp(buf, buf2, (size_t)read) == 0,
              "READ returns the same prefix previously observed by READ_ALL");
    }

    long size_unread_after_read = syscall(SYS_syslog, SYSLOG_ACTION_SIZE_UNREAD, NULL, 0);
    CHECK(size_unread_after_read == size_unread - read,
          "READ consumes unread bytes");

    CHECK_RET(syscall(SYS_syslog, SYSLOG_ACTION_CONSOLE_OFF, NULL, 0), 0,
              "CONSOLE_OFF returns 0");
    CHECK_RET(syscall(SYS_syslog, SYSLOG_ACTION_CONSOLE_ON, NULL, 0), 0,
              "CONSOLE_ON returns 0");
    CHECK_RET(syscall(SYS_syslog, SYSLOG_ACTION_CONSOLE_LEVEL, NULL, 1), 7,
              "CONSOLE_LEVEL returns the previous level");
    CHECK_RET(syscall(SYS_syslog, SYSLOG_ACTION_CONSOLE_LEVEL, NULL, 7), 1,
              "CONSOLE_LEVEL updates and reports the old level");
    CHECK_ERR(syscall(SYS_syslog, SYSLOG_ACTION_CONSOLE_LEVEL, NULL, 0), EINVAL,
              "CONSOLE_LEVEL rejects level 0 with EINVAL");
    CHECK_ERR(syscall(SYS_syslog, SYSLOG_ACTION_CONSOLE_LEVEL, NULL, 9), EINVAL,
              "CONSOLE_LEVEL rejects level 9 with EINVAL");

    long read_clear = syscall(SYS_syslog, SYSLOG_ACTION_READ_CLEAR, buf3, (int)sizeof(buf3));
    CHECK(read_clear >= 0, "READ_CLEAR returns a non-negative length");
    CHECK(read_clear <= size_unread_after_read,
          "READ_CLEAR does not exceed remaining unread bytes");

    long size_unread_after_read_clear = syscall(SYS_syslog, SYSLOG_ACTION_SIZE_UNREAD, NULL, 0);
    CHECK(size_unread_after_read_clear == 0,
          "READ_CLEAR clears unread bytes after copying");

    CHECK_RET(syscall(SYS_syslog, SYSLOG_ACTION_CLEAR, NULL, 0), 0,
              "CLEAR returns 0");
    CHECK_RET(syscall(SYS_syslog, SYSLOG_ACTION_READ_CLEAR, buf, (int)sizeof(buf)), 0,
              "READ_CLEAR returns 0 after CLEAR on empty buffer");

    CHECK_ERR(syscall(SYS_syslog, 99, NULL, 0), EINVAL,
              "unknown action returns EINVAL");

    CHECK(run_in_child(child_syslog_eperm),
          "non-root privilege denial (child process)");

    TEST_DONE();
}
