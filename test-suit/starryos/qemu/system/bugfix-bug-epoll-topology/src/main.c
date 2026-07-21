#define _GNU_SOURCE
#include "test_framework.h"

#include <pthread.h>
#include <sys/epoll.h>
#include <unistd.h>

struct concurrent_add {
    pthread_barrier_t *barrier;
    int source;
    int target;
    int rc;
    int error;
};

static struct epoll_event nested_event(void)
{
    struct epoll_event event = {
        .events = EPOLLIN,
        .data.u64 = 1,
    };
    return event;
}

static int add_nested(int source, int target)
{
    struct epoll_event event = nested_event();
    return epoll_ctl(source, EPOLL_CTL_ADD, target, &event);
}

static void close_epolls(int *epolls, size_t count)
{
    for (size_t i = 0; i < count; i++)
        close(epolls[i]);
}

static void test_nesting_depth_limit(void)
{
    int epolls[6];
    for (size_t i = 0; i < 6; i++) {
        epolls[i] = epoll_create1(EPOLL_CLOEXEC);
        CHECK(epolls[i] >= 0, "create epoll for nesting-depth chain");
    }

    for (size_t i = 0; i < 4; i++)
        CHECK_RET(add_nested(epolls[i], epolls[i + 1]), 0,
                  "four nested epoll edges are allowed");
    CHECK_ERR(add_nested(epolls[4], epolls[5]), ELOOP,
              "fifth nested epoll edge returns ELOOP");

    close_epolls(epolls, 6);
}

static void test_indirect_cycle(void)
{
    int epolls[3];
    for (size_t i = 0; i < 3; i++) {
        epolls[i] = epoll_create1(EPOLL_CLOEXEC);
        CHECK(epolls[i] >= 0, "create epoll for indirect-cycle test");
    }

    CHECK_RET(add_nested(epolls[0], epolls[1]), 0,
              "add first indirect-cycle edge");
    CHECK_RET(add_nested(epolls[1], epolls[2]), 0,
              "add second indirect-cycle edge");
    CHECK_ERR(add_nested(epolls[2], epolls[0]), ELOOP,
              "indirect epoll cycle returns ELOOP");

    close_epolls(epolls, 3);
}

static void test_duplicate_target_edges_are_deleted_exactly(void)
{
    int source = epoll_create1(EPOLL_CLOEXEC);
    int target = epoll_create1(EPOLL_CLOEXEC);
    CHECK(source >= 0 && target >= 0,
          "create epolls for exact nested-edge deletion");
    int target_alias = dup(target);
    CHECK(target_alias >= 0, "duplicate nested epoll descriptor");

    CHECK_RET(add_nested(source, target), 0, "add first target edge");
    CHECK_RET(add_nested(source, target_alias), 0,
              "add duplicate-descriptor target edge");
    CHECK_RET(epoll_ctl(source, EPOLL_CTL_DEL, target, NULL), 0,
              "delete only the first target edge");
    CHECK_ERR(add_nested(target, source), ELOOP,
              "remaining duplicate target edge still prevents a cycle");

    CHECK_RET(epoll_ctl(source, EPOLL_CTL_DEL, target_alias, NULL), 0,
              "delete the remaining target edge");
    CHECK_RET(add_nested(target, source), 0,
              "reverse edge succeeds after both target edges are removed");

    close(target_alias);
    close(source);
    close(target);
}

static void *concurrent_add_thread(void *opaque)
{
    struct concurrent_add *add = opaque;
    int barrier_rc = pthread_barrier_wait(add->barrier);
    if (barrier_rc != 0 && barrier_rc != PTHREAD_BARRIER_SERIAL_THREAD) {
        add->rc = -1;
        add->error = barrier_rc;
        return NULL;
    }

    errno = 0;
    add->rc = add_nested(add->source, add->target);
    add->error = errno;
    return NULL;
}

static void test_concurrent_reverse_edges(void)
{
    int epolls[2] = {
        epoll_create1(EPOLL_CLOEXEC),
        epoll_create1(EPOLL_CLOEXEC),
    };
    CHECK(epolls[0] >= 0 && epolls[1] >= 0,
          "create epolls for concurrent reverse-edge test");

    pthread_barrier_t barrier;
    CHECK_RET(pthread_barrier_init(&barrier, NULL, 2), 0,
              "initialize concurrent epoll barrier");
    struct concurrent_add adds[2] = {
        {.barrier = &barrier, .source = epolls[0], .target = epolls[1], .rc = -2},
        {.barrier = &barrier, .source = epolls[1], .target = epolls[0], .rc = -2},
    };
    pthread_t threads[2];
    CHECK_RET(pthread_create(&threads[0], NULL, concurrent_add_thread, &adds[0]), 0,
              "create first reverse-edge thread");
    CHECK_RET(pthread_create(&threads[1], NULL, concurrent_add_thread, &adds[1]), 0,
              "create second reverse-edge thread");
    CHECK_RET(pthread_join(threads[0], NULL), 0,
              "join first reverse-edge thread");
    CHECK_RET(pthread_join(threads[1], NULL), 0,
              "join second reverse-edge thread");

    int successes = (adds[0].rc == 0) + (adds[1].rc == 0);
    int loops = (adds[0].rc == -1 && adds[0].error == ELOOP) +
                (adds[1].rc == -1 && adds[1].error == ELOOP);
    CHECK(successes == 1 && loops == 1,
          "concurrent reverse edges yield exactly one success and one ELOOP");

    pthread_barrier_destroy(&barrier);
    close_epolls(epolls, 2);
}

int main(void)
{
    TEST_START("epoll topology transactions");
    test_nesting_depth_limit();
    test_indirect_cycle();
    test_duplicate_target_edges_are_deleted_exactly();
    test_concurrent_reverse_edges();
    TEST_DONE();
}
