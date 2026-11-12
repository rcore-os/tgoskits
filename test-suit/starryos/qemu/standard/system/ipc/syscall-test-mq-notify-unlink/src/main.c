/*
 * mq-notify-unlink.c - Regression test for POSIX message-queue gaps fixed
 * against Linux ipc/mqueue.c.
 *
 * Minimal-ABI coverage of behaviours that regress on the pre-fix StarryOS
 * implementation:
 *
 *   1. mq_notify registration is cleared on EVERY fd-closing path, not only
 *      an explicit close(2): a dup2() replacement and a close_range() must
 *      each release the registration (Linux mqueue_flush_file runs from
 *      filp_flush on all of them). Proven by re-registering from the same
 *      process afterwards succeeding rather than returning EBUSY.
 *
 *   2. A queue unlinked while a descriptor is still open stays fully
 *      functional (the Arc/inode outlives the name), and its name is gone.
 *
 *   3. Basic send/receive/getattr priority ordering, as a smoke floor.
 *
 * Runs as root, so DAC/sticky checks pass trivially; this test targets the
 * structural notify-cleanup and unlink-while-open behaviour rather than the
 * permission gates.
 */

#include "test_framework.h"

#include <fcntl.h>
#include <mqueue.h>
#include <signal.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

static const char *MQ_NAME = "/starry_mq_regr";

static void cleanup(void)
{
    mq_unlink(MQ_NAME);
}

static mqd_t open_fresh(void)
{
    mq_unlink(MQ_NAME);
    struct mq_attr attr = {0};
    attr.mq_maxmsg = 4;
    attr.mq_msgsize = 64;
    return mq_open(MQ_NAME, O_CREAT | O_RDWR, 0600, &attr);
}

/* mq_notify must be released when the registering fd is replaced by dup2(). */
static void test_notify_cleared_on_dup2(void)
{
    mqd_t mq = open_fresh();
    CHECK(mq != (mqd_t)-1, "mq_open for dup2 notify test");
    if (mq == (mqd_t)-1)
        return;

    struct sigevent sev = {0};
    sev.sigev_notify = SIGEV_SIGNAL;
    sev.sigev_signo = SIGUSR1;
    CHECK_RET(mq_notify(mq, &sev), 0, "mq_notify registers");

    /* A second registration on the live queue must be refused. */
    CHECK_ERR(mq_notify(mq, &sev), EBUSY, "second mq_notify EBUSY while held");

    /* Replace the registering fd with a scratch fd via dup2(): this closes the
     * mq fd through the dup2 path (not close(2)), which must run the flush
     * hook and drop the registration. */
    int scratch = open("/dev/null", O_RDONLY);
    CHECK(scratch >= 0, "open scratch fd");
    CHECK(dup2(scratch, (int)mq) == (int)mq, "dup2 replaces mq fd");
    close(scratch);

    /* Re-open and re-register: succeeds iff the dup2 close cleared the notify. */
    mqd_t mq2 = mq_open(MQ_NAME, O_RDWR);
    CHECK(mq2 != (mqd_t)-1, "reopen queue after dup2");
    if (mq2 != (mqd_t)-1)
    {
        CHECK_RET(mq_notify(mq2, &sev), 0,
                  "mq_notify re-registers after dup2 cleared it");
        mq_close(mq2);
    }
    cleanup();
}

/* mq_notify must also be released via close_range(). */
static void test_notify_cleared_on_close_range(void)
{
    mqd_t mq = open_fresh();
    CHECK(mq != (mqd_t)-1, "mq_open for close_range notify test");
    if (mq == (mqd_t)-1)
        return;

    struct sigevent sev = {0};
    sev.sigev_notify = SIGEV_NONE;
    CHECK_RET(mq_notify(mq, &sev), 0, "mq_notify(SIGEV_NONE) registers");

#ifdef __NR_close_range
    long r = syscall(__NR_close_range, (unsigned)mq, (unsigned)mq, 0u);
    CHECK(r == 0, "close_range closes mq fd");
#else
    mq_close(mq);
#endif

    mqd_t mq2 = mq_open(MQ_NAME, O_RDWR);
    CHECK(mq2 != (mqd_t)-1, "reopen queue after close_range");
    if (mq2 != (mqd_t)-1)
    {
        CHECK_RET(mq_notify(mq2, &sev), 0,
                  "mq_notify re-registers after close_range cleared it");
        mq_close(mq2);
    }
    cleanup();
}

/* A queue unlinked while open stays usable; its name disappears. */
static void test_unlink_while_open(void)
{
    mqd_t mq = open_fresh();
    CHECK(mq != (mqd_t)-1, "mq_open for unlink-while-open test");
    if (mq == (mqd_t)-1)
        return;

    CHECK_RET(mq_unlink(MQ_NAME), 0, "mq_unlink while descriptor open");

    /* Name is gone: a plain open must fail with ENOENT. */
    CHECK_ERR(mq_open(MQ_NAME, O_RDWR), ENOENT, "reopen unlinked name ENOENT");

    /* The still-open descriptor keeps working. */
    const char *payload = "still-alive";
    CHECK_RET(mq_send(mq, payload, strlen(payload), 0), 0,
              "mq_send on unlinked-but-open queue");

    char buf[64] = {0};
    unsigned prio = 99;
    ssize_t n = mq_receive(mq, buf, sizeof(buf), &prio);
    CHECK(n == (ssize_t)strlen(payload), "mq_receive returns payload length");
    CHECK(memcmp(buf, payload, strlen(payload)) == 0, "payload round-trips");
    CHECK(prio == 0, "priority round-trips");

    mq_close(mq);
}

/* Smoke floor: priority ordering + getattr counts. */
static void test_priority_and_getattr(void)
{
    mqd_t mq = open_fresh();
    CHECK(mq != (mqd_t)-1, "mq_open for priority test");
    if (mq == (mqd_t)-1)
        return;

    CHECK_RET(mq_send(mq, "lo", 2, 1), 0, "send prio 1");
    CHECK_RET(mq_send(mq, "hi", 2, 5), 0, "send prio 5");
    CHECK_RET(mq_send(mq, "mid", 3, 3), 0, "send prio 3");

    struct mq_attr attr = {0};
    CHECK_RET(mq_getattr(mq, &attr), 0, "mq_getattr");
    CHECK(attr.mq_curmsgs == 3, "getattr reports 3 queued");
    CHECK(attr.mq_maxmsg == 4, "getattr reports maxmsg");

    char buf[64];
    unsigned prio = 0;
    ssize_t n = mq_receive(mq, buf, sizeof(buf), &prio);
    CHECK(n == 2 && prio == 5, "highest priority delivered first");

    mq_close(mq);
    cleanup();
}

int main(void)
{
    TEST_START("mq notify-cleanup / unlink-while-open regression");

    test_notify_cleared_on_dup2();
    test_notify_cleared_on_close_range();
    test_unlink_while_open();
    test_priority_and_getattr();

    TEST_DONE();
}
