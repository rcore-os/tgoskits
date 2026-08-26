#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

enum {
    ADDRESS_SPACE_HELD = 1,
    USER_COPY_COMPLETED = 2,
    EAGER_PREPARATION_OBSERVED = 3,
    COPY_BUFFER_SIZE = 4096,
};

static const char CONTROL_PATH[] = "/sys/kernel/debug/uaccess_lock_regression";
static atomic_int holder_finished;
static atomic_int holder_result;
static char copy_buffer[COPY_BUFFER_SIZE];
static int null_fd = -1;

static void *hold_address_space(void *argument)
{
    const int fd = *(const int *)argument;
    const ssize_t written = write(fd, "hold", 4);

    atomic_store_explicit(&holder_result, written == 4 ? 0 : errno ? errno : EIO,
                          memory_order_release);
    atomic_store_explicit(&holder_finished, 1, memory_order_release);
    return NULL;
}

static int observe_state(int fd)
{
    const off_t state = lseek(fd, 0, SEEK_END);

    if (state < 0) {
        perror("lseek control state");
        return -1;
    }
    return (int)state;
}

static int wait_until_address_space_is_held(int observer_fd)
{
    for (;;) {
        const int state = observe_state(observer_fd);

        if (state == ADDRESS_SPACE_HELD) {
            return 0;
        }
        if (state < 0 || atomic_load_explicit(&holder_finished, memory_order_acquire)) {
            fprintf(stderr, "FAIL: holder exited before publishing the held state\n");
            return -1;
        }
        sched_yield();
    }
}

static int verify_copy_result(const char *name, int observer_fd, pthread_t holder,
                              long syscall_result)
{
    const int join_error = pthread_join(holder, NULL);
    const int state = observe_state(observer_fd);
    const int hold_error = atomic_load_explicit(&holder_result, memory_order_acquire);

    if (join_error != 0 || hold_error != 0) {
        fprintf(stderr, "FAIL: %s holder failed: join=%d hold=%d\n", name, join_error,
                hold_error);
        return -1;
    }
    if (syscall_result < 0) {
        fprintf(stderr, "FAIL: %s syscall failed: errno=%d\n", name, errno);
        return -1;
    }
    if (state == EAGER_PREPARATION_OBSERVED) {
        fprintf(stderr,
                "FAIL: %s tried to acquire the process address-space lock before copying\n",
                name);
        return -1;
    }
    if (state != USER_COPY_COMPLETED) {
        fprintf(stderr, "FAIL: %s completed with unexpected control state %d\n", name,
                state);
        return -1;
    }
    return 0;
}

static int run_copy_phase(const char *name, int observer_fd, long (*copy_operation)(void))
{
    int holder_fd = open(CONTROL_PATH, O_RDWR | O_CLOEXEC);
    pthread_t holder;

    if (holder_fd < 0) {
        perror("open holder control");
        return -1;
    }
    atomic_store_explicit(&holder_finished, 0, memory_order_relaxed);
    atomic_store_explicit(&holder_result, 0, memory_order_relaxed);
    const int create_error = pthread_create(&holder, NULL, hold_address_space, &holder_fd);
    if (create_error != 0) {
        fprintf(stderr, "pthread_create: %d\n", create_error);
        close(holder_fd);
        return -1;
    }
    if (wait_until_address_space_is_held(observer_fd) != 0) {
        return -1;
    }

    const long result = copy_operation();
    const int verified = verify_copy_result(name, observer_fd, holder, result);
    close(holder_fd);
    return verified;
}

static long copy_current_directory_to_user(void)
{
    return syscall(SYS_getcwd, copy_buffer, sizeof(copy_buffer));
}

static long copy_user_buffer_to_null(void)
{
    return syscall(SYS_write, null_fd, copy_buffer, sizeof(copy_buffer));
}

int main(void)
{
    memset(copy_buffer, 0x5a, sizeof(copy_buffer));
    const int observer_fd = open(CONTROL_PATH, O_RDONLY | O_CLOEXEC);
    if (observer_fd < 0) {
        perror("open observer control");
        return 1;
    }
    null_fd = open("/dev/null", O_WRONLY | O_CLOEXEC);
    if (null_fd < 0) {
        perror("open /dev/null");
        close(observer_fd);
        return 1;
    }

    if (run_copy_phase("copy_to_user", observer_fd, copy_current_directory_to_user) != 0) {
        close(null_fd);
        close(observer_fd);
        return 1;
    }
    puts("PASS: copy_to_user completed while another thread held the address-space lock");
    if (run_copy_phase("copy_from_user", observer_fd, copy_user_buffer_to_null) != 0) {
        close(null_fd);
        close(observer_fd);
        return 1;
    }

    close(null_fd);
    close(observer_fd);
    puts("PASS: ordinary user copies do not serialize on the process address-space lock");
    return 0;
}
