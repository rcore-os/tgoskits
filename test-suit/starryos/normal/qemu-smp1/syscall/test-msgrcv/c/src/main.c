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

#ifndef MSG_COPY
#define MSG_COPY 040000
#endif
#ifndef MSG_EXCEPT
#define MSG_EXCEPT 020000
#endif
#ifndef MSG_NOERROR
#define MSG_NOERROR 010000
#endif
#ifndef IPC_NOWAIT
#define IPC_NOWAIT 04000
#endif

struct msgbuf_local
{
    long mtype;
    char mtext[128];
};

static volatile sig_atomic_t got_signal;

static void signal_handler(int signo)
{
    (void)signo;
    got_signal = 1;
}

static key_t make_key(void)
{
    key_t key = (key_t)(((unsigned int)getpid() << 8) ^ 0x4d534753U);
    if (key == IPC_PRIVATE)
    {
        key ^= 0x33;
    }
    return key;
}

static void remove_queue_if_exists(key_t key)
{
    int id = msgget(key, 0600);
    if (id >= 0)
    {
        (void)msgctl(id, IPC_RMID, NULL);
    }
}

static int send_message(int msqid, long mtype, const char *text)
{
    struct msgbuf_local msg;
    size_t len = strlen(text);

    msg.mtype = mtype;
    memcpy(msg.mtext, text, len);

    return msgsnd(msqid, &msg, len, 0);
}

static int check_message(const struct msgbuf_local *msg, long expected_type,
                         const char *expected_text, size_t expected_len)
{
    if (msg->mtype != expected_type)
    {
        return 0;
    }
    return memcmp(msg->mtext, expected_text, expected_len) == 0;
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

int main(void)
{
    TEST_START("msgrcv semantic checks");

    key_t key = make_key();
    remove_queue_if_exists(key);

    int msqid = msgget(key, IPC_CREAT | 0600);
    CHECK(msqid >= 0, "msgget IPC_CREAT creates queue for msgrcv test");
    if (msqid < 0)
    {
        TEST_DONE();
    }

    CHECK_RET(send_message(msqid, 1, "AAAAA"), 0, "msgsnd type=1");
    CHECK_RET(send_message(msqid, 2, "BBBBBBBBBB"), 0, "msgsnd type=2");
    CHECK_RET(send_message(msqid, 3, "CCCC"), 0, "msgsnd type=3");
    CHECK_RET(send_message(msqid, 5, "DDDDDDDD"), 0, "msgsnd type=5");

    struct msgbuf_local recv_buf;
    ssize_t n = 0;

    CHECK_ERR(msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 0, MSG_COPY), EINVAL,
              "MSG_COPY without IPC_NOWAIT => EINVAL");
    CHECK_ERR(msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 0,
                     MSG_COPY | MSG_EXCEPT | IPC_NOWAIT),
              EINVAL, "MSG_COPY with MSG_EXCEPT => EINVAL");

    n = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 0, MSG_COPY | IPC_NOWAIT);
    CHECK(n == 5 && check_message(&recv_buf, 1, "AAAAA", 5),
          "MSG_COPY returns first message without removal");

    n = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 1, 0);
    CHECK(n == 5 && check_message(&recv_buf, 1, "AAAAA", 5),
          "msgtyp=1 returns matching message");

    n = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), -3, 0);
    CHECK(n == 10 && check_message(&recv_buf, 2, "BBBBBBBBBB", 10),
          "negative msgtyp returns smallest type <= abs(msgtyp)");

    n = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 3, MSG_EXCEPT);
    CHECK(n == 8 && check_message(&recv_buf, 5, "DDDDDDDD", 8),
          "MSG_EXCEPT skips requested type and returns next available");

    CHECK_RET(send_message(msqid, 6, "EEEEEEEEEEEE"), 0, "msgsnd type=6 long payload");

    CHECK_ERR(msgrcv(msqid, &recv_buf, 4, 6, 0), E2BIG,
              "msgsz too small without MSG_NOERROR => E2BIG");

    n = msgrcv(msqid, &recv_buf, 4, 6, MSG_NOERROR);
    CHECK(n == 4 && check_message(&recv_buf, 6, "EEEE", 4),
          "MSG_NOERROR truncates long message");

    n = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 0, 0);
    CHECK(n == 4 && check_message(&recv_buf, 3, "CCCC", 4),
          "receive remaining message with msgtyp=0");

    CHECK_RET(send_message(msqid, 5, "FIRST"), 0, "msgsnd fifo type=5 first");
    CHECK_RET(send_message(msqid, 1, "SECOND"), 0, "msgsnd fifo type=1 second");
    CHECK_RET(send_message(msqid, 3, "THIRD"), 0, "msgsnd fifo type=3 third");

    n = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 0, MSG_COPY | IPC_NOWAIT);
    CHECK(n == 5 && check_message(&recv_buf, 5, "FIRST", 5),
          "MSG_COPY index 0 follows FIFO order");

    n = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 1, MSG_COPY | IPC_NOWAIT);
    CHECK(n == 6 && check_message(&recv_buf, 1, "SECOND", 6),
          "MSG_COPY index 1 follows FIFO order");

    n = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 0, 0);
    CHECK(n == 5 && check_message(&recv_buf, 5, "FIRST", 5),
          "msgtyp=0 returns FIFO head even if mtype is larger");

    n = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 0, 0);
    CHECK(n == 6 && check_message(&recv_buf, 1, "SECOND", 6),
          "msgtyp=0 drains FIFO in order #2");

    n = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 0, 0);
    CHECK(n == 5 && check_message(&recv_buf, 3, "THIRD", 5),
          "msgtyp=0 drains FIFO in order #3");

    CHECK_ERR(msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 0, IPC_NOWAIT), ENOMSG,
              "empty queue with IPC_NOWAIT => ENOMSG");

    pid_t pid = fork();
    if (pid == 0)
    {
        errno = 0;
        ssize_t ret = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 7, 0);
        if (ret == 5 && check_message(&recv_buf, 7, "BLOCK", 5))
        {
            _exit(0);
        }
        _exit(1);
    }
    sleep_ms(100);
    CHECK_RET(send_message(msqid, 7, "BLOCK"), 0, "msgsnd wakes blocking msgrcv");
    CHECK(wait_child(pid, 2000), "blocking msgrcv unblocks after message arrival");


    CHECK_RET(send_message(msqid, 1, "EXCEPT_ZERO"), 0,
              "msgsnd type=1 for MSG_EXCEPT msgtyp=0");
    CHECK_RET(send_message(msqid, 2, "EXCEPT_NEG"), 0,
              "msgsnd type=2 for MSG_EXCEPT negative msgtyp");

    n = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 0, MSG_EXCEPT);
    CHECK(n == 11 && check_message(&recv_buf, 1, "EXCEPT_ZERO", 11),
          "MSG_EXCEPT with msgtyp=0 is accepted and ignored");

    n = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), -2, MSG_EXCEPT);
    CHECK(n == 10 && check_message(&recv_buf, 2, "EXCEPT_NEG", 10),
          "MSG_EXCEPT with negative msgtyp is accepted and ignored");

    pid_t type1_pid = fork();
    if (type1_pid == 0)
    {
        ssize_t ret = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 1, 0);
        if (ret == 9 && check_message(&recv_buf, 1, "WAKE_ONE1", 9))
        {
            _exit(0);
        }
        _exit(1);
    }
    sleep_ms(100);

    pid_t type2_pid = fork();
    if (type2_pid == 0)
    {
        ssize_t ret = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 2, 0);
        if (ret == 9 && check_message(&recv_buf, 2, "WAKE_TWO2", 9))
        {
            _exit(0);
        }
        _exit(1);
    }
    sleep_ms(100);

    CHECK_RET(send_message(msqid, 2, "WAKE_TWO2"), 0,
              "msgsnd wakes all receive waiters for type recheck");
    CHECK(wait_child(type2_pid, 2000),
          "blocking msgrcv for later matching type is woken");
    CHECK_RET(send_message(msqid, 1, "WAKE_ONE1"), 0,
              "msgsnd wakes remaining receive waiter");
    CHECK(wait_child(type1_pid, 2000),
          "blocking msgrcv for first type still completes");

    pid = fork();
    if (pid == 0)
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
        ssize_t ret = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 9, 0);
        if (ret == -1 && errno == EINTR && got_signal)
        {
            _exit(0);
        }
        _exit(1);
    }
    sleep_ms(100);
    CHECK_RET(kill(pid, SIGUSR1), 0, "signal blocking msgrcv with SA_RESTART");
    CHECK(wait_child(pid, 2000), "blocking msgrcv returns EINTR despite SA_RESTART");

    pid = fork();
    if (pid == 0)
    {
        errno = 0;
        ssize_t ret = msgrcv(msqid, &recv_buf, sizeof(recv_buf.mtext), 9, 0);
        if (ret == -1 && errno == EIDRM)
        {
            _exit(0);
        }
        _exit(1);
    }
    sleep_ms(100);
    CHECK_RET(msgctl(msqid, IPC_RMID, NULL), 0, "msgctl IPC_RMID wakes blocking msgrcv");
    CHECK(wait_child(pid, 2000), "blocking msgrcv returns EIDRM after IPC_RMID");

    TEST_DONE();
}
