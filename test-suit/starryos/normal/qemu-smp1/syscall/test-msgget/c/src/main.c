#include "test_framework.h"

#include <fcntl.h>
#include <sys/ipc.h>
#include <sys/msg.h>
#include <sys/types.h>
#include <unistd.h>

static key_t make_key(void)
{
    key_t key = (key_t)(((unsigned int)getpid() << 8) ^ 0x4d534747U);
    if (key == IPC_PRIVATE)
    {
        key ^= 0x55;
    }
    return key;
}

static void remove_queue_if_exists(key_t key)
{
    /* Best-effort cleanup so repeated test runs start from a clean state. */
    int id = msgget(key, 0600);
    if (id >= 0)
    {
        (void)msgctl(id, IPC_RMID, NULL);
    }
}

int main(void)
{
    TEST_START("msgget semantic checks");

    /* Use a per-process key so the test can verify both creation and reuse. */
    key_t key = make_key();
    int queue_id = -1;
    int private_id_1 = -1;
    int private_id_2 = -1;

    remove_queue_if_exists(key);

    /* Missing key without IPC_CREAT should fail with ENOENT. */
    CHECK_ERR(msgget(key, 0600), ENOENT, "msgget without IPC_CREAT on missing key => ENOENT");
    /* IPC_EXCL alone does not create a queue, so the same missing-key case still returns ENOENT. */
    CHECK_ERR(msgget(key, IPC_EXCL | 0600), ENOENT,
              "msgget IPC_EXCL without IPC_CREAT on missing key => ENOENT");

    /* IPC_CREAT must create the queue on first use. */
    queue_id = msgget(key, IPC_CREAT | 0600);
    CHECK(queue_id >= 0, "msgget IPC_CREAT creates queue");

    if (queue_id >= 0)
    {
        /* Reopening the same key should return the same queue id. */
        CHECK_RET(msgget(key, 0600), queue_id, "msgget existing key returns same queue id");
        /* IPC_CREAT | IPC_EXCL must fail when the queue already exists. */
        CHECK_ERR(msgget(key, IPC_CREAT | IPC_EXCL | 0600), EEXIST,
                  "msgget IPC_CREAT|IPC_EXCL on existing key => EEXIST");
        /* Remove the queue so the post-RMID lookup path can be checked. */
        CHECK_RET(msgctl(queue_id, IPC_RMID, NULL), 0, "msgctl IPC_RMID removes created queue");
        queue_id = -1;
    }

    /* After removal, the key should be treated as missing again. */
    CHECK_ERR(msgget(key, 0600), ENOENT, "msgget after IPC_RMID on removed key => ENOENT");

    /* IPC_PRIVATE must always allocate a fresh queue id. */
    private_id_1 = msgget(IPC_PRIVATE, 0600);
    CHECK(private_id_1 >= 0, "msgget IPC_PRIVATE creates queue #1");

    private_id_2 = msgget(IPC_PRIVATE, 0600);
    CHECK(private_id_2 >= 0, "msgget IPC_PRIVATE creates queue #2");
    if (private_id_1 >= 0 && private_id_2 >= 0)
    {
        /* Two IPC_PRIVATE calls should never alias the same queue id. */
        CHECK(private_id_1 != private_id_2, "IPC_PRIVATE returns distinct queue ids");
    }

    /* Clean up any queues created during the test. */
    if (private_id_1 >= 0)
    {
        CHECK_RET(msgctl(private_id_1, IPC_RMID, NULL), 0,
                  "remove IPC_PRIVATE queue #1");
    }
    if (private_id_2 >= 0)
    {
        CHECK_RET(msgctl(private_id_2, IPC_RMID, NULL), 0,
                  "remove IPC_PRIVATE queue #2");
    }

    TEST_DONE();
}
