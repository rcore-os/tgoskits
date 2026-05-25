#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#define WORKERS 8
#define ITERS 300
#define STACK_SIZE (64 * 1024)

static volatile int g_started;
static volatile int g_done;
static volatile int g_failed;
static volatile int g_progress;

static int worker(void *arg)
{
    int id = (int)(long)arg;
    char path[128];

    __sync_fetch_and_add(&g_started, 1);

    for (int i = 0; i < ITERS && !g_failed; i++) {
        mode_t old = umask((mode_t)((id + i) & 077));

        snprintf(path, sizeof(path), "/tmp/openat-umask-smp-%d-%d.tmp", id, i);
        int fd = openat(AT_FDCWD, path, O_CREAT | O_RDWR | O_TRUNC | O_CLOEXEC, 0666);
        (void)umask(old);
        if (fd < 0) {
            printf("FAIL: worker %d openat iter %d errno=%d (%s)\n", id, i,
                   errno, strerror(errno));
            g_failed = 1;
            return 1;
        }

        if (write(fd, "x", 1) != 1) {
            printf("FAIL: worker %d write iter %d errno=%d (%s)\n", id, i, errno,
                   strerror(errno));
            g_failed = 1;
        }
        close(fd);
        unlink(path);

        __sync_fetch_and_add(&g_progress, 1);
        if ((i & 15) == 0) {
            sched_yield();
        }
    }

    __sync_fetch_and_add(&g_done, 1);
    return g_failed ? 1 : 0;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);

    void *stacks[WORKERS];
    int tids[WORKERS];
    memset(stacks, 0, sizeof(stacks));
    memset(tids, -1, sizeof(tids));

    int flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND;
    for (int i = 0; i < WORKERS; i++) {
        stacks[i] = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (stacks[i] == MAP_FAILED) {
            printf("FAIL: mmap stack %d errno=%d (%s)\n", i, errno,
                   strerror(errno));
            return 1;
        }

        int tid = clone(worker, (char *)stacks[i] + STACK_SIZE, flags,
                        (void *)(long)i);
        if (tid < 0) {
            printf("FAIL: clone worker %d errno=%d (%s)\n", i, errno,
                   strerror(errno));
            return 1;
        }
        tids[i] = tid;
    }

    int last = -1;
    int stalls = 0;
    for (int tick = 0; tick < 600 && g_done < WORKERS && !g_failed; tick++) {
        usleep(100000);
        int progress = __atomic_load_n(&g_progress, __ATOMIC_RELAXED);
        if (progress == last) {
            if (++stalls > 100) {
                printf("FAIL: stalled started=%d done=%d progress=%d\n", g_started,
                       g_done, progress);
                g_failed = 1;
                break;
            }
        } else {
            stalls = 0;
            last = progress;
        }
    }

    for (int i = 0; i < WORKERS; i++) {
        if (tids[i] >= 0) {
            int status = 0;
            if (waitpid(tids[i], &status, __WALL) < 0) {
                printf("FAIL: wait worker %d errno=%d (%s)\n", i, errno,
                       strerror(errno));
                g_failed = 1;
            } else if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
                printf("FAIL: worker %d exit status=0x%x\n", i, status);
                g_failed = 1;
            }
        }
    }

    if (g_failed || g_done != WORKERS || g_progress != WORKERS * ITERS) {
        printf("FAIL: started=%d done=%d progress=%d expected=%d\n", g_started,
               g_done, g_progress, WORKERS * ITERS);
        return 1;
    }

    printf("PASS: openat_umask_smp workers=%d iterations=%d\n", WORKERS, ITERS);
    return 0;
}
