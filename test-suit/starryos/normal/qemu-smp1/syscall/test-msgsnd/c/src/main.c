#include "test_framework.h"

#include <errno.h>
#include <signal.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/msg.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define LOCAL_MSGMAX 8192

#ifndef IPC_NOWAIT
#define IPC_NOWAIT 04000
#endif

struct msgbuf_local
{
    long mtype;
    char mtext[LOCAL_MSGMAX + 1];
};

static volatile sig_atomic_t got_signal;

static void signal_handler(int signo)
{
    (void)signo;
    got_signal = 1;
}

static void sleep_ms(int ms)
{
    struct timespec ts;
    ts.tv_sec = ms / 1000;
    ts.tv_nsec = (long)(ms % 1000) * 1000 * 1000;
    nanosleep(&ts, NULL);
}

static int wait_child(pid_t pid, int timeout_ms)
{
    int elapsed = 0;

    while (elapsed < timeout_ms)
    {
        int status = 0;
        pid_t ret = waitpid(pid, &status, WNOHANG);
        if (ret == pid)
        {
            return WIFEXITED(status) && WEXITSTATUS(status) == 0;
        }
        if (ret < 0)
        {
            return 0;
        }
        sleep_ms(50);
        elapsed += 50;
    }

    kill(pid, SIGKILL);
    (void)waitpid(pid, NULL, 0);
    return 0;
}

static size_t fill_queue(int msqid, size_t msg_qbytes)
{
    struct msgbuf_local msg;
    size_t chunk = LOCAL_MSGMAX;

    if (msg_qbytes / 2 < chunk)
    {
        chunk = msg_qbytes / 2;
    }
    if (chunk == 0)
    {
        chunk = 1;
    }

    memset(msg.mtext, 'A', chunk);
    msg.mtype = 1;

    size_t total = 0;
    while (total + chunk <= msg_qbytes)
    {
        if (msgsnd(msqid, &msg, chunk, IPC_NOWAIT) != 0)
        {
            break;
        }
        total += chunk;
    }

    return total;
}

static int send_sized_message(int msqid, long mtype, size_t msgsz, int flags)
{
    struct msgbuf_local msg;

    msg.mtype = mtype;
    memset(msg.mtext, (int)('A' + (mtype % 26)), msgsz);

    return msgsnd(msqid, &msg, msgsz, flags);
}

static int set_queue_bytes(int msqid, unsigned long qbytes)
{
    struct msqid_ds info;

    if (msgctl(msqid, IPC_STAT, &info) != 0)
    {
        return -1;
    }
    info.msg_qbytes = qbytes;
    return msgctl(msqid, IPC_SET, &info);
}

static int fill_to_exact_one_byte_messages(int msqid, int count)
{
    for (int i = 0; i < count; i++)
    {
        if (send_sized_message(msqid, 1, 1, IPC_NOWAIT) != 0)
        {
            return -1;
        }
    }
    return 0;
}

static int fill_to_full(int msqid)
{
    struct msgbuf_local msg;

    msg.mtype = 1;
    msg.mtext[0] = 'Z';

    errno = 0;
    while (msgsnd(msqid, &msg, 1, IPC_NOWAIT) == 0)
    {
    }

    return errno == EAGAIN;
}

int main(void)
{
    TEST_START("msgsnd semantic checks");

    int msqid = msgget(IPC_PRIVATE, 0600);
    CHECK(msqid >= 0, "msgget IPC_PRIVATE creates queue for msgsnd test");
    if (msqid < 0)
    {
        TEST_DONE();
    }

    struct msgbuf_local msg;
    msg.mtype = 0;
    msg.mtext[0] = 'X';
    CHECK_ERR(msgsnd(msqid, &msg, 1, 0), EINVAL, "mtype <= 0 => EINVAL");

    msg.mtype = 1;
    memset(msg.mtext, 'B', LOCAL_MSGMAX + 1);
    CHECK_ERR(msgsnd(msqid, &msg, LOCAL_MSGMAX + 1, 0), EINVAL,
              "msgsz > MSGMAX => EINVAL");

    struct msqid_ds info;
    CHECK_RET(msgctl(msqid, IPC_STAT, &info), 0, "msgctl IPC_STAT returns queue info");

    size_t total = fill_queue(msqid, (size_t)info.msg_qbytes);
    CHECK(total > 0, "filled queue to trigger EAGAIN");
    CHECK(fill_to_full(msqid), "queue reaches full capacity");

    msg.mtype = 2;
    msg.mtext[0] = 'C';
    CHECK_ERR(msgsnd(msqid, &msg, 1, IPC_NOWAIT), EAGAIN,
              "queue full + IPC_NOWAIT => EAGAIN");

    pid_t pid = fork();
    if (pid == 0)
    {
        msg.mtype = 3;
        msg.mtext[0] = 'D';
        if (msgsnd(msqid, &msg, 1, 0) == 0)
        {
            _exit(0);
        }
        _exit(1);
    }
    sleep_ms(100);
    struct msgbuf_local recv_buf;
    CHECK(msgrcv(msqid, &recv_buf, LOCAL_MSGMAX, 0, 0) >= 0,
          "msgrcv frees space for blocking msgsnd");
    CHECK(wait_child(pid, 2000), "blocking msgsnd unblocks after space freed");


    int wake_msqid = msgget(IPC_PRIVATE, 0600);
    CHECK(wake_msqid >= 0, "msgget creates queue for multi-sender wake test");
    if (wake_msqid >= 0)
    {
        CHECK_RET(set_queue_bytes(wake_msqid, 16), 0,
                  "IPC_SET lowers qbytes for multi-sender wake test");
        CHECK_RET(fill_to_exact_one_byte_messages(wake_msqid, 16), 0,
                  "fill queue with one-byte messages for multi-sender wake test");

        pid_t long_pid = fork();
        if (long_pid == 0)
        {
            errno = 0;
            if (send_sized_message(wake_msqid, 2, 8, 0) == 0 || errno == EIDRM)
            {
                _exit(0);
            }
            _exit(1);
        }
        sleep_ms(100);

        pid_t short_pid = fork();
        if (short_pid == 0)
        {
            if (send_sized_message(wake_msqid, 3, 1, 0) == 0)
            {
                _exit(0);
            }
            _exit(1);
        }
        sleep_ms(100);

        CHECK(msgrcv(wake_msqid, &recv_buf, LOCAL_MSGMAX, 0, 0) == 1,
              "msgrcv frees only enough space for short sender");
        CHECK(wait_child(short_pid, 2000),
              "short blocking msgsnd wakes even if earlier long sender cannot fit");
        CHECK_RET(msgctl(wake_msqid, IPC_RMID, NULL), 0,
                  "IPC_RMID cleans multi-sender wake queue");
        CHECK(wait_child(long_pid, 2000),
              "long blocking msgsnd exits after multi-sender queue removal");
    }

    int grow_msqid = msgget(IPC_PRIVATE, 0600);
    CHECK(grow_msqid >= 0, "msgget creates queue for IPC_SET wake test");
    if (grow_msqid >= 0)
    {
        CHECK_RET(set_queue_bytes(grow_msqid, 1), 0,
                  "IPC_SET lowers qbytes for grow wake test");
        CHECK_RET(send_sized_message(grow_msqid, 1, 1, IPC_NOWAIT), 0,
                  "fill one-byte queue before grow wake test");

        pid_t grow_pid = fork();
        if (grow_pid == 0)
        {
            if (send_sized_message(grow_msqid, 2, 1, 0) == 0)
            {
                _exit(0);
            }
            _exit(1);
        }
        sleep_ms(100);
        CHECK_RET(set_queue_bytes(grow_msqid, 2), 0,
                  "IPC_SET increases qbytes while sender waits");
        CHECK(wait_child(grow_pid, 2000),
              "blocking msgsnd wakes after IPC_SET increases qbytes");
        CHECK_RET(msgctl(grow_msqid, IPC_RMID, NULL), 0,
                  "IPC_RMID cleans IPC_SET grow wake queue");
    }

    int intr_msqid = msgget(IPC_PRIVATE, 0600);
    CHECK(intr_msqid >= 0, "msgget creates queue for msgsnd EINTR test");
    if (intr_msqid >= 0)
    {
        CHECK_RET(set_queue_bytes(intr_msqid, 1), 0,
                  "IPC_SET lowers qbytes for msgsnd EINTR test");
        CHECK_RET(send_sized_message(intr_msqid, 1, 1, IPC_NOWAIT), 0,
                  "fill queue before msgsnd EINTR test");

        pid_t intr_pid = fork();
        if (intr_pid == 0)
        {
            struct sigaction sa;
            memset(&sa, 0, sizeof(sa));
            sa.sa_handler = signal_handler;
            sa.sa_flags = SA_RESTART;
            sigemptyset(&sa.sa_mask);
            if (sigaction(SIGUSR1, &sa, NULL) != 0)
            {
                _exit(1);
            }

            errno = 0;
            got_signal = 0;
            if (send_sized_message(intr_msqid, 2, 1, 0) == -1 && errno == EINTR && got_signal)
            {
                _exit(0);
            }
            _exit(1);
        }
        sleep_ms(100);
        CHECK_RET(kill(intr_pid, SIGUSR1), 0, "signal blocking msgsnd with SA_RESTART");
        CHECK(wait_child(intr_pid, 2000), "blocking msgsnd returns EINTR despite SA_RESTART");
        CHECK_RET(msgctl(intr_msqid, IPC_RMID, NULL), 0,
                  "IPC_RMID cleans msgsnd EINTR queue");
    }

    fill_queue(msqid, (size_t)info.msg_qbytes);
    CHECK(fill_to_full(msqid), "queue full before IPC_RMID test");

    pid = fork();
    if (pid == 0)
    {
        msg.mtype = 4;
        msg.mtext[0] = 'E';
        errno = 0;
        if (msgsnd(msqid, &msg, 1, 0) == -1 && errno == EIDRM)
        {
            _exit(0);
        }
        _exit(1);
    }
    sleep_ms(100);
    CHECK_RET(msgctl(msqid, IPC_RMID, NULL), 0, "msgctl IPC_RMID removes queue");
    CHECK(wait_child(pid, 2000), "blocking msgsnd returns EIDRM after IPC_RMID");

    TEST_DONE();
}
