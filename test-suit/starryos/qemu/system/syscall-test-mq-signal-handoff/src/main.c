/*
 * mq-signal-handoff.c - Deterministic regression for POSIX message-queue
 * signal-interruption and send->receive handoff / notification semantics,
 * against Linux ipc/mqueue.c.
 *
 * Covers the behaviours a count-only or wrong-SA_RESTART implementation
 * regresses on:
 *
 *   1. A signal with a non-SA_RESTART handler interrupts a blocked
 *      mq_timedreceive with EINTR (ipc/mqueue.c wq_sleep returns
 *      -ERESTARTSYS, which the no-SA_RESTART return path turns into EINTR).
 *   2. Same for a blocked mq_timedsend on a full queue.
 *   3. A signal with an SA_RESTART handler does NOT surface EINTR: the
 *      syscall is restarted and later completes with a real message. mq
 *      uses an absolute CLOCK_REALTIME timeout, so the restart is safe and
 *      Linux keeps mq_timedsend/mq_timedreceive OUT of the never-restart set
 *      (only the System V msgsnd/msgrcv are never restarted, signal(7)).
 *   4. Sending to an empty queue with a receiver already blocked hands the
 *      message straight to that receiver AND suppresses the empty->non-empty
 *      notification (Linux pipelined_send skips __do_notify when a waiter is
 *      served).
 *   5. Sending to an empty queue with NO receiver blocked fires the
 *      registered notification exactly once (the empty->non-empty edge).
 *   6. A receiver that blocks and then times out must not strand a deferred
 *      notification: after it leaves, a later send still fires notify. Run in
 *      a loop to exercise the timeout/handoff race window repeatedly.
 *
 * Runs as root. Deterministic handshake: the worker publishes an "armed"
 * flag immediately before the blocking call and the driver waits for it plus
 * a short settle before acting, so the worker is parked in the syscall.
 */

#include "test_framework.h"

#include <fcntl.h>
#include <mqueue.h>
#include <pthread.h>
#include <signal.h>
#include <time.h>
#include <unistd.h>

static const char *MQ_NAME = "/starry_mq_sighand";

static volatile sig_atomic_t g_notify_hits;
static volatile sig_atomic_t g_usr1_hits;

static void notify_handler(int sig)
{
    (void)sig;
    g_notify_hits++;
}

static void usr1_handler(int sig)
{
    (void)sig;
    g_usr1_hits++;
}

/* Install `sig` handler; with_restart selects SA_RESTART. */
static void install(int sig, void (*fn)(int), int with_restart)
{
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = fn;
    sa.sa_flags = with_restart ? SA_RESTART : 0;
    sigemptyset(&sa.sa_mask);
    sigaction(sig, &sa, NULL);
}

static mqd_t open_fresh(long maxmsg)
{
    mq_unlink(MQ_NAME);
    struct mq_attr attr = {0};
    attr.mq_maxmsg = maxmsg;
    attr.mq_msgsize = 64;
    return mq_open(MQ_NAME, O_CREAT | O_RDWR, 0600, &attr);
}

static struct timespec deadline_in(int secs)
{
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    ts.tv_sec += secs;
    return ts;
}

/* ---- shared worker state ---- */
static mqd_t g_mq;
static volatile sig_atomic_t g_armed;
static long g_ret;
static int g_errno;
static int g_deadline_secs;

static void *recv_worker(void *arg)
{
    (void)arg;
    char buf[64];
    unsigned prio = 0;
    struct timespec ts = deadline_in(g_deadline_secs);
    g_armed = 1;
    errno = 0;
    g_ret = mq_timedreceive(g_mq, buf, sizeof(buf), &prio, &ts);
    g_errno = errno;
    return NULL;
}

static void *send_worker(void *arg)
{
    (void)arg;
    struct timespec ts = deadline_in(g_deadline_secs);
    g_armed = 1;
    errno = 0;
    g_ret = mq_timedsend(g_mq, "blocked", 7, 0, &ts);
    g_errno = errno;
    return NULL;
}

/* Spin until the worker armed, then let it actually park in the syscall. */
static void wait_parked(void)
{
    while (!g_armed)
        sched_yield();
    usleep(150 * 1000);
}

/* 1. non-SA_RESTART signal interrupts a blocked mq_timedreceive with EINTR. */
static void test_recv_eintr(void)
{
    g_mq = open_fresh(4);
    CHECK(g_mq != (mqd_t)-1, "mq_open for recv-EINTR");
    if (g_mq == (mqd_t)-1)
        return;

    install(SIGUSR1, usr1_handler, 0);
    g_armed = 0;
    g_deadline_secs = 30;
    pthread_t th;
    pthread_create(&th, NULL, recv_worker, NULL);
    wait_parked();
    pthread_kill(th, SIGUSR1);
    pthread_join(th, NULL);

    CHECK(g_ret == -1 && g_errno == EINTR,
          "mq_timedreceive interrupted by non-SA_RESTART signal returns EINTR");
    mq_close(g_mq);
    mq_unlink(MQ_NAME);
}

/* 2. non-SA_RESTART signal interrupts a blocked mq_timedsend with EINTR. */
static void test_send_eintr(void)
{
    g_mq = open_fresh(2);
    CHECK(g_mq != (mqd_t)-1, "mq_open for send-EINTR");
    if (g_mq == (mqd_t)-1)
        return;
    /* Fill the queue so the next send blocks. */
    CHECK_RET(mq_send(g_mq, "a", 1, 0), 0, "prime full queue #1");
    CHECK_RET(mq_send(g_mq, "b", 1, 0), 0, "prime full queue #2");

    install(SIGUSR1, usr1_handler, 0);
    g_armed = 0;
    g_deadline_secs = 30;
    pthread_t th;
    pthread_create(&th, NULL, send_worker, NULL);
    wait_parked();
    pthread_kill(th, SIGUSR1);
    pthread_join(th, NULL);

    CHECK(g_ret == -1 && g_errno == EINTR,
          "mq_timedsend on full queue interrupted by non-SA_RESTART returns EINTR");
    mq_close(g_mq);
    mq_unlink(MQ_NAME);
}

/* 3. SA_RESTART signal does NOT surface EINTR: the blocked receive restarts
 *    and later completes with the message sent after the signal. */
static void test_recv_sa_restart(void)
{
    g_mq = open_fresh(4);
    CHECK(g_mq != (mqd_t)-1, "mq_open for SA_RESTART");
    if (g_mq == (mqd_t)-1)
        return;

    install(SIGUSR1, usr1_handler, 1 /* SA_RESTART */);
    g_usr1_hits = 0;
    g_armed = 0;
    g_deadline_secs = 30;
    pthread_t th;
    pthread_create(&th, NULL, recv_worker, NULL);
    wait_parked();
    pthread_kill(th, SIGUSR1);
    usleep(150 * 1000); /* let the handler run and the syscall restart */
    CHECK_RET(mq_send(g_mq, "restarted", 9, 0), 0, "send after SA_RESTART signal");
    pthread_join(th, NULL);

    CHECK(g_usr1_hits >= 1, "SA_RESTART handler actually ran");
    CHECK(g_ret == 9 && g_errno == 0,
          "SA_RESTART mq_timedreceive restarts and returns the message (no EINTR)");
    mq_close(g_mq);
    mq_unlink(MQ_NAME);
}

/* 4. Receiver blocked + send: message handed to the receiver, and the
 *    empty->non-empty notification is suppressed (a waiter consumed it). */
static void test_handoff_suppresses_notify(void)
{
    g_mq = open_fresh(4);
    CHECK(g_mq != (mqd_t)-1, "mq_open for handoff-suppress");
    if (g_mq == (mqd_t)-1)
        return;

    install(SIGUSR2, notify_handler, 0);
    struct sigevent sev = {0};
    sev.sigev_notify = SIGEV_SIGNAL;
    sev.sigev_signo = SIGUSR2;
    CHECK_RET(mq_notify(g_mq, &sev), 0, "mq_notify registers for handoff test");

    g_notify_hits = 0;
    g_armed = 0;
    g_deadline_secs = 30;
    pthread_t th;
    pthread_create(&th, NULL, recv_worker, NULL);
    wait_parked();
    CHECK_RET(mq_send(g_mq, "direct", 6, 0), 0, "send to a queue with a blocked receiver");
    pthread_join(th, NULL);

    CHECK(g_ret == 6 && g_errno == 0, "blocked receiver got the handed-off message");
    usleep(150 * 1000); /* give any errant notification time to arrive */
    CHECK(g_notify_hits == 0,
          "notification suppressed when a receiver consumed the message");
    mq_close(g_mq);
    mq_unlink(MQ_NAME);
}

/* 5. Send to an empty queue with no receiver fires the notification once. */
static void test_notify_edge_fires(void)
{
    g_mq = open_fresh(4);
    CHECK(g_mq != (mqd_t)-1, "mq_open for notify-edge");
    if (g_mq == (mqd_t)-1)
        return;

    install(SIGUSR2, notify_handler, 0);
    struct sigevent sev = {0};
    sev.sigev_notify = SIGEV_SIGNAL;
    sev.sigev_signo = SIGUSR2;
    CHECK_RET(mq_notify(g_mq, &sev), 0, "mq_notify registers for edge test");

    g_notify_hits = 0;
    CHECK_RET(mq_send(g_mq, "edge", 4, 0), 0, "send to empty queue with no receiver");
    usleep(150 * 1000);
    CHECK(g_notify_hits == 1, "empty->non-empty edge fired notification exactly once");

    /* Single-shot: a second send must NOT re-fire the (now consumed) notify. */
    CHECK_RET(mq_send(g_mq, "edge2", 5, 0), 0, "second send (queue already non-empty)");
    usleep(150 * 1000);
    CHECK(g_notify_hits == 1, "notification is single-shot (not re-fired)");

    char buf[64];
    (void)mq_receive(g_mq, buf, sizeof(buf), NULL);
    (void)mq_receive(g_mq, buf, sizeof(buf), NULL);
    mq_close(g_mq);
    mq_unlink(MQ_NAME);
}

/* 6. A receiver that times out must not strand a deferred notification:
 *    after it leaves, a later send still fires notify. Looped to exercise the
 *    timeout/handoff race window; every iteration must deliver notify. */
static void test_timeout_then_notify(void)
{
    int rounds = 20;
    int fires = 0;
    int reg_ok = 1;
    for (int i = 0; i < rounds; i++)
    {
        g_mq = open_fresh(4);
        if (g_mq == (mqd_t)-1)
        {
            reg_ok = 0;
            break;
        }
        install(SIGUSR2, notify_handler, 0);
        struct sigevent sev = {0};
        sev.sigev_notify = SIGEV_SIGNAL;
        sev.sigev_signo = SIGUSR2;
        if (mq_notify(g_mq, &sev) != 0)
            reg_ok = 0;

        /* Receiver blocks briefly then times out with an empty queue.
         * The driver thread itself does the timed receive - no helper needed,
         * the point is that the receiver parks and leaves via the timeout
         * path (guard drop), not that it runs concurrently with the send. */
        g_notify_hits = 0;
        struct timespec ts = deadline_in(0);
        ts.tv_nsec += 80 * 1000 * 1000; /* ~80ms deadline */
        if (ts.tv_nsec >= 1000000000L)
        {
            ts.tv_sec += 1;
            ts.tv_nsec -= 1000000000L;
        }
        char buf[64];
        unsigned prio = 0;
        errno = 0;
        ssize_t r = mq_timedreceive(g_mq, buf, sizeof(buf), &prio, &ts);
        int e = errno;
        /* Empty queue, so this must time out. */
        if (!(r == -1 && e == ETIMEDOUT))
            reg_ok = 0;

        /* Now, with the receiver gone, a send must fire the notification. */
        if (mq_send(g_mq, "late", 4, 0) == 0)
        {
            usleep(20 * 1000);
            if (g_notify_hits >= 1)
                fires++;
        }
        char drain[64];
        (void)mq_receive(g_mq, drain, sizeof(drain), NULL);
        mq_close(g_mq);
        mq_unlink(MQ_NAME);
    }
    CHECK(reg_ok, "timeout/notify loop setup consistent (all timed out, no reg error)");
    CHECK(fires == rounds,
          "every post-timeout send fired notify (no stranded deferred notification)");
}

int main(void)
{
    TEST_START("mq signal-interruption / send-receive handoff regression");

    test_recv_eintr();
    test_send_eintr();
    test_recv_sa_restart();
    test_handoff_suppresses_notify();
    test_notify_edge_fires();
    test_timeout_then_notify();

    TEST_DONE();
}
