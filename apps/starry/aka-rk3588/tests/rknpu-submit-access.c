/* Run as root on Starry with RKNPU. No valid hardware job is submitted here. */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <unistd.h>

struct subcore { uint32_t start, count; };
struct submit {
    uint32_t flags, timeout, start, count, counter;
    int32_t priority;
    uint64_t object;
    uint32_t domain, reserved;
    uint64_t base;
    int64_t elapsed;
    uint32_t mask;
    int32_t fence;
    struct subcore cores[5];
};
struct create {
    uint32_t handle, flags;
    uint64_t size, object, dma, sram;
    int32_t domain;
    uint32_t mask;
};
#define SUBMIT _IOWR('d', 0x41, struct submit)
#define CREATE _IOWR('d', 0x42, struct create)

static void rejected(int fd, struct submit *job, int expected, const char *where) {
    errno = 0;
    int result = ioctl(fd, SUBMIT, job);
    if (result != -1 || errno != expected) {
        fprintf(stderr, "RKNPU_ACCESS_FAIL %s ret=%d errno=%d expected=%d\n",
                where, result, errno, expected);
        exit(1);
    }
}
int main(void) {
    int fd = open("/dev/dri/card1", O_RDWR);
    int other = open("/dev/dri/card1", O_RDWR);
    if (fd < 0 || other < 0) { perror("card1"); return 1; }
    struct submit job = {.flags=1, .timeout=100, .count=1, .object=1, .mask=1};
    rejected(fd, &job, EFAULT, "unknown object");
    struct create gem = {.size=4096};
    if (ioctl(fd, CREATE, &gem) != 0) { perror("create"); return 1; }
    job.object = gem.object;
    rejected(other, &job, EFAULT, "foreign open object");
    job.object = gem.object + gem.size - 1;
    rejected(fd, &job, EFAULT, "task crosses allocation");
    pid_t child = fork();
    if (child == 0) {
        if (setuid(1000)) { perror("setuid"); _exit(1); }
        job.object = gem.object;
        rejected(fd, &job, EPERM, "inherited fd after dropping privilege");
        _exit(0);
    }
    int status = 0;
    if (child < 0 || waitpid(child, &status, 0) != child ||
        !WIFEXITED(status) || WEXITSTATUS(status) != 0) return 1;
    close(other);
    close(fd);
    puts("RKNPU_SUBMIT_ACCESS_PASS");
    return 0;
}
