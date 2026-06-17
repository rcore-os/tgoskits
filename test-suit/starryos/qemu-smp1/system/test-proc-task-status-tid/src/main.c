#define _GNU_SOURCE

#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

struct thread_sync {
    pthread_mutex_t lock;
    pthread_cond_t ready;
    pthread_cond_t done;
    pid_t tid;
    int should_exit;
};

static int fail(const char *msg)
{
    printf("FAIL: %s: errno=%d (%s)\n", msg, errno, strerror(errno));
    return 1;
}

static pid_t raw_gettid(void)
{
    return (pid_t)syscall(SYS_gettid);
}

static void *worker_main(void *arg)
{
    struct thread_sync *sync = arg;
    pid_t tid = raw_gettid();

    pthread_mutex_lock(&sync->lock);
    sync->tid = tid;
    pthread_cond_signal(&sync->ready);
    while (!sync->should_exit) {
        pthread_cond_wait(&sync->done, &sync->lock);
    }
    pthread_mutex_unlock(&sync->lock);
    return NULL;
}

static int read_status_value(pid_t tid, const char *key)
{
    char path[128];
    snprintf(path, sizeof(path), "/proc/self/task/%ld/status", (long)tid);

    FILE *file = fopen(path, "r");
    if (file == NULL) {
        return -1;
    }

    char line[256];
    int value = -1;
    while (fgets(line, sizeof(line), file) != NULL) {
        if (sscanf(line, "Tgid:\t%d", &value) == 1 && strcmp(key, "Tgid") == 0) {
            fclose(file);
            return value;
        }
        if (sscanf(line, "Pid:\t%d", &value) == 1 && strcmp(key, "Pid") == 0) {
            fclose(file);
            return value;
        }
    }

    fclose(file);
    errno = ENOENT;
    return -1;
}

static void stop_worker(struct thread_sync *sync, pthread_t worker)
{
    pthread_mutex_lock(&sync->lock);
    sync->should_exit = 1;
    pthread_cond_signal(&sync->done);
    pthread_mutex_unlock(&sync->lock);
    pthread_join(worker, NULL);
}

int main(void)
{
    struct thread_sync sync = {
        .lock = PTHREAD_MUTEX_INITIALIZER,
        .ready = PTHREAD_COND_INITIALIZER,
        .done = PTHREAD_COND_INITIALIZER,
        .tid = 0,
        .should_exit = 0,
    };
    pthread_t worker;

    if (pthread_create(&worker, NULL, worker_main, &sync) != 0) {
        return fail("pthread_create");
    }

    pthread_mutex_lock(&sync.lock);
    while (sync.tid == 0) {
        pthread_cond_wait(&sync.ready, &sync.lock);
    }
    pid_t worker_tid = sync.tid;
    pthread_mutex_unlock(&sync.lock);

    int tgid = read_status_value(worker_tid, "Tgid");
    int pid = read_status_value(worker_tid, "Pid");
    if (tgid < 0 || pid < 0) {
        int saved_errno = errno;
        stop_worker(&sync, worker);
        errno = saved_errno;
        return fail("read /proc/self/task/<tid>/status");
    }

    pid_t process_pid = getpid();
    if (tgid != process_pid) {
        printf("FAIL: expected task status Tgid %ld, got %d\n", (long)process_pid, tgid);
        stop_worker(&sync, worker);
        return 1;
    }
    if (pid != worker_tid) {
        printf("FAIL: expected task status Pid %ld, got %d\n", (long)worker_tid, pid);
        stop_worker(&sync, worker);
        return 1;
    }

    stop_worker(&sync, worker);

    printf("DONE: 1 pass, 0 fail\n");
    return 0;
}
