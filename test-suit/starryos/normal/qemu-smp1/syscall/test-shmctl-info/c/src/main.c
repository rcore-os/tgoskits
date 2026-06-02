/*
 * test-shmctl-info — verify shmctl(2) IPC_INFO, SHM_INFO and SHM_STAT
 *                    command paths.
 *
 * These commands are Linux-specific and bypass the per-segment lookup;
 * they exist so that tools like `ipcs` can enumerate the shared memory
 * table without knowing shmids in advance.
 */

#include "test_framework.h"

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/shm.h>
#include <unistd.h>

/* Linux-specific shmctl commands (may be missing from libc headers). */
#ifndef IPC_INFO
#define IPC_INFO 3
#endif

#ifndef SHM_STAT
#define SHM_STAT 13
#endif

#ifndef SHM_INFO
#define SHM_INFO 14
#endif

/*
 * struct shminfo64 — filled by shmctl(0, IPC_INFO, ...).
 *
 * Layout must match the kernel's 64-bit layout (each field is an unsigned
 * long, i.e. 8 bytes on 64-bit platforms).
 */
struct shminfo64
{
    unsigned long shmmax;
    unsigned long shmmin;
    unsigned long shmmni;
    unsigned long shmseg;
    unsigned long shmall;
};

/* struct shm_info is provided by <bits/shm.h> (included via <sys/shm.h>). */

/* Build a process-unique key that is never IPC_PRIVATE. */
static key_t make_key(unsigned int salt)
{
    key_t key = (key_t)(((unsigned int)getpid() << 8) ^ salt);
    if (key == IPC_PRIVATE)
    {
        key ^= 0x1234;
    }
    return key;
}

static void run_ipc_info_test(void)
{
    struct shminfo64 info;
    memset(&info, 0, sizeof(info));

    /* IPC_INFO fills system-wide parameters. */
    int maxid = shmctl(0, IPC_INFO, (struct shmid_ds *)&info);
    CHECK(maxid >= 0, "shmctl(0, IPC_INFO, ...) succeeds");

    /* The returned values must be sane. */
    CHECK(info.shmmax > 0, "IPC_INFO: shmmax > 0");
    CHECK(info.shmmin == 1, "IPC_INFO: shmmin == 1");
    CHECK(info.shmmni > 0, "IPC_INFO: shmmni > 0");
    CHECK(info.shmseg > 0, "IPC_INFO: shmseg > 0");
    CHECK(info.shmall > 0, "IPC_INFO: shmall > 0");

    printf("  INFO | shmmax=%lu shmmin=%lu shmmni=%lu shmseg=%lu shmall=%lu maxid=%d\n",
           info.shmmax, info.shmmin, info.shmmni, info.shmseg, info.shmall, maxid);
}

static void run_shm_info_test(void)
{
    /* Create a segment so that used_ids > 0. */
    key_t key = make_key('S');
    int shmid = shmget(key, 4096, IPC_CREAT | 0600);
    CHECK(shmid >= 0, "shmget for SHM_INFO test");

    struct shm_info info;
    memset(&info, 0, sizeof(info));

    int maxid = shmctl(0, SHM_INFO, (struct shmid_ds *)&info);
    CHECK(maxid >= 0, "shmctl(0, SHM_INFO, ...) succeeds");

    CHECK(info.used_ids >= 1, "SHM_INFO: used_ids >= 1 (we created a segment)");
    CHECK(info.shm_tot > 0, "SHM_INFO: shm_tot > 0");
    /* Without swap, rss == tot. */
    CHECK(info.shm_rss == info.shm_tot, "SHM_INFO: shm_rss == shm_tot");
    CHECK(info.shm_swp == 0, "SHM_INFO: shm_swp == 0");

    printf("  INFO | used_ids=%d shm_tot=%lu shm_rss=%lu maxid=%d\n",
           info.used_ids, info.shm_tot, info.shm_rss, maxid);

    /* Cleanup. */
    int rc = shmctl(shmid, IPC_RMID, NULL);
    CHECK_RET(rc, 0, "IPC_RMID cleanup after SHM_INFO test");
}

static void run_shm_stat_test(void)
{
    /* Create two segments so we can walk them via SHM_STAT. */
    key_t key1 = make_key('A');
    key_t key2 = make_key('B');

    int id1 = shmget(key1, 4096, IPC_CREAT | 0600);
    CHECK(id1 >= 0, "shmget seg1");
    int id2 = shmget(key2, 8192, IPC_CREAT | 0600);
    CHECK(id2 >= 0, "shmget seg2");

    /* Get the max index via IPC_INFO. */
    struct shminfo64 info;
    memset(&info, 0, sizeof(info));
    int maxid = shmctl(0, IPC_INFO, (struct shmid_ds *)&info);
    CHECK(maxid >= 0, "IPC_INFO before SHM_STAT walk");

    int found = 0;
    for (int i = 0; i <= maxid; i++)
    {
        struct shmid_ds ds;
        memset(&ds, 0, sizeof(ds));
        errno = 0;
        int cur = shmctl(i, SHM_STAT, &ds);
        if (cur == -1)
        {
            /* EINVAL means no segment at this index — expected. */
            CHECK(errno == EINVAL || errno == EACCES,
                  "SHM_STAT: non-existent index gives EINVAL or EACCES");
            continue;
        }
        /* The return value is the actual shmid. */
        CHECK(cur == id1 || cur == id2,
              "SHM_STAT returned a known shmid");
        CHECK(ds.shm_segsz > 0, "SHM_STAT: shm_segsz > 0");
        found++;
    }

    CHECK(found >= 2, "SHM_STAT: found at least 2 segments via iteration");

    /* Cleanup. */
    shmctl(id1, IPC_RMID, NULL);
    shmctl(id2, IPC_RMID, NULL);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("shmctl IPC_INFO / SHM_INFO / SHM_STAT basic paths");

    run_ipc_info_test();
    run_shm_info_test();
    run_shm_stat_test();

    TEST_DONE();
}
