/*
 * Exercise the shared DRM syncobj ABI through the Starry userspace boundary.
 * UAPI layouts and behavior follow Linux v7.2.3:
 * include/uapi/drm/drm.h and drivers/gpu/drm/drm_syncobj.c.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

struct drm_get_cap {
    uint64_t capability;
    uint64_t value;
};

struct drm_syncobj_create {
    uint32_t handle;
    uint32_t flags;
};

struct drm_syncobj_destroy {
    uint32_t handle;
    uint32_t pad;
};

struct drm_syncobj_handle {
    uint32_t handle;
    uint32_t flags;
    int32_t fd;
    uint32_t pad;
    uint64_t point;
};

struct drm_syncobj_wait {
    uint64_t handles;
    int64_t timeout_nsec;
    uint32_t count_handles;
    uint32_t flags;
    uint32_t first_signaled;
    uint32_t pad;
    uint64_t deadline_nsec;
};

struct drm_syncobj_timeline_wait {
    uint64_t handles;
    uint64_t points;
    int64_t timeout_nsec;
    uint32_t count_handles;
    uint32_t flags;
    uint32_t first_signaled;
    uint32_t pad;
    uint64_t deadline_nsec;
};

struct drm_syncobj_array {
    uint64_t handles;
    uint32_t count_handles;
    uint32_t pad;
};

struct drm_syncobj_timeline_array {
    uint64_t handles;
    uint64_t points;
    uint32_t count_handles;
    uint32_t flags;
};

struct drm_syncobj_eventfd {
    uint32_t handle;
    uint32_t flags;
    uint64_t point;
    int32_t fd;
    uint32_t pad;
};

struct sync_file_info {
    char name[32];
    int32_t status;
    uint32_t flags;
    uint32_t num_fences;
    uint32_t pad;
    uint64_t sync_fence_info;
};

#define SYNC_IOC_FILE_INFO _IOWR('>', 4, struct sync_file_info)
_Static_assert(sizeof(struct sync_file_info) == 56, "Linux sync_file info ABI");

_Static_assert(sizeof(struct drm_syncobj_create) == 8, "Linux syncobj create ABI");
_Static_assert(sizeof(struct drm_syncobj_handle) == 24, "Linux syncobj fd ABI");
_Static_assert(sizeof(struct drm_syncobj_wait) == 40, "Linux syncobj wait ABI");
_Static_assert(sizeof(struct drm_syncobj_timeline_wait) == 48,
               "Linux timeline wait ABI");
_Static_assert(sizeof(struct drm_syncobj_array) == 16, "Linux syncobj array ABI");
_Static_assert(sizeof(struct drm_syncobj_timeline_array) == 24,
               "Linux timeline array ABI");
_Static_assert(sizeof(struct drm_syncobj_eventfd) == 24,
               "Linux syncobj eventfd ABI");

#define DRM_IOCTL_GET_CAP                  _IOWR('d', 0x0c, struct drm_get_cap)
#define DRM_IOCTL_SYNCOBJ_CREATE           _IOWR('d', 0xbf, struct drm_syncobj_create)
#define DRM_IOCTL_SYNCOBJ_DESTROY          _IOWR('d', 0xc0, struct drm_syncobj_destroy)
#define DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD     _IOWR('d', 0xc1, struct drm_syncobj_handle)
#define DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE     _IOWR('d', 0xc2, struct drm_syncobj_handle)
#define DRM_IOCTL_SYNCOBJ_WAIT             _IOWR('d', 0xc3, struct drm_syncobj_wait)
#define DRM_IOCTL_SYNCOBJ_SIGNAL           _IOWR('d', 0xc5, struct drm_syncobj_array)
#define DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT    _IOWR('d', 0xca, struct drm_syncobj_timeline_wait)
#define DRM_IOCTL_SYNCOBJ_QUERY            _IOWR('d', 0xcb, struct drm_syncobj_timeline_array)
#define DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL  _IOWR('d', 0xcd, struct drm_syncobj_timeline_array)
#define DRM_IOCTL_SYNCOBJ_EVENTFD          _IOWR('d', 0xcf, struct drm_syncobj_eventfd)

#define DRM_CAP_SYNCOBJ                    0x13
#define DRM_CAP_SYNCOBJ_TIMELINE           0x14
#define DRM_SYNCOBJ_HANDLE_TO_FD_EXPORT_SYNC_FILE (1u << 0)
#define DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT (1u << 1)
#define DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE  (1u << 2)

static unsigned int passed;
static unsigned int failed;

#define CHECK(condition, message) do { \
    if (condition) { \
        printf("  PASS | %s\n", message); \
        passed++; \
    } else { \
        printf("  FAIL | %s | errno=%d (%s)\n", message, errno, strerror(errno)); \
        failed++; \
    } \
} while (0)

#define CHECK_RET(call, expected, message) do { \
    errno = 0; \
    long result_ = (long)(call); \
    if (result_ == (long)(expected)) { \
        printf("  PASS | %s\n", message); \
        passed++; \
    } else { \
        printf("  FAIL | %s | expected=%ld got=%ld errno=%d (%s)\n", \
               message, (long)(expected), result_, errno, strerror(errno)); \
        failed++; \
    } \
} while (0)

#define CHECK_ERR(call, expected_errno, message) do { \
    errno = 0; \
    long result_ = (long)(call); \
    if (result_ == -1 && errno == (expected_errno)) { \
        printf("  PASS | %s\n", message); \
        passed++; \
    } else { \
        printf("  FAIL | %s | expected errno=%d got ret=%ld errno=%d (%s)\n", \
               message, (int)(expected_errno), result_, errno, strerror(errno)); \
        failed++; \
    } \
} while (0)

static int create_syncobj(int drm_fd, uint32_t flags)
{
    struct drm_syncobj_create create = { .flags = flags };
    if (ioctl(drm_fd, DRM_IOCTL_SYNCOBJ_CREATE, &create) < 0) {
        fprintf(stderr, "FAIL | SYNCOBJ_CREATE flags=%u errno=%d (%s)\n",
                flags, errno, strerror(errno));
        failed++;
        return 0;
    }
    CHECK(create.handle != 0, "SYNCOBJ_CREATE returns a nonzero handle");
    return (int)create.handle;
}

static void destroy_syncobj(int drm_fd, uint32_t handle)
{
    struct drm_syncobj_destroy destroy = { .handle = handle };
    CHECK_RET(ioctl(drm_fd, DRM_IOCTL_SYNCOBJ_DESTROY, &destroy), 0,
              "SYNCOBJ_DESTROY succeeds");
}

static void check_syncobj_wait(int drm_fd, uint32_t handle, uint32_t flags,
                               int expected_errno, const char *message)
{
    struct drm_syncobj_wait wait = {
        .handles = (uint64_t)(uintptr_t)&handle,
        .timeout_nsec = 0,
        .count_handles = 1,
        .flags = flags,
    };
    if (expected_errno) {
        CHECK_ERR(ioctl(drm_fd, DRM_IOCTL_SYNCOBJ_WAIT, &wait), expected_errno,
                  message);
    } else {
        CHECK_RET(ioctl(drm_fd, DRM_IOCTL_SYNCOBJ_WAIT, &wait), 0, message);
    }
}

static int64_t monotonic_ns(void)
{
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0)
        return -1;
    return (int64_t)now.tv_sec * INT64_C(1000000000) + now.tv_nsec;
}

static void interrupted_wait(int signo)
{
    (void)signo;
}

/* After the readiness byte the child executes only the target ioctl. Observe
 * it sleeping through procfs before producing the event; no delay is used to
 * guess whether it has entered the wait. The ioctl deadline bounds broken
 * implementations which ignore wakeups or signals. */
static int child_is_sleeping(pid_t child)
{
    char path[64], contents[512];
    snprintf(path, sizeof(path), "/proc/%ld/stat", (long)child);
    int64_t deadline = monotonic_ns() + INT64_C(2000000000);
    while (monotonic_ns() < deadline) {
        int fd = open(path, O_RDONLY | O_CLOEXEC);
        if (fd < 0)
            return 0;
        ssize_t size = read(fd, contents, sizeof(contents) - 1);
        close(fd);
        if (size > 0) {
            contents[size] = '\0';
            char *end_name = strrchr(contents, ')');
            if (end_name && end_name[1] == ' ' && end_name[2] == 'S')
                return 1;
        }
        sched_yield();
    }
    return 0;
}

static void check_blocking_lifecycle(int drm_fd)
{
    for (int interrupt = 0; interrupt != 2; ++interrupt) {
        int created = create_syncobj(drm_fd, 0);
        if (!created)
            return;
        uint32_t handle = (uint32_t)created;
        int communication[2];
        if (pipe2(communication, O_CLOEXEC) != 0) {
            CHECK(0, "create syncobj wait coordination pipe");
            destroy_syncobj(drm_fd, handle);
            return;
        }
        pid_t child = fork();
        if (child == 0) {
            close(communication[0]);
            struct sigaction action = { .sa_handler = interrupted_wait };
            sigemptyset(&action.sa_mask);
            if (sigaction(SIGUSR1, &action, NULL) != 0)
                _exit(2);
            struct drm_syncobj_wait wait = {
                .handles = (uint64_t)(uintptr_t)&handle,
                .timeout_nsec = monotonic_ns() + INT64_C(5000000000),
                .count_handles = 1,
                .flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT,
            };
            if (write(communication[1], "R", 1) != 1)
                _exit(3);
            errno = 0;
            long result = syscall(SYS_ioctl, drm_fd, DRM_IOCTL_SYNCOBJ_WAIT, &wait);
            int status[2] = { (int)result, errno };
            if (write(communication[1], status, sizeof(status)) != sizeof(status))
                _exit(4);
            _exit(0);
        }
        close(communication[1]);
        CHECK(child >= 0, "fork shares the DRM open-file syncobj handles");
        if (child < 0) {
            close(communication[0]);
            destroy_syncobj(drm_fd, handle);
            return;
        }
        char ready = 0;
        CHECK(read(communication[0], &ready, 1) == 1 && ready == 'R',
              "child is ready for blocking syncobj wait");
        int sleeping = child_is_sleeping(child);
        CHECK(sleeping, "syncobj waiter actually blocks before the event");
        if (interrupt) {
            CHECK_RET(syscall(SYS_kill, child, SIGUSR1), 0,
                      "deliver non-restarting signal to blocked syncobj waiter");
        } else {
            struct drm_syncobj_array signal = {
                .handles = (uint64_t)(uintptr_t)&handle,
                .count_handles = 1,
            };
            CHECK_RET(syscall(SYS_ioctl, drm_fd, DRM_IOCTL_SYNCOBJ_SIGNAL, &signal), 0,
                      "parent submits and signals the shared syncobj");
        }
        struct pollfd result_fd = { .fd = communication[0], .events = POLLIN };
        CHECK_RET(poll(&result_fd, 1, 1000), 1,
                  "blocked syncobj wait returns promptly after event");
        int result[2] = { -2, 0 };
        CHECK(read(communication[0], result, sizeof(result)) == sizeof(result),
              "receive wait result from child");
        if (interrupt ? result[0] != -1 || result[1] != EINTR : result[0] != 0)
            fprintf(stderr, "  wait result: ret=%d errno=%d interrupt=%d\n",
                    result[0], result[1], interrupt);
        CHECK(interrupt ? result[0] == -1 && result[1] == EINTR : result[0] == 0,
              interrupt ? "WAIT_FOR_SUBMIT reports EINTR for a signal" :
                          "WAIT_FOR_SUBMIT observes cross-process submission");
        int status = 0;
        CHECK(waitpid(child, &status, 0) == child && WIFEXITED(status) &&
              WEXITSTATUS(status) == 0, "reap syncobj waiter without leaked children");
        close(communication[0]);
        destroy_syncobj(drm_fd, handle);
    }
}

int main(void)
{
    puts("================================================");
    puts("  TEST: drm-syncobj-lifecycle");

    int drm_fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
    CHECK(drm_fd >= 0, "open /dev/dri/card0");
    if (drm_fd < 0)
        goto done;

    struct drm_get_cap cap = { .capability = DRM_CAP_SYNCOBJ };
    CHECK_RET(ioctl(drm_fd, DRM_IOCTL_GET_CAP, &cap), 0, "GET_CAP SYNCOBJ");
    CHECK(cap.value == 1, "SYNCOBJ capability is advertised");
    cap.capability = DRM_CAP_SYNCOBJ_TIMELINE;
    cap.value = 0;
    CHECK_RET(ioctl(drm_fd, DRM_IOCTL_GET_CAP, &cap), 0,
              "GET_CAP SYNCOBJ_TIMELINE");
    CHECK(cap.value == 1, "SYNCOBJ_TIMELINE capability is advertised");

    /* Syncobj ioctls are DRM_RENDER_ALLOW in Linux; they must work without
     * KMS/master privileges on the render node as well. */
    int render_fd = open("/dev/dri/renderD128", O_RDWR | O_CLOEXEC);
    CHECK(render_fd >= 0, "open /dev/dri/renderD128");
    if (render_fd >= 0) {
        cap.capability = DRM_CAP_SYNCOBJ;
        cap.value = 0;
        CHECK_RET(ioctl(render_fd, DRM_IOCTL_GET_CAP, &cap), 0,
                  "render node GET_CAP SYNCOBJ");
        CHECK(cap.value == 1, "render node advertises SYNCOBJ");
        int render_syncobj = create_syncobj(render_fd, 0);
        if (render_syncobj) {
            uint32_t handle = (uint32_t)render_syncobj;
            struct drm_syncobj_array signal = {
                .handles = (uint64_t)(uintptr_t)&handle,
                .count_handles = 1,
            };
            CHECK_RET(ioctl(render_fd, DRM_IOCTL_SYNCOBJ_SIGNAL, &signal), 0,
                      "render node can signal syncobj without DRM master");
            check_syncobj_wait(render_fd, handle, 0, 0,
                               "render-node syncobj wait sees signal");
            destroy_syncobj(render_fd, handle);
        }
        CHECK_RET(close(render_fd), 0, "close render node");
    }

    /* The syncobj fd keeps the object alive after the original GEM-style
     * per-file handle is destroyed; importing it creates another live handle. */
    int binary = create_syncobj(drm_fd, 0);
    if (binary) {
        check_syncobj_wait(drm_fd, (uint32_t)binary, 0, EINVAL,
                           "empty binary syncobj without WAIT_FOR_SUBMIT is EINVAL");
        check_syncobj_wait(drm_fd, (uint32_t)binary,
                           DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT, ETIME,
                           "WAIT_FOR_SUBMIT times out when no fence is submitted");

        struct drm_syncobj_handle export_handle = {
            .handle = (uint32_t)binary,
            .fd = -1,
        };
        CHECK_RET(ioctl(drm_fd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD,
                        &export_handle), 0, "HANDLE_TO_FD exports syncobj ref");
        int syncobj_fd = export_handle.fd;
        destroy_syncobj(drm_fd, (uint32_t)binary);

        struct drm_syncobj_handle import_handle = {
            .fd = syncobj_fd,
        };
        CHECK_RET(ioctl(drm_fd, DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE,
                        &import_handle), 0, "FD_TO_HANDLE imports retained syncobj");
        CHECK_RET(close(syncobj_fd), 0, "close exported syncobj fd");

        if (import_handle.handle) {
            uint32_t handle = import_handle.handle;
            struct drm_syncobj_array signal = {
                .handles = (uint64_t)(uintptr_t)&handle,
                .count_handles = 1,
            };
            CHECK_RET(ioctl(drm_fd, DRM_IOCTL_SYNCOBJ_SIGNAL, &signal), 0,
                      "SIGNAL imported binary syncobj");
            check_syncobj_wait(drm_fd, handle, 0, 0,
                               "imported handle remains valid after fd close");

            struct drm_syncobj_handle sync_file = {
                .handle = handle,
                .flags = DRM_SYNCOBJ_HANDLE_TO_FD_EXPORT_SYNC_FILE,
                .fd = -1,
            };
            CHECK_RET(ioctl(drm_fd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD,
                            &sync_file), 0, "HANDLE_TO_FD exports signaled sync_file");
            if (sync_file.fd >= 0) {
                struct pollfd pfd = { .fd = sync_file.fd, .events = POLLIN };
                CHECK_RET(poll(&pfd, 1, 0), 1,
                          "poll reports exported signaled sync_file ready");
                CHECK((pfd.revents & POLLIN) != 0,
                      "sync_file readiness includes POLLIN");
                struct sync_file_info info = {0};
                CHECK_RET(syscall(SYS_ioctl, sync_file.fd, SYNC_IOC_FILE_INFO, &info), 0,
                          "exported sync_file dispatches FILE_INFO ioctl");
                CHECK(info.status == 1 && info.num_fences == 1,
                      "FILE_INFO reports the actual signaled fence");
                CHECK_RET(close(sync_file.fd), 0, "close exported sync_file");
            }
            destroy_syncobj(drm_fd, handle);
        }
    }

    /* Timeline points are available only after submission and become waitable
     * when signalled; query reports the last signalled point. */
    int timeline = create_syncobj(drm_fd, 0);
    if (timeline) {
        uint32_t handle = (uint32_t)timeline;
        uint64_t point = 17;
        struct drm_syncobj_timeline_wait wait = {
            .handles = (uint64_t)(uintptr_t)&handle,
            .points = (uint64_t)(uintptr_t)&point,
            .timeout_nsec = 0,
            .count_handles = 1,
        };
        CHECK_ERR(ioctl(drm_fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &wait), EINVAL,
                  "unsubmitted timeline point without WAIT_AVAILABLE is EINVAL");
        wait.flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE;
        CHECK_ERR(ioctl(drm_fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &wait), ETIME,
                  "WAIT_AVAILABLE times out before timeline point submission");

        struct drm_syncobj_timeline_array signal = {
            .handles = (uint64_t)(uintptr_t)&handle,
            .points = (uint64_t)(uintptr_t)&point,
            .count_handles = 1,
        };
        CHECK_RET(ioctl(drm_fd, DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL, &signal), 0,
                  "TIMELINE_SIGNAL submits point 17");
        wait.flags = 0;
        CHECK_RET(ioctl(drm_fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &wait), 0,
                  "TIMELINE_WAIT succeeds at signaled point");

        uint64_t queried_point = 0;
        struct drm_syncobj_timeline_array query = {
            .handles = (uint64_t)(uintptr_t)&handle,
            .points = (uint64_t)(uintptr_t)&queried_point,
            .count_handles = 1,
        };
        CHECK_RET(ioctl(drm_fd, DRM_IOCTL_SYNCOBJ_QUERY, &query), 0,
                  "SYNCOBJ_QUERY reads timeline point");
        CHECK(queried_point == point, "query returns signaled timeline point");
        destroy_syncobj(drm_fd, handle);
    }

    check_blocking_lifecycle(drm_fd);

    /* eventfd registration is a one-shot notification for the binary fence. */
    int event = (int)syscall(SYS_eventfd2, 0, EFD_CLOEXEC | EFD_NONBLOCK);
    CHECK(event >= 0, "eventfd2 creates nonblocking notification fd");
    int event_syncobj = event >= 0 ? create_syncobj(drm_fd, 0) : 0;
    if (event_syncobj) {
        struct drm_syncobj_eventfd registration = {
            .handle = (uint32_t)event_syncobj,
            .fd = event,
        };
        CHECK_RET(ioctl(drm_fd, DRM_IOCTL_SYNCOBJ_EVENTFD, &registration), 0,
                  "SYNCOBJ_EVENTFD registers notification");

        struct pollfd pfd = { .fd = event, .events = POLLIN };
        CHECK_RET(poll(&pfd, 1, 0), 0,
                  "eventfd remains unreadable before syncobj signal");

        uint32_t handle = (uint32_t)event_syncobj;
        struct drm_syncobj_array signal = {
            .handles = (uint64_t)(uintptr_t)&handle,
            .count_handles = 1,
        };
        CHECK_RET(ioctl(drm_fd, DRM_IOCTL_SYNCOBJ_SIGNAL, &signal), 0,
                  "SIGNAL wakes registered eventfd");
        pfd.revents = 0;
        CHECK_RET(poll(&pfd, 1, 1000), 1,
                  "poll observes eventfd notification");
        CHECK((pfd.revents & POLLIN) != 0,
              "eventfd readiness includes POLLIN");
        uint64_t count = 0;
        CHECK_RET(read(event, &count, sizeof(count)), sizeof(count),
                  "read eventfd counter");
        CHECK(count == 1, "syncobj notification increments eventfd once");

        /* Kernel completion is allowed to saturate an eventfd beyond the
         * userspace-write limit. Waiting only for POLLERR avoids consuming
         * the preexisting readable value before the deferred signal runs. */
        count = UINT64_MAX - 1;
        CHECK_RET(write(event, &count, sizeof(count)), sizeof(count),
                  "fill eventfd to the userspace limit");
        CHECK_RET(ioctl(drm_fd, DRM_IOCTL_SYNCOBJ_EVENTFD, &registration), 0,
                  "register a completion on the full eventfd");
        pfd.events = 0;
        pfd.revents = 0;
        CHECK_RET(poll(&pfd, 1, 1000), 1,
                  "kernel completion wakes eventfd error-only observer");
        CHECK((pfd.revents & POLLERR) != 0,
              "saturated kernel eventfd counter reports POLLERR");
        count = 0;
        CHECK_RET(read(event, &count, sizeof(count)), sizeof(count),
                  "read saturated eventfd counter");
        CHECK(count == UINT64_MAX, "completion is retained at eventfd saturation");
        pfd.revents = 0;
        CHECK_RET(poll(&pfd, 1, 0), 0, "consuming eventfd saturation clears POLLERR");
        destroy_syncobj(drm_fd, handle);
    }
    if (event >= 0)
        CHECK_RET(close(event), 0, "close eventfd");

    CHECK_RET(close(drm_fd), 0, "close DRM device");

done:
    puts("------------------------------------------------");
    printf("  DONE: %u pass, %u fail\n", passed, failed);
    puts("================================================");
    return failed ? 1 : 0;
}
