#include "test_framework.h"

#include <errno.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/msg.h>
#include <sys/types.h>
#include <unistd.h>

#ifndef MSG_STAT
#define MSG_STAT 11
#endif
#ifndef MSG_INFO
#define MSG_INFO 12
#endif
#ifndef IPC_INFO
#define IPC_INFO 3
#endif

struct msginfo_local
{
    int msgpool;
    int msgmap;
    int msgmax;
    int msgmnb;
    int msgmni;
    int msgssz;
    int msgtql;
    unsigned short msgseg;
};

static key_t make_key(void)
{
    key_t key = (key_t)(((unsigned int)getpid() << 8) ^ 0x4d534d43U);
    if (key == IPC_PRIVATE)
    {
        key ^= 0x27;
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

int main(void)
{
    TEST_START("msgctl semantic checks");

    key_t key = make_key();
    remove_queue_if_exists(key);

    int msqid = msgget(key, IPC_CREAT | 0600);
    CHECK(msqid >= 0, "msgget IPC_CREAT creates queue for msgctl test");
    if (msqid < 0)
    {
        TEST_DONE();
    }

    struct msqid_ds info;
    CHECK_RET(msgctl(msqid, IPC_STAT, &info), 0, "msgctl IPC_STAT returns queue info");
    CHECK(info.msg_qnum == 0, "new queue has zero messages");
    CHECK(info.msg_qbytes > 0, "queue size limit initialized");

    struct msqid_ds set = info;
    set.msg_perm.mode = (set.msg_perm.mode & ~0777) | 0640;
    CHECK_RET(msgctl(msqid, IPC_SET, &set), 0, "msgctl IPC_SET updates mode");

    struct msqid_ds after;
    CHECK_RET(msgctl(msqid, IPC_STAT, &after), 0, "msgctl IPC_STAT after IPC_SET");
    CHECK((after.msg_perm.mode & 0777) == 0640, "IPC_SET applied mode");

    struct msqid_ds msginfo_ds;
    long msginfo_ret = msgctl(msqid, MSG_INFO, &msginfo_ds);
    CHECK(msginfo_ret >= 1, "MSG_INFO returns queue count");
    CHECK(msginfo_ds.msg_qnum >= 1, "MSG_INFO reports queue count");
    CHECK(msginfo_ds.msg_qbytes > 0, "MSG_INFO reports MSGMNB");

    struct msqid_ds stat_ds;
    long stat_ret = msgctl(0, MSG_STAT, &stat_ds);
    CHECK(stat_ret >= 0, "MSG_STAT returns a valid msqid");
    if (stat_ret >= 0)
    {
        struct msqid_ds verify;
        CHECK_RET(msgctl((int)stat_ret, IPC_STAT, &verify), 0,
                  "MSG_STAT msqid is usable");
    }

    struct msginfo_local ipc_info;
    CHECK_RET(msgctl(0, IPC_INFO, (struct msqid_ds *)&ipc_info), 0,
              "IPC_INFO returns system info");
    CHECK(ipc_info.msgmax > 0, "IPC_INFO reports msgmax");
    CHECK(ipc_info.msgmni > 0, "IPC_INFO reports msgmni");

    CHECK_ERR(msgctl(msqid, 999, NULL), EINVAL, "msgctl unknown cmd => EINVAL");

    CHECK_RET(msgctl(msqid, IPC_RMID, NULL), 0, "msgctl IPC_RMID removes queue");
    CHECK_ERR(msgctl(msqid, IPC_STAT, &after), EINVAL,
              "IPC_STAT on removed queue => EINVAL");
    CHECK_ERR(msgget(key, 0600), ENOENT,
              "msgget after IPC_RMID on removed key => ENOENT");

    TEST_DONE();
}
