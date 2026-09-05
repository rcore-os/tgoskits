#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <unistd.h>

static int failures;
#define CHECK(condition, name) do { \
    if (condition) printf("PASS: %s\n", name); \
    else { printf("FAIL: %s (errno=%d)\n", name, errno); failures++; } \
} while (0)
#define REQUIRE(condition) do { if (!(condition)) { perror(#condition); exit(1); } } while (0)

static void socket_pair(int pair[2])
{
    REQUIRE(socketpair(AF_UNIX, SOCK_DGRAM | SOCK_NONBLOCK, 0, pair) == 0);
}

static void *two_pages(size_t page_size)
{
    void *pages = mmap(NULL, page_size * 2, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(pages != MAP_FAILED);
    return pages;
}

static void output_fault_follows_socket_operation(void)
{
    struct sockaddr_storage address;
    socklen_t *bad_length = (socklen_t *)(uintptr_t)1;
    errno = 0;
    long result = syscall(SYS_accept4, -1, &address, bad_length, 0);
    CHECK(result == -1 && errno == EBADF, "accept4 validates fd before output length");
    errno = 0;
    result = syscall(SYS_getsockname, -1, &address, bad_length);
    CHECK(result == -1 && errno == EBADF, "getsockname validates fd before output length");
    errno = 0;
    result = syscall(SYS_getpeername, -1, &address, bad_length);
    CHECK(result == -1 && errno == EBADF, "getpeername validates fd before output length");

    int unconnected = socket(AF_INET, SOCK_STREAM, 0);
    REQUIRE(unconnected >= 0);
    errno = 0;
    result = syscall(SYS_getpeername, unconnected, &address, bad_length);
    CHECK(result == -1 && errno == ENOTCONN, "getpeername checks connection before copyout");
    close(unconnected);

    int pair[2];
    socket_pair(pair);
    char byte = 0;
    errno = 0;
    result = syscall(SYS_recvfrom, pair[1], &byte, 1, MSG_DONTWAIT, &address, bad_length);
    CHECK(result == -1 && errno == EAGAIN, "empty recvfrom precedes output-length fault");
    REQUIRE(send(pair[0], "x", 1, 0) == 1);
    errno = 0;
    result = syscall(SYS_recvfrom, pair[1], &byte, 1, MSG_DONTWAIT, &address, bad_length);
    CHECK(result == -1 && errno == EFAULT, "recvfrom reports bad output length after receive");
    errno = 0;
    result = recv(pair[1], &byte, 1, MSG_DONTWAIT);
    CHECK(result == -1 && errno == EAGAIN, "faulting recvfrom already consumed its datagram");
    close(pair[0]);
    close(pair[1]);
}

static void mmsg_keeps_completed_prefix(size_t page_size, int receive)
{
    int pair[2];
    socket_pair(pair);
    unsigned char *pages = two_pages(page_size);
    struct mmsghdr *messages = (struct mmsghdr *)(pages + page_size - sizeof(struct mmsghdr));
    char byte = 'p';
    struct iovec iov = { .iov_base = &byte, .iov_len = 1 };
    memset(messages, 0, sizeof(*messages));
    messages[0].msg_hdr.msg_iov = &iov;
    messages[0].msg_hdr.msg_iovlen = 1;
    REQUIRE(mprotect(pages + page_size, page_size, PROT_NONE) == 0);
    if (receive) REQUIRE(send(pair[0], "r", 1, 0) == 1);
    long result = receive ? syscall(SYS_recvmmsg, pair[1], messages, 2, MSG_DONTWAIT, NULL)
                          : syscall(SYS_sendmmsg, pair[0], messages, 2, 0);
    CHECK(result == 1, receive ? "recvmmsg keeps prefix after second header fault"
                               : "sendmmsg keeps prefix after second header fault");
    CHECK(messages[0].msg_len == 1, "completed mmsg length is visible");
    munmap(pages, page_size * 2);
    close(pair[0]);
    close(pair[1]);
}

static void sendmmsg_writes_only_result(size_t page_size)
{
    int pair[2];
    socket_pair(pair);
    unsigned char *pages = two_pages(page_size);
    struct mmsghdr *message = (struct mmsghdr *)(pages + page_size - offsetof(struct mmsghdr, msg_len));
    char byte = 's';
    struct iovec iov = { .iov_base = &byte, .iov_len = 1 };
    memset(message, 0, sizeof(*message));
    message->msg_hdr.msg_iov = &iov;
    message->msg_hdr.msg_iovlen = 1;
    REQUIRE(mprotect(pages, page_size, PROT_READ) == 0);
    CHECK(syscall(SYS_sendmmsg, pair[0], message, 1, 0) == 1,
          "sendmmsg accepts read-only input fields with writable msg_len");
    CHECK(message->msg_len == 1, "sendmmsg writes msg_len independently");
    munmap(pages, page_size * 2);
    close(pair[0]);
    close(pair[1]);
}

static void recvmsg_writes_only_results(size_t page_size, int multiple)
{
    int pair[2];
    socket_pair(pair);
    unsigned char *pages = two_pages(page_size);
    struct mmsghdr *message = (struct mmsghdr *)(pages + page_size - sizeof(void *));
    char byte = 0;
    struct iovec iov = { .iov_base = &byte, .iov_len = 1 };
    memset(message, 0, sizeof(*message));
    message->msg_hdr.msg_iov = &iov;
    message->msg_hdr.msg_iovlen = 1;
    REQUIRE(mprotect(pages, page_size, PROT_READ) == 0);
    REQUIRE(send(pair[0], "r", 1, 0) == 1);
    long result = multiple ? syscall(SYS_recvmmsg, pair[1], message, 1, MSG_DONTWAIT, NULL)
                           : syscall(SYS_recvmsg, pair[1], &message->msg_hdr, MSG_DONTWAIT);
    CHECK(result == 1 && byte == 'r', multiple ? "recvmmsg preserves read-only msg_name input"
                                              : "recvmsg preserves read-only msg_name input");
    munmap(pages, page_size * 2);
    close(pair[0]);
    close(pair[1]);
}

static int fd_count(void)
{
    int count = 0;
    for (int fd = 0; fd < 1024; fd++) if (fcntl(fd, F_GETFD) >= 0) count++;
    return count;
}

static void socketpair_copyout_failure_preserves_fd_table(size_t page_size)
{
    void *output = mmap(NULL, page_size, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(output != MAP_FAILED);
    int before = fd_count();
    errno = 0;
    long result = syscall(SYS_socketpair, AF_UNIX, SOCK_DGRAM, 0, output);
    CHECK(result == -1 && errno == EFAULT, "socketpair reports failed result copyout");
    CHECK(fd_count() == before, "socketpair publishes no descriptors after failed copyout");
    munmap(output, page_size);
}

static void scm_rights_fault_does_not_install_fd(size_t page_size)
{
    int pair[2];
    socket_pair(pair);
    int source = open("/dev/null", O_RDONLY);
    REQUIRE(source >= 0);
    char byte = 'f';
    struct iovec iov = { .iov_base = &byte, .iov_len = 1 };
    union { struct cmsghdr alignment; unsigned char bytes[CMSG_SPACE(sizeof(int))]; } control;
    memset(&control, 0, sizeof(control));
    struct msghdr send_header = { .msg_iov = &iov, .msg_iovlen = 1,
                                 .msg_control = control.bytes, .msg_controllen = sizeof(control.bytes) };
    struct cmsghdr *header = CMSG_FIRSTHDR(&send_header);
    header->cmsg_level = SOL_SOCKET;
    header->cmsg_type = SCM_RIGHTS;
    header->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(header), &source, sizeof(source));
    REQUIRE(sendmsg(pair[0], &send_header, 0) == 1);
    void *bad_control = mmap(NULL, page_size, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(bad_control != MAP_FAILED);
    struct msghdr receive_header = { .msg_iov = &iov, .msg_iovlen = 1,
                                    .msg_control = bad_control, .msg_controllen = sizeof(control.bytes) };
    int before = fd_count();
    long result = syscall(SYS_recvmsg, pair[1], &receive_header, MSG_DONTWAIT);
    CHECK(result == 1 && (receive_header.msg_flags & MSG_CTRUNC),
          "SCM_RIGHTS copy fault truncates ancillary data and preserves payload");
    CHECK(fd_count() == before, "SCM_RIGHTS copy fault installs no unreachable fd");
    munmap(bad_control, page_size);
    close(source);
    close(pair[0]);
    close(pair[1]);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    size_t page_size = (size_t)sysconf(_SC_PAGESIZE);
    REQUIRE(page_size >= sizeof(struct mmsghdr));
    output_fault_follows_socket_operation();
    mmsg_keeps_completed_prefix(page_size, 0);
    mmsg_keeps_completed_prefix(page_size, 1);
    sendmmsg_writes_only_result(page_size);
    recvmsg_writes_only_results(page_size, 0);
    recvmsg_writes_only_results(page_size, 1);
    scm_rights_fault_does_not_install_fd(page_size);
    socketpair_copyout_failure_preserves_fd_table(page_size);
    printf("USERCOPY_SOCKET_RESULTS_%s failures=%d\n", failures ? "FAILED" : "PASSED", failures);
    return failures != 0;
}
