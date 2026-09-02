#define _GNU_SOURCE

#include <errno.h>
#include <poll.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

enum {
    CHILD_STACK_SIZE = 64 * 1024,
    WAIT_TIMEOUT_MS = 5000,
};

struct clone_child_args {
    int go_fd;
    int done_fd;
    volatile unsigned char *shared_page;
};

struct ready_message {
    pid_t child_pid;
};

static int write_all(int fd, const void *buffer, size_t length)
{
    const unsigned char *cursor = buffer;
    while (length != 0) {
        ssize_t written = write(fd, cursor, length);
        if (written < 0 && errno == EINTR) {
            continue;
        }
        if (written <= 0) {
            return -1;
        }
        cursor += (size_t)written;
        length -= (size_t)written;
    }
    return 0;
}

static int read_all(int fd, void *buffer, size_t length)
{
    unsigned char *cursor = buffer;
    while (length != 0) {
        ssize_t received = read(fd, cursor, length);
        if (received < 0 && errno == EINTR) {
            continue;
        }
        if (received <= 0) {
            return -1;
        }
        cursor += (size_t)received;
        length -= (size_t)received;
    }
    return 0;
}

static int read_byte_with_timeout(int fd, unsigned char *value)
{
    struct pollfd descriptor = {
        .fd = fd,
        .events = POLLIN,
    };
    int ready;
    do {
        ready = poll(&descriptor, 1, WAIT_TIMEOUT_MS);
    } while (ready < 0 && errno == EINTR);
    if (ready != 1 || (descriptor.revents & POLLIN) == 0) {
        return -1;
    }
    return read_all(fd, value, 1);
}

static int clone_child_main(void *opaque)
{
    struct clone_child_args *args = opaque;
    unsigned char command = 0;
    if (read_all(args->go_fd, &command, 1) != 0 || command != 0xa5) {
        _exit(20);
    }

    args->shared_page[0] = 0x5a;
    __sync_synchronize();
    command = 0x3c;
    if (write_all(args->done_fd, &command, 1) != 0) {
        _exit(21);
    }
    _exit(0);
}

static void run_mm_owner(int ready_fd, int go_fd, int done_fd,
                         volatile unsigned char *shared_page)
{
    void *child_stack = mmap(NULL, CHILD_STACK_SIZE, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
    struct clone_child_args *args = mmap(NULL, (size_t)sysconf(_SC_PAGESIZE),
                                         PROT_READ | PROT_WRITE,
                                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (child_stack == MAP_FAILED || args == MAP_FAILED) {
        _exit(10);
    }

    args->go_fd = go_fd;
    args->done_fd = done_fd;
    args->shared_page = shared_page;
    void *stack_top = (unsigned char *)child_stack + CHILD_STACK_SIZE;
    pid_t child = clone(clone_child_main, stack_top, CLONE_VM | SIGCHLD, args);
    if (child < 0) {
        _exit(11);
    }

    struct ready_message message = {.child_pid = child};
    if (write_all(ready_fd, &message, sizeof(message)) != 0) {
        kill(child, SIGKILL);
        _exit(12);
    }

    /* The CLONE_VM child keeps this MM live after this process owner exits. */
    _exit(0);
}

int main(void)
{
    int ready_pipe[2];
    int go_pipe[2];
    int done_pipe[2];
    if (pipe(ready_pipe) != 0 || pipe(go_pipe) != 0 || pipe(done_pipe) != 0) {
        perror("pipe");
        return 1;
    }

    size_t page_size = (size_t)sysconf(_SC_PAGESIZE);
    volatile unsigned char *shared_page = mmap(
        NULL, page_size, PROT_READ | PROT_WRITE,
        MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (shared_page == MAP_FAILED) {
        perror("mmap shared observation page");
        return 1;
    }
    shared_page[0] = 0x11;

    pid_t owner = fork();
    if (owner < 0) {
        perror("fork MM owner");
        return 1;
    }
    if (owner == 0) {
        close(ready_pipe[0]);
        close(go_pipe[1]);
        close(done_pipe[0]);
        run_mm_owner(ready_pipe[1], go_pipe[0], done_pipe[1], shared_page);
    }

    close(ready_pipe[1]);
    close(go_pipe[0]);
    close(done_pipe[1]);

    struct ready_message message;
    if (read_all(ready_pipe[0], &message, sizeof(message)) != 0) {
        fprintf(stderr, "MM lifecycle: owner did not publish clone child\n");
        return 1;
    }

    int owner_status = 0;
    if (waitpid(owner, &owner_status, 0) != owner
        || !WIFEXITED(owner_status) || WEXITSTATUS(owner_status) != 0) {
        fprintf(stderr, "MM lifecycle: owner failed before releasing its MmHandle\n");
        kill(message.child_pid, SIGKILL);
        return 1;
    }

    unsigned char command = 0xa5;
    if (write_all(go_pipe[1], &command, 1) != 0) {
        perror("release CLONE_VM child");
        kill(message.child_pid, SIGKILL);
        return 1;
    }

    unsigned char completion = 0;
    if (read_byte_with_timeout(done_pipe[0], &completion) != 0
        || completion != 0x3c || shared_page[0] != 0x5a) {
        fprintf(stderr,
                "MM lifecycle: CLONE_VM child lost its MM after owner exit "
                "(completion=%#x, page=%#x)\n",
                completion, shared_page[0]);
        kill(message.child_pid, SIGKILL);
        return 1;
    }

    close(ready_pipe[0]);
    close(go_pipe[1]);
    close(done_pipe[0]);
    munmap((void *)shared_page, page_size);
    printf("MM_LIFECYCLE_CLONE_VM_OWNER_EXIT_PASSED\n");
    return 0;
}
