/*
 * test-shm-family - semantic conformance test for System V shared memory
 *                   operations: shmget(2), shmat(2), shmdt(2), shmctl(2).
 *
 * Reference: https://man7.org/linux/man-pages/man2/shmget.2.html
 *            https://man7.org/linux/man-pages/man2/shmop.2.html
 *            https://man7.org/linux/man-pages/man2/shmctl.2.html
 *
 * Semantics exercised by this test (each assertion checks the exact
 * return value / errno that Linux produces):
 *
 *   shmget(key, size, shmflg)
 *     - success: returns a non-negative shmid for the segment of `key`.
 *     - key == IPC_PRIVATE -> always a fresh, distinct segment.
 *     - existing key       -> returns the same shmid; permission bits in
 *                             shmflg need not match the creation flags.
 *     - EEXIST : IPC_CREAT | IPC_EXCL given for an already-existing key.
 *     - ENOENT : key has no segment and IPC_CREAT was not given.
 *     - EINVAL : creating a segment of size 0, or requesting a size
 *                larger than an existing segment.
 *
 *   shmat(shmid, shmaddr, shmflg)
 *     - success: returns the page-aligned attach address; never (void*)-1.
 *     - shmaddr == NULL  -> kernel picks a free, page-aligned address.
 *     - SHM_RDONLY       -> segment attached read-only (still readable).
 *     - EINVAL           -> shmid does not refer to a live segment.
 *     A new segment's pages are zero-filled on first attach.
 *
 *   shmdt(shmaddr)
 *     - success: returns 0; the mapping at shmaddr is removed.
 *     - EINVAL : shmaddr is not the start of an attached segment
 *                (wrong address, NULL, already-detached, unrelated addr).
 *
 *   shmctl(shmid, cmd, buf)
 *     - IPC_STAT: returns 0, fills *buf (shm_segsz, shm_nattch, shm_cpid...).
 *     - IPC_SET : returns 0, updates the permission bits of the segment.
 *     - IPC_RMID: returns 0, marks the segment for destruction. An
 *                 unattached segment is destroyed at once; an attached
 *                 one survives until the last shmdt().
 *     - EINVAL  : invalid command, or shmid does not refer to a live
 *                 segment.
 *
 * Cross-process sharing (fork + shmat) is verified at the end: a write
 * by one process must be observed by another attached to the same id.
 *
 * Note on shm_nattch: the kernel exposes it as a 16-bit count while the
 * userspace struct field is wider, so comparisons mask the low 16 bits.
 */

#include "test_framework.h"

#include <errno.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/shm.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#define SEG_SIZE 4096
/* A shmid that cannot correspond to any segment created by this test. */
#define BAD_SHMID 0x7fffffff

/* Build a process-unique, non-IPC_PRIVATE key for shmget tests. */
static key_t make_key(unsigned int salt)
{
    key_t key = (key_t)(((unsigned int)getpid() << 8) ^ salt);
    if (key == IPC_PRIVATE)
    {
        key ^= 0x1234;
    }
    return key;
}

int main(void)
{
    /* Unbuffered so parent/child output is never lost across fork/exit. */
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("shm-family (shmget / shmat / shmdt / shmctl) semantic checks");

    /* ---- Section 1: shmat basic attach, zero-init, read/write ---- */
    {
        int seg = shmget(IPC_PRIVATE, SEG_SIZE, IPC_CREAT | 0666);
        CHECK(seg >= 0, "shmget IPC_PRIVATE creates a segment");

        errno = 0;
        void *p = shmat(seg, NULL, 0);
        CHECK(p != (void *)-1, "shmat(NULL) returns a valid attach address");

        if (p != (void *)-1)
        {
            unsigned char *b = (unsigned char *)p;
            int allzero = 1;
            for (int i = 0; i < 64; i++)
            {
                if (b[i] != 0)
                    allzero = 0;
            }
            CHECK(allzero, "freshly created segment is zero-initialized");

            int ok = 1;
            for (int i = 0; i < 64; i++)
            {
                b[i] = (unsigned char)(i ^ 0x5a);
            }
            for (int i = 0; i < 64; i++)
            {
                if (b[i] != (unsigned char)(i ^ 0x5a))
                    ok = 0;
            }
            CHECK(ok, "data written through the attached segment reads back");

            CHECK_RET(shmdt(p), 0, "shmdt detaches the attached segment");
        }

        shmctl(seg, IPC_RMID, NULL);
    }

    /* ---- Section 2: shmat error cases ---- */
    {
        errno = 0;
        void *bad1 = shmat(BAD_SHMID, NULL, 0);
        CHECK(bad1 == (void *)-1 && errno == EINVAL,
              "shmat with nonexistent shmid => EINVAL");

        errno = 0;
        void *bad2 = shmat(-1, NULL, 0);
        CHECK(bad2 == (void *)-1 && errno == EINVAL,
              "shmat with shmid -1 => EINVAL");
    }

    /* ---- Section 3: SHM_RDONLY attach can still read seeded data ---- */
    {
        int seg = shmget(IPC_PRIVATE, SEG_SIZE, IPC_CREAT | 0666);
        CHECK(seg >= 0, "shmget for SHM_RDONLY test");

        void *rw = shmat(seg, NULL, 0);
        CHECK(rw != (void *)-1, "shmat read-write to seed data");
        if (rw != (void *)-1)
        {
            *(unsigned int *)rw = 0xa5a5f00du;
            CHECK_RET(shmdt(rw), 0, "shmdt the seeding read-write attach");
        }

        errno = 0;
        void *ro = shmat(seg, NULL, SHM_RDONLY);
        CHECK(ro != (void *)-1, "shmat SHM_RDONLY returns a valid address");
        if (ro != (void *)-1)
        {
            CHECK(*(volatile unsigned int *)ro == 0xa5a5f00du,
                  "SHM_RDONLY attach reads previously written data");
            CHECK_RET(shmdt((void *)ro), 0, "shmdt the read-only attach");
        }

        shmctl(seg, IPC_RMID, NULL);
    }

    /* ---- Section 4: shmdt error cases ---- */
    {
        int seg = shmget(IPC_PRIVATE, SEG_SIZE, IPC_CREAT | 0666);
        CHECK(seg >= 0, "shmget for shmdt error tests");

        void *p = shmat(seg, NULL, 0);
        CHECK(p != (void *)-1, "shmat for shmdt error tests");
        if (p != (void *)-1)
        {
            CHECK_ERR(shmdt((char *)p + 64), EINVAL,
                      "shmdt with a non-attach address => EINVAL");
            CHECK_RET(shmdt(p), 0,
                      "shmdt with the exact attach address succeeds");
            CHECK_ERR(shmdt(p), EINVAL,
                      "shmdt of an already-detached address => EINVAL");
        }

        CHECK_ERR(shmdt(NULL), EINVAL, "shmdt(NULL) => EINVAL");

        char dummy = 0;
        CHECK_ERR(shmdt(&dummy), EINVAL,
                  "shmdt of an unrelated stack address => EINVAL");

        shmctl(seg, IPC_RMID, NULL);
    }

    /* ---- Section 5: shmctl IPC_STAT / IPC_SET ---- */
    {
        int seg = shmget(IPC_PRIVATE, SEG_SIZE, IPC_CREAT | 0660);
        CHECK(seg >= 0, "shmget for shmctl IPC_STAT/IPC_SET test");

        void *p = shmat(seg, NULL, 0);
        CHECK(p != (void *)-1, "shmat before shmctl IPC_STAT");

        struct shmid_ds info;
        memset(&info, 0, sizeof(info));
        CHECK_RET(shmctl(seg, IPC_STAT, &info), 0,
                  "shmctl IPC_STAT returns segment metadata");
        CHECK(info.shm_segsz == (size_t)SEG_SIZE,
              "IPC_STAT reports the requested segment size");
        CHECK((info.shm_nattch & 0xffffUL) == 1UL,
              "IPC_STAT reports exactly one attach");
        CHECK(info.shm_cpid == getpid(),
              "IPC_STAT reports the creator pid");

        struct shmid_ds set = info;
        set.shm_perm.mode = (set.shm_perm.mode & ~0777u) | 0600u;
        CHECK_RET(shmctl(seg, IPC_SET, &set), 0,
                  "shmctl IPC_SET updates the segment");

        struct shmid_ds after;
        memset(&after, 0, sizeof(after));
        CHECK_RET(shmctl(seg, IPC_STAT, &after), 0,
                  "shmctl IPC_STAT after IPC_SET");
        CHECK((after.shm_perm.mode & 0777u) == 0600u,
              "IPC_SET applied the new permission bits");

        if (p != (void *)-1)
        {
            CHECK_RET(shmdt(p), 0, "shmdt before re-checking attach count");
        }
        memset(&after, 0, sizeof(after));
        CHECK_RET(shmctl(seg, IPC_STAT, &after), 0,
                  "shmctl IPC_STAT after detach");
        CHECK((after.shm_nattch & 0xffffUL) == 0UL,
              "IPC_STAT reports zero attaches after shmdt");

        shmctl(seg, IPC_RMID, NULL);
    }

    /* ---- Section 6: shmctl error cases ---- */
    {
        int seg = shmget(IPC_PRIVATE, SEG_SIZE, IPC_CREAT | 0666);
        CHECK(seg >= 0, "shmget for shmctl error tests");

        struct shmid_ds tmp;
        memset(&tmp, 0, sizeof(tmp));
        CHECK_ERR(shmctl(seg, 9999, &tmp), EINVAL,
                  "shmctl with an unknown command => EINVAL");
        CHECK_ERR(shmctl(BAD_SHMID, IPC_STAT, &tmp), EINVAL,
                  "shmctl IPC_STAT with a bad shmid => EINVAL");
        CHECK_ERR(shmctl(-1, IPC_STAT, &tmp), EINVAL,
                  "shmctl IPC_STAT with shmid -1 => EINVAL");
        CHECK_ERR(shmctl(BAD_SHMID, IPC_RMID, NULL), EINVAL,
                  "shmctl IPC_RMID with a bad shmid => EINVAL");
        CHECK_RET(shmctl(seg, IPC_RMID, NULL), 0,
                  "shmctl IPC_RMID removes the segment");
    }

    /* ---- Section 7: IPC_RMID lifecycle ---- */
    {
        /* 7a: IPC_RMID on an unattached segment destroys it immediately. */
        int seg = shmget(IPC_PRIVATE, SEG_SIZE, IPC_CREAT | 0666);
        CHECK(seg >= 0, "shmget for immediate-RMID test");
        CHECK_RET(shmctl(seg, IPC_RMID, NULL), 0,
                  "IPC_RMID on an unattached segment");

        errno = 0;
        void *gone = shmat(seg, NULL, 0);
        CHECK(gone == (void *)-1 && errno == EINVAL,
              "shmat on a destroyed segment => EINVAL");

        struct shmid_ds d;
        memset(&d, 0, sizeof(d));
        CHECK_ERR(shmctl(seg, IPC_STAT, &d), EINVAL,
                  "shmctl IPC_STAT on a destroyed segment => EINVAL");

        /* 7b: IPC_RMID while attached defers destruction to last detach. */
        int seg2 = shmget(IPC_PRIVATE, SEG_SIZE, IPC_CREAT | 0666);
        CHECK(seg2 >= 0, "shmget for deferred-RMID test");

        void *m = shmat(seg2, NULL, 0);
        CHECK(m != (void *)-1, "shmat before deferred IPC_RMID");
        if (m != (void *)-1)
        {
            *(unsigned int *)m = 0xdeadbeefu;
            CHECK_RET(shmctl(seg2, IPC_RMID, NULL), 0,
                      "IPC_RMID while the segment is still attached");
            CHECK(*(volatile unsigned int *)m == 0xdeadbeefu,
                  "segment marked for deletion stays usable while attached");
            CHECK_RET(shmdt(m), 0,
                      "final shmdt triggers the deferred destruction");

            errno = 0;
            void *gone2 = shmat(seg2, NULL, 0);
            CHECK(gone2 == (void *)-1 && errno == EINVAL,
                  "shmat after the last detach of a removed segment => EINVAL");
        }
    }

    /* ---- Section 8: cross-process shared memory via fork ---- */
    {
        int seg = shmget(IPC_PRIVATE, SEG_SIZE, IPC_CREAT | 0666);
        CHECK(seg >= 0, "shmget for cross-process sharing test");

        void *raw = shmat(seg, NULL, 0);
        CHECK(raw != (void *)-1, "parent shmat for sharing test");

        if (raw != (void *)-1)
        {
            volatile unsigned int *sp = (volatile unsigned int *)raw;
            *sp = 0xcafebabeu; /* parent writes before fork */

            pid_t pid = fork();
            CHECK(pid >= 0, "fork for shared-memory test");

            if (pid == 0)
            {
                /* Child: attach the same id and observe the parent's write. */
                int cfail = __fail;
                errno = 0;
                void *craw = shmat(seg, NULL, 0);
                CHECK(craw != (void *)-1, "child shmat of the shared segment");
                if (craw != (void *)-1)
                {
                    volatile unsigned int *cp = (volatile unsigned int *)craw;
                    CHECK(*cp == 0xcafebabeu,
                          "child sees the parent's pre-fork write");
                    *cp = 0x1234abcdu; /* child writes back */
                    CHECK_RET(shmdt((void *)cp), 0, "child shmdt");
                }
                fflush(stdout);
                _exit(__fail > cfail ? 1 : 0);
            }

            /* Parent: wait for the child, then read what it wrote. */
            int status = 0;
            pid_t w = waitpid(pid, &status, 0);
            CHECK(w == pid, "waitpid collected the child");
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "child passed its own shared-memory assertions");
            CHECK(*sp == 0x1234abcdu,
                  "parent sees the child's write through the shared segment");
            CHECK_RET(shmdt((void *)sp), 0, "parent shmdt of the shared segment");
        }

        shmctl(seg, IPC_RMID, NULL);
    }

    /* ---- Section 9: shmget creation modes (IPC_CREAT / IPC_EXCL) ---- */
    {
        key_t key = make_key(0x53484d31u); /* "SHM1" */

        /* A key with no segment and no IPC_CREAT must fail with ENOENT. */
        CHECK_ERR(shmget(key, SEG_SIZE, 0666), ENOENT,
                  "shmget on a missing key without IPC_CREAT => ENOENT");

        /* IPC_CREAT creates the segment. */
        int id1 = shmget(key, SEG_SIZE, IPC_CREAT | 0666);
        CHECK(id1 >= 0, "shmget IPC_CREAT creates a keyed segment");

        /* The same key returns the same id. */
        CHECK_RET(shmget(key, SEG_SIZE, IPC_CREAT | 0666), id1,
                  "shmget on an existing key returns the same id");

        /* An existing key is returned even without IPC_CREAT. */
        CHECK_RET(shmget(key, SEG_SIZE, 0666), id1,
                  "shmget on an existing key succeeds without IPC_CREAT");

        /* IPC_CREAT | IPC_EXCL on an existing key must fail with EEXIST. */
        CHECK_ERR(shmget(key, SEG_SIZE, IPC_CREAT | IPC_EXCL | 0666), EEXIST,
                  "shmget IPC_CREAT|IPC_EXCL on an existing key => EEXIST");

        CHECK_RET(shmctl(id1, IPC_RMID, NULL), 0,
                  "shmctl IPC_RMID removes the keyed segment");

        /* After IPC_RMID the key has no segment again. */
        CHECK_ERR(shmget(key, SEG_SIZE, 0666), ENOENT,
                  "shmget on the key after IPC_RMID => ENOENT");
    }

    /* ---- Section 10: shmget size semantics on an existing key ---- */
    {
        key_t key = make_key(0x53484d32u); /* "SHM2" */

        int id = shmget(key, 2 * SEG_SIZE, IPC_CREAT | 0666);
        CHECK(id >= 0, "shmget creates a two-page segment");

        /* A size <= the segment size is accepted on an existing key. */
        CHECK_RET(shmget(key, SEG_SIZE, 0), id,
                  "shmget with a size <= the segment size returns the id");

        /* The permission bits in shmflg need not match the creation flags. */
        CHECK_RET(shmget(key, 2 * SEG_SIZE, IPC_CREAT | 0600), id,
                  "shmget on an existing key ignores differing permissions");

        /* A size larger than the segment must fail with EINVAL. */
        CHECK_ERR(shmget(key, 3 * SEG_SIZE, 0), EINVAL,
                  "shmget with a size larger than the segment => EINVAL");

        CHECK_RET(shmctl(id, IPC_RMID, NULL), 0,
                  "shmctl IPC_RMID removes the sized segment");
    }

    /* ---- Section 11: shmget error and uniqueness cases ---- */
    {
        /* Creating a segment of size 0 is invalid. */
        CHECK_ERR(shmget(IPC_PRIVATE, 0, IPC_CREAT | 0666), EINVAL,
                  "shmget creating a segment of size 0 => EINVAL");

        /* Each IPC_PRIVATE call yields a distinct segment. */
        int a = shmget(IPC_PRIVATE, SEG_SIZE, IPC_CREAT | 0666);
        int b = shmget(IPC_PRIVATE, SEG_SIZE, IPC_CREAT | 0666);
        CHECK(a >= 0 && b >= 0, "two IPC_PRIVATE shmget calls both succeed");
        CHECK(a != b, "IPC_PRIVATE always creates a distinct segment");

        if (a >= 0)
        {
            shmctl(a, IPC_RMID, NULL);
        }
        if (b >= 0)
        {
            shmctl(b, IPC_RMID, NULL);
        }
    }

    /* ---- Section 12: write-only IPC mode preserved in IPC_STAT ---- */
    {
        /*
         * Regression: the RISC-V PTE layer adds READ when WRITE is set
         * (W=1,R=0 is reserved in RISC-V). The kernel must keep that
         * workaround inside the page-table layer and NOT leak it into
         * the user-visible shm_perm.mode reported by IPC_STAT.
         */
        int seg = shmget(IPC_PRIVATE, SEG_SIZE, IPC_CREAT | 0200);
        CHECK(seg >= 0, "shmget IPC_CREAT|0200 creates a write-only segment");

        void *p = shmat(seg, NULL, 0);
        CHECK(p != (void *)-1, "shmat on a write-only segment succeeds");
        if (p != (void *)-1)
        {
            /* The segment must be writable (kernel may also make it readable
             * internally, but write access must work regardless). */
            *(volatile unsigned int *)p = 0xbeef0200u;
            CHECK(*(volatile unsigned int *)p == 0xbeef0200u,
                  "write-only segment is readable and writable after attach");
            CHECK_RET(shmdt(p), 0, "shmdt the write-only segment");
        }

        struct shmid_ds info;
        memset(&info, 0, sizeof(info));
        CHECK_RET(shmctl(seg, IPC_STAT, &info), 0,
                  "shmctl IPC_STAT on write-only segment");
        CHECK((info.shm_perm.mode & 0777u) == 0200u,
              "IPC_STAT reports write-only permission, not kernel-internal flags");

        shmctl(seg, IPC_RMID, NULL);
    }

    TEST_DONE();
}
