#define _GNU_SOURCE

#include "test_framework.h"

#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <sys/ipc.h>
#include <sys/msg.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef IPC_INFO
#define IPC_INFO 3
#endif

enum {
    CONCURRENT_CREATORS = 8,
    FREE_SLOTS = 16,
    MAX_LOCAL_IDS = CONCURRENT_CREATORS * FREE_SLOTS,
};

struct msginfo_local {
    int msgpool;
    int msgmap;
    int msgmax;
    int msgmnb;
    int msgmni;
    int msgssz;
    int msgtql;
    unsigned short msgseg;
};

struct creator {
    int ids[MAX_LOCAL_IDS];
    int created;
    int error;
};

static atomic_int creators_ready;
static atomic_int start_creators;

static key_t make_key(void)
{
    key_t key = (key_t)(((unsigned int)getpid() << 8) ^ 0x4d534731U);
    return key == IPC_PRIVATE ? key ^ 0x27 : key;
}

static void *create_until_full(void *arg)
{
    struct creator *creator = arg;

    atomic_fetch_add_explicit(&creators_ready, 1, memory_order_release);
    while (atomic_load_explicit(&start_creators, memory_order_acquire) == 0) {
        sched_yield();
    }

    for (int i = 0; i < MAX_LOCAL_IDS; i++) {
        if (creator->created >= MAX_LOCAL_IDS) {
            creator->error = EOVERFLOW;
            break;
        }
        errno = 0;
        int id = msgget(IPC_PRIVATE, 0600);
        if (id >= 0) {
            creator->ids[creator->created++] = id;
        } else if (errno == ENOSPC) {
            break;
        } else {
            creator->error = errno;
            break;
        }
    }
    return NULL;
}

static int child_permission_denied(key_t key)
{
    if (setgid(65534) != 0 || setuid(65534) != 0) {
        return 2;
    }

    errno = 0;
    int id = msgget(key, 0400);
    return id == -1 && errno == EACCES ? 0 : 1;
}

int main(void)
{
    int *ids = NULL;
    int stored = 0;
    int id_capacity = 0;
    int limit = 0;
    key_t key = make_key();
    key_t missing_key = key ^ 0x40000000;
    pthread_t threads[CONCURRENT_CREATORS];
    struct creator creators[CONCURRENT_CREATORS] = {0};
    int started = 0;
    int cleanup_failures = 0;
    int freed = 0;
    int concurrent_created = 0;
    int worker_error = 0;

    TEST_START("msgget namespace capacity and creation-only ENOSPC");

    if (unshare(CLONE_NEWIPC) != 0) {
        CHECK(0, "unshare a private IPC namespace for the capacity regression");
        TEST_DONE();
    }
    CHECK(1, "unshare a private IPC namespace for the capacity regression");

    struct msginfo_local info = {0};
    long info_result = msgctl(0, IPC_INFO, (struct msqid_ds *)&info);
    CHECK(info_result >= 0 && info.msgmni > 0,
          "IPC_INFO reports the namespace queue limit");
    if (info_result < 0 || info.msgmni <= FREE_SLOTS + 1) {
        TEST_DONE();
    }

    limit = info.msgmni;
    id_capacity = limit + MAX_LOCAL_IDS + 1;
    ids = calloc((size_t)id_capacity, sizeof(*ids));
    CHECK(ids != NULL, "allocate queue-id tracking table");
    if (ids == NULL) {
        TEST_DONE();
    }

    errno = 0;
    int keyed = msgget(key, IPC_CREAT | IPC_EXCL | 0600);
    CHECK(keyed >= 0, "create the reusable keyed queue");
    if (keyed < 0) {
        goto cleanup;
    }
    ids[stored++] = keyed;

    for (;;) {
        errno = 0;
        int id = msgget(IPC_PRIVATE, 0600);
        if (id < 0) {
            CHECK(errno == ENOSPC,
                  "private queue creation stops only at ENOSPC");
            break;
        }
        if (stored >= id_capacity) {
            CHECK(0, "queue-id tracking capacity remains sufficient");
            goto cleanup;
        }
        ids[stored++] = id;
    }
    CHECK(stored > 0, "queue creation reaches the process-visible namespace limit");

    errno = 0;
    int over_limit = msgget(IPC_PRIVATE, 0600);
    CHECK(over_limit == -1 && errno == ENOSPC,
          "a new private queue at MSGMNI returns ENOSPC");
    if (over_limit >= 0) {
        if (msgctl(over_limit, IPC_RMID, NULL) != 0) {
            cleanup_failures++;
        }
    }

    errno = 0;
    int opened = msgget(key, 0);
    CHECK(opened == keyed,
          "opening an existing keyed queue is not hidden by ENOSPC");

    errno = 0;
    int exclusive = msgget(key, IPC_CREAT | IPC_EXCL | 0600);
    CHECK(exclusive == -1 && errno == EEXIST,
          "IPC_CREAT|IPC_EXCL reports EEXIST instead of ENOSPC");

    errno = 0;
    int missing = msgget(missing_key, 0);
    CHECK(missing == -1 && errno == ENOENT,
          "missing key without IPC_CREAT remains ENOENT at MSGMNI");
    if (missing >= 0) {
        if (msgctl(missing, IPC_RMID, NULL) != 0) {
            cleanup_failures++;
        }
    }

    errno = 0;
    missing = msgget(missing_key, IPC_CREAT | 0600);
    CHECK(missing == -1 && errno == ENOSPC,
          "missing key with IPC_CREAT reports ENOSPC at MSGMNI");
    if (missing >= 0) {
        if (msgctl(missing, IPC_RMID, NULL) != 0) {
            cleanup_failures++;
        }
    }

    errno = 0;
    missing = msgget(missing_key, 0);
    CHECK(missing == -1 && errno == ENOENT,
          "failed IPC_CREAT leaves the missing key absent");
    if (missing >= 0) {
        if (msgctl(missing, IPC_RMID, NULL) != 0) {
            cleanup_failures++;
        }
    }

    pid_t child = fork();
    CHECK(child >= 0, "fork unprivileged msgget permission probe");
    if (child == 0) {
        _exit(child_permission_denied(key));
    }
    if (child > 0) {
        int status = 0;
        do {
            errno = 0;
        } while (waitpid(child, &status, 0) == -1 && errno == EINTR);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "denied existing-key open reports EACCES instead of ENOSPC");
    }

    /*
     * Free a small batch, then race creators for exactly those slots. Any
     * check/insert race that lets the queue count pass MSGMNI is observable in
     * the successful-allocation count.
     */
    for (int i = stored - 1; i > 0 && freed < FREE_SLOTS; i--) {
        if (msgctl(ids[i], IPC_RMID, NULL) == 0) {
            ids[i] = -1;
            freed++;
        }
    }
    CHECK(freed == FREE_SLOTS, "open exactly one concurrent batch of slots");

    atomic_init(&creators_ready, 0);
    atomic_init(&start_creators, 0);
    for (started = 0; started < CONCURRENT_CREATORS; started++) {
        int error = pthread_create(&threads[started], NULL,
                                   create_until_full, &creators[started]);
        if (error != 0) {
            errno = error;
            CHECK(0, "create concurrent msgget workers");
            break;
        }
    }

    if (started == CONCURRENT_CREATORS) {
        while (atomic_load_explicit(&creators_ready, memory_order_acquire) <
               CONCURRENT_CREATORS) {
            sched_yield();
        }
        atomic_store_explicit(&start_creators, 1, memory_order_release);
        for (int i = 0; i < started; i++) {
            CHECK(pthread_join(threads[i], NULL) == 0,
                  "join concurrent msgget worker");
        }
    } else {
        atomic_store_explicit(&start_creators, 1, memory_order_release);
        for (int i = 0; i < started; i++) {
            (void)pthread_join(threads[i], NULL);
        }
    }

    for (int i = 0; i < started; i++) {
        concurrent_created += creators[i].created;
        if (creators[i].error != 0) {
            worker_error = creators[i].error;
        }
        for (int j = 0; j < creators[i].created; j++) {
            if (stored >= id_capacity) {
                if (msgctl(creators[i].ids[j], IPC_RMID, NULL) != 0) {
                    cleanup_failures++;
                }
                worker_error = EOVERFLOW;
                continue;
            }
            ids[stored++] = creators[i].ids[j];
        }
    }
    errno = worker_error;
    CHECK(started == CONCURRENT_CREATORS && worker_error == 0,
          "concurrent creators only observe success or ENOSPC");
    CHECK(concurrent_created == FREE_SLOTS,
          "concurrent creators fill exactly the freed MSGMNI slots");

    errno = 0;
    over_limit = msgget(IPC_PRIVATE, 0600);
    CHECK(over_limit == -1 && errno == ENOSPC,
          "concurrent creation returns to ENOSPC without crossing MSGMNI");
    if (over_limit >= 0) {
        if (msgctl(over_limit, IPC_RMID, NULL) != 0) {
            cleanup_failures++;
        }
    }

cleanup:
    for (int i = stored - 1; i >= 0; i--) {
        if (ids[i] >= 0) {
            if (msgctl(ids[i], IPC_RMID, NULL) != 0) {
                cleanup_failures++;
            }
        }
    }
    errno = cleanup_failures;
    CHECK(cleanup_failures == 0,
          "capacity regression removes every tracked queue");
    if (stored > 0) {
        errno = 0;
        CHECK(msgget(key, 0) == -1 && errno == ENOENT,
              "capacity regression cleans up its keyed queue");
    }
    free(ids);
    TEST_DONE();
}
