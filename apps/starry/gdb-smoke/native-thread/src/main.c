#include <pthread.h>
#include <stdio.h>

struct worker_arg {
    int id;
    int result;
};

__attribute__((noinline)) static int thread_marker(int id)
{
    volatile int marker_id = id;
    return marker_id + 100;
}

static void *thread_worker(void *arg)
{
    struct worker_arg *worker = arg;
    worker->result = thread_marker(worker->id);
    return NULL;
}

int main(void)
{
    pthread_t threads[2];
    struct worker_arg args[2] = {
        {.id = 1, .result = 0},
        {.id = 2, .result = 0},
    };

    for (int i = 0; i < 2; i++) {
        if (pthread_create(&threads[i], NULL, thread_worker, &args[i]) != 0) {
            perror("pthread_create");
            return 1;
        }
    }

    for (int i = 0; i < 2; i++) {
        if (pthread_join(threads[i], NULL) != 0) {
            perror("pthread_join");
            return 1;
        }
    }

    printf("gdb-native-thread-target results=%d,%d\n", args[0].result, args[1].result);
    return args[0].result == 101 && args[1].result == 102 ? 0 : 1;
}
