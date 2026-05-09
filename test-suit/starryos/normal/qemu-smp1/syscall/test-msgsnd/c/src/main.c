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
