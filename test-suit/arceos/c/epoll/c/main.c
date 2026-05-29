#include <assert.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <unistd.h>

static void expect_errno_int(int ret, int err)
{
    assert(ret == -1);
    assert(errno == err);
}

static void expect_errno_ssize(ssize_t ret, int err)
{
    assert(ret == -1);
    assert(errno == err);
}

static struct epoll_event make_event(uint32_t events, int fd)
{
    struct epoll_event event;
    memset(&event, 0, sizeof(event));
    event.events = events;
    event.data.fd = fd;
    return event;
}

static void expect_no_event(int epfd)
{
    struct epoll_event event;
    memset(&event, 0, sizeof(event));
    assert(epoll_wait(epfd, &event, 1, 0) == 0);
}

static void expect_one_event(int epfd, uint32_t events, int fd)
{
    struct epoll_event event;
    memset(&event, 0, sizeof(event));
    assert(epoll_wait(epfd, &event, 1, 0) == 1);
    assert(event.data.fd == fd);
    assert((event.events & events) == events);
}

static void test_epoll_arguments(void)
{
    struct epoll_event event = make_event(EPOLLIN, 0);
    char byte = 0;
    int epfd;
    int pipefd[2];

    errno = 0;
    expect_errno_int(epoll_create(0), EINVAL);

    errno = 0;
    expect_errno_int(epoll_create1(EPOLL_NONBLOCK), EINVAL);

    epfd = epoll_create1(EPOLL_CLOEXEC);
    assert(epfd >= 0);

    errno = 0;
    expect_errno_int(epoll_wait(epfd, &event, 0, 0), EINVAL);

    errno = 0;
    expect_errno_int(epoll_wait(epfd, NULL, 1, 0), EFAULT);

    errno = 0;
    expect_errno_ssize(read(epfd, &byte, sizeof(byte)), EINVAL);

    errno = 0;
    expect_errno_ssize(write(epfd, "x", 1), EINVAL);

    assert(pipe(pipefd) == 0);

    errno = 0;
    expect_errno_int(epoll_ctl(epfd, EPOLL_CTL_ADD, pipefd[0], NULL), EFAULT);

    errno = 0;
    expect_errno_int(epoll_ctl(epfd, EPOLL_CTL_ADD, epfd, &event), ELOOP);

    close(pipefd[0]);
    close(pipefd[1]);
    close(epfd);
}

static void test_level_triggered_epoll(int epfd, int write_fd)
{
    struct epoll_event event = make_event(EPOLLOUT, write_fd);

    assert(epoll_ctl(epfd, EPOLL_CTL_ADD, write_fd, &event) == 0);
    expect_one_event(epfd, EPOLLOUT, write_fd);
    expect_one_event(epfd, EPOLLOUT, write_fd);
    assert(epoll_ctl(epfd, EPOLL_CTL_DEL, write_fd, NULL) == 0);
    expect_no_event(epfd);
}

static void test_edge_triggered_epoll(int epfd, int read_fd, int write_fd)
{
    char buf[4];
    struct epoll_event event = make_event(EPOLLIN | EPOLLET, read_fd);

    assert(epoll_ctl(epfd, EPOLL_CTL_ADD, read_fd, &event) == 0);
    expect_no_event(epfd);

    assert(write(write_fd, "abc", 3) == 3);
    expect_one_event(epfd, EPOLLIN, read_fd);
    expect_no_event(epfd);

    assert(read(read_fd, buf, 3) == 3);
    expect_no_event(epfd);

    assert(write(write_fd, "d", 1) == 1);
    expect_one_event(epfd, EPOLLIN, read_fd);
    assert(epoll_ctl(epfd, EPOLL_CTL_DEL, read_fd, NULL) == 0);
    assert(read(read_fd, buf, 1) == 1);
    expect_no_event(epfd);
}

static void test_oneshot_epoll(int epfd, int read_fd, int write_fd)
{
    char buf[1];
    struct epoll_event event = make_event(EPOLLIN | EPOLLONESHOT, read_fd);

    assert(epoll_ctl(epfd, EPOLL_CTL_ADD, read_fd, &event) == 0);
    assert(write(write_fd, "z", 1) == 1);

    expect_one_event(epfd, EPOLLIN, read_fd);
    expect_no_event(epfd);

    assert(epoll_ctl(epfd, EPOLL_CTL_MOD, read_fd, &event) == 0);
    expect_one_event(epfd, EPOLLIN, read_fd);

    assert(read(read_fd, buf, 1) == 1);
    assert(epoll_ctl(epfd, EPOLL_CTL_DEL, read_fd, NULL) == 0);
    expect_no_event(epfd);
}

void main()
{
    int epfd;
    int pipefd[2];

    test_epoll_arguments();

    epfd = epoll_create(1);
    assert(epfd >= 0);
    assert(pipe(pipefd) == 0);

    test_level_triggered_epoll(epfd, pipefd[1]);
    test_edge_triggered_epoll(epfd, pipefd[0], pipefd[1]);
    test_oneshot_epoll(epfd, pipefd[0], pipefd[1]);

    close(pipefd[0]);
    close(pipefd[1]);
    close(epfd);

    puts("(C)Epoll tests run OK");
}
