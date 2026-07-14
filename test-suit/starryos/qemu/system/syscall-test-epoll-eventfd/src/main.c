/*
 * bug-epoll-eventfd-et: Edge-triggered epoll + eventfd edge cases.
 *
 * Tests the exact semantics that mio/tokio relies on for task waking:
 * - eventfd registered with EPOLLET|EPOLLIN
 * - write() to eventfd must generate a new epoll edge event EVERY time,
 *   even if the counter was already > 0 (i.e., even without a read in between)
 *
 * This is the suspected root cause of the tokio loopback stall on StarryOS.
 * Linux fires ep_poll_callback on every write() to eventfd, regardless of
 * the current counter value. If StarryOS only fires on 0→non-zero transitions,
 * the second write (without intermediate read) won't wake epoll_wait.
 */
#define _GNU_SOURCE
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <unistd.h>
#include <stdint.h>
#include <fcntl.h>
#include "test_framework.h"

static int efd = -1;
static int epfd = -1;

static void setup(void) {
    efd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    if (efd < 0) {
        perror("eventfd");
        exit(1);
    }
    epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        perror("epoll_create1");
        exit(1);
    }
    struct epoll_event ev = {
        .events = EPOLLIN | EPOLLET,
        .data.fd = efd,
    };
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, efd, &ev) < 0) {
        perror("epoll_ctl ADD");
        exit(1);
    }
}

static void teardown(void) {
    close(epfd);
    close(efd);
    epfd = -1;
    efd = -1;
}

static void write_eventfd(uint64_t val) {
    uint64_t v = val;
    ssize_t n = write(efd, &v, sizeof(v));
    if (n != 8) {
        perror("write eventfd");
        exit(1);
    }
}

static void read_eventfd(void) {
    uint64_t v;
    ssize_t n = read(efd, &v, sizeof(v));
    if (n != 8 && !(n == -1 && errno == EAGAIN)) {
        perror("read eventfd");
        exit(1);
    }
}

/* Returns number of events (0 or 1) */
static int poll_once(int timeout_ms) {
    struct epoll_event ev;
    int n = epoll_wait(epfd, &ev, 1, timeout_ms);
    if (n < 0) {
        perror("epoll_wait");
        exit(1);
    }
    return n;
}

/*
 * Test 1: Basic — write then epoll_wait should return event.
 */
static void test_basic_write_then_wait(void) {
    printf("\n--- test_basic_write_then_wait ---\n");
    setup();

    write_eventfd(1);
    int n = poll_once(100);
    CHECK(n == 1, "epoll_wait returns event after write");

    /* Second epoll_wait without new write should NOT return (ET) */
    n = poll_once(0);
    CHECK(n == 0, "no event on second epoll_wait without new write (ET behavior)");

    teardown();
}

/*
 * Test 2: write, wait, read, write, wait — classic re-arm.
 * After reading (counter→0) and writing again (0→non-zero), should fire.
 */
static void test_write_wait_read_write_wait(void) {
    printf("\n--- test_write_wait_read_write_wait ---\n");
    setup();

    write_eventfd(1);
    int n = poll_once(100);
    CHECK(n == 1, "first write triggers event");

    read_eventfd();  /* counter → 0 */
    write_eventfd(1);  /* counter 0 → 1 */
    n = poll_once(100);
    CHECK(n == 1, "write after read triggers new event");

    teardown();
}

/*
 * Test 3: THE CRITICAL CASE — write, wait, NO read, write again, wait.
 * This is what mio's Waker does: write(1) to wake, and tokio may not
 * read/drain the eventfd before the next wake() call.
 *
 * On Linux: every write() fires ep_poll_callback regardless of counter value.
 * If StarryOS only fires on 0→non-zero, this will FAIL.
 */
static void test_write_wait_noread_write_wait(void) {
    printf("\n--- test_write_wait_noread_write_wait (CRITICAL) ---\n");
    setup();

    write_eventfd(1);  /* counter: 0 → 1 */
    int n = poll_once(100);
    CHECK(n == 1, "first write triggers event");

    /* DO NOT read — counter stays at 1 */
    write_eventfd(1);  /* counter: 1 → 2 (no state transition in readability!) */
    n = poll_once(100);
    CHECK(n == 1, "second write WITHOUT read triggers new event (mio waker pattern)");

    teardown();
}

/*
 * Test 4: Multiple writes without any reads — each should generate an edge.
 * This simulates rapid wake() calls from tokio.
 */
static void test_multiple_writes_no_reads(void) {
    printf("\n--- test_multiple_writes_no_reads ---\n");
    setup();

    write_eventfd(1);
    int n = poll_once(100);
    CHECK(n == 1, "write #1 triggers event");

    write_eventfd(1);
    n = poll_once(100);
    CHECK(n == 1, "write #2 (no read) triggers event");

    write_eventfd(1);
    n = poll_once(100);
    CHECK(n == 1, "write #3 (no read) triggers event");

    write_eventfd(1);
    n = poll_once(100);
    CHECK(n == 1, "write #4 (no read) triggers event");

    teardown();
}

/*
 * Test 5: Write BEFORE registering with epoll — should fire on first wait.
 * (fd is already readable when added to epoll)
 */
static void test_write_before_register(void) {
    printf("\n--- test_write_before_register ---\n");

    efd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    epfd = epoll_create1(EPOLL_CLOEXEC);

    /* Write BEFORE epoll_ctl ADD */
    write_eventfd(1);

    struct epoll_event ev = {
        .events = EPOLLIN | EPOLLET,
        .data.fd = efd,
    };
    epoll_ctl(epfd, EPOLL_CTL_ADD, efd, &ev);

    int n = poll_once(100);
    CHECK(n == 1, "event fires for fd already readable at registration time");

    teardown();
}

/*
 * Test 6: Interleaved read and write — proper re-arming.
 */
static void test_interleaved_read_write(void) {
    printf("\n--- test_interleaved_read_write ---\n");
    setup();

    for (int i = 0; i < 5; i++) {
        write_eventfd(1);
        int n = poll_once(100);
        CHECK(n == 1, "write triggers event in loop iteration");
        read_eventfd();
    }

    teardown();
}

/*
 * Test 7: epoll_wait with timeout=0 (non-blocking) after write.
 * Ensures the event is available immediately, not deferred.
 */
static void test_nonblocking_poll_after_write(void) {
    printf("\n--- test_nonblocking_poll_after_write ---\n");
    setup();

    write_eventfd(1);
    int n = poll_once(0);  /* timeout=0: return immediately */
    CHECK(n == 1, "event available immediately with timeout=0");

    teardown();
}

/*
 * Test 8: Large write value — eventfd accepts any u64 except UINT64_MAX.
 */
static void test_large_write_value(void) {
    printf("\n--- test_large_write_value ---\n");
    setup();

    write_eventfd(42);
    int n = poll_once(100);
    CHECK(n == 1, "large write value triggers event");

    /* Write again without read */
    write_eventfd(100);
    n = poll_once(100);
    CHECK(n == 1, "second large write without read triggers event");

    teardown();
}

/*
 * Test 9: EPOLL_CTL_MOD — re-register with same flags, then write.
 * Some implementations lose the pending event on MOD.
 */
static void test_mod_then_write(void) {
    printf("\n--- test_mod_then_write ---\n");
    setup();

    /* MOD with same flags */
    struct epoll_event ev = {
        .events = EPOLLIN | EPOLLET,
        .data.fd = efd,
    };
    int rc = epoll_ctl(epfd, EPOLL_CTL_MOD, efd, &ev);
    CHECK(rc == 0, "EPOLL_CTL_MOD succeeds");

    write_eventfd(1);
    int n = poll_once(100);
    CHECK(n == 1, "write after MOD triggers event");

    teardown();
}

/*
 * Test 10: The tokio waker pattern — write, wait, read, write, no-wait, write, wait.
 * Simulates: wake → epoll returns → drain eventfd → wake again → 
 * runtime processes tasks (no epoll) → wake again → epoll_wait.
 */
static void test_tokio_waker_pattern(void) {
    printf("\n--- test_tokio_waker_pattern ---\n");
    setup();

    /* First wake cycle */
    write_eventfd(1);
    int n = poll_once(100);
    CHECK(n == 1, "first wake cycle: event fires");

    /* Drain (tokio reads eventfd after epoll returns) */
    read_eventfd();

    /* Second wake — but runtime doesn't call epoll_wait yet */
    write_eventfd(1);

    /* Third wake — runtime still hasn't called epoll_wait */
    write_eventfd(1);

    /* NOW runtime calls epoll_wait */
    n = poll_once(100);
    CHECK(n == 1, "event fires after multiple writes since last read");

    /* Drain again */
    read_eventfd();

    /* One more cycle to confirm */
    write_eventfd(1);
    n = poll_once(100);
    CHECK(n == 1, "final wake cycle works");

    teardown();
}

/*
 * Test 11: eventfd with pipes — epoll monitors BOTH an eventfd and a pipe.
 * Write to eventfd while pipe has no data. Only eventfd should fire.
 * This simulates tokio monitoring sockets + waker eventfd simultaneously.
 */
static void test_eventfd_with_pipe(void) {
    printf("\n--- test_eventfd_with_pipe ---\n");

    int pipefd[2];
    if (pipe(pipefd) < 0) {
        perror("pipe");
        exit(1);
    }
    /* Make pipe read end non-blocking */
    fcntl(pipefd[0], F_SETFL, O_NONBLOCK);

    efd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    epfd = epoll_create1(EPOLL_CLOEXEC);

    struct epoll_event ev;

    /* Register pipe read end */
    ev.events = EPOLLIN | EPOLLET;
    ev.data.fd = pipefd[0];
    epoll_ctl(epfd, EPOLL_CTL_ADD, pipefd[0], &ev);

    /* Register eventfd */
    ev.events = EPOLLIN | EPOLLET;
    ev.data.fd = efd;
    epoll_ctl(epfd, EPOLL_CTL_ADD, efd, &ev);

    /* Write to eventfd only */
    write_eventfd(1);

    struct epoll_event events[2];
    int n = epoll_wait(epfd, events, 2, 100);
    CHECK(n == 1, "only eventfd fires (pipe has no data)");
    if (n == 1) {
        CHECK(events[0].data.fd == efd, "fired event is the eventfd");
    }

    /* Now write to pipe too */
    char c = 'x';
    (void)write(pipefd[1], &c, 1);

    /* Both should NOT fire — eventfd already consumed, pipe is new */
    n = epoll_wait(epfd, events, 2, 100);
    CHECK(n == 1, "only pipe fires (eventfd edge already consumed)");
    if (n == 1) {
        CHECK(events[0].data.fd == pipefd[0], "fired event is the pipe");
    }

    close(pipefd[0]);
    close(pipefd[1]);
    teardown();
}

/*
 * Test 12: The EXACT mio pattern — eventfd + socket, write eventfd
 * while socket is also registered. Confirms eventfd wakes epoll_wait
 * even when other fds are being monitored.
 */
static void test_eventfd_wakes_among_sockets(void) {
    printf("\n--- test_eventfd_wakes_among_sockets ---\n");

    int pipefd[2];
    if (pipe(pipefd) < 0) {
        perror("pipe");
        exit(1);
    }
    fcntl(pipefd[0], F_SETFL, O_NONBLOCK);

    efd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    epfd = epoll_create1(EPOLL_CLOEXEC);

    struct epoll_event ev;
    ev.events = EPOLLIN | EPOLLET;
    ev.data.fd = pipefd[0];
    epoll_ctl(epfd, EPOLL_CTL_ADD, pipefd[0], &ev);

    ev.events = EPOLLIN | EPOLLET;
    ev.data.fd = efd;
    epoll_ctl(epfd, EPOLL_CTL_ADD, efd, &ev);

    /* First: trigger eventfd, consume it */
    write_eventfd(1);
    struct epoll_event events[2];
    int n = epoll_wait(epfd, events, 2, 100);
    CHECK(n >= 1, "initial eventfd wake");
    read_eventfd();

    /* Now: write eventfd again (the "task wake" after processing) */
    write_eventfd(1);
    n = epoll_wait(epfd, events, 2, 100);
    CHECK(n == 1, "eventfd re-wake after drain works");
    if (n == 1) {
        CHECK(events[0].data.fd == efd, "re-wake event is eventfd");
    }

    close(pipefd[0]);
    close(pipefd[1]);
    teardown();
}

int main(void) {
    TEST_START("bug-epoll-eventfd-et");

    test_basic_write_then_wait();
    test_write_wait_read_write_wait();
    test_write_wait_noread_write_wait();      /* CRITICAL — suspected bug */
    test_multiple_writes_no_reads();          /* CRITICAL — suspected bug */
    test_write_before_register();
    test_interleaved_read_write();
    test_nonblocking_poll_after_write();
    test_large_write_value();
    test_mod_then_write();
    test_tokio_waker_pattern();
    test_eventfd_with_pipe();
    test_eventfd_wakes_among_sockets();

    TEST_DONE();
}
