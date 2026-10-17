/*
 * test-drm-syncobj — card0 syncobj 家族与 Linux UAPI 语义一致性测试
 *
 * 覆盖（对齐 Linux drm_syncobj.c 的可判定语义，全部无需 3D/venus 宿主）：
 *   - SYNCOBJ_CREATE / TIMELINE_SIGNAL / TIMELINE_QUERY 水位往返
 *   - TIMELINE_WAIT：绝对 CLOCK_MONOTONIC 截止（过期立即 ETIME、未来截止
 *     按真实剩余时间等待）、已信号点先查状态再判超时、ETIME 而非 ETIMEDOUT、
 *     未知 flag EINVAL、空句柄列表成功、未知句柄 ENOENT、
 *     无 WAIT_FOR_SUBMIT 且点未提交 → EINVAL
 *   - 二进制 WAIT：WAIT_AVAILABLE 非法（EINVAL）、空列表成功、未提交 → EINVAL、
 *     非 WAIT_ALL 成功时写回 first_signaled
 *   - HANDLE_TO_FD / FD_TO_HANDLE：flag/pad/point 校验、未知句柄 ENOENT、
 *     同步文件导出导入往返（含 TIMELINE 点位导出导入）、坏 fd EINVAL
 *   - RESET / SIGNAL 水位清零与置一
 *   - EVENTFD：无 WAIT_AVAILABLE 且点未背书 → EINVAL；
 *     TIMELINE_SIGNAL 与二进制 SIGNAL 两条路径都唤醒等待者
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <sys/eventfd.h>
#include <sys/ioctl.h>
#include <time.h>
#include <unistd.h>

/* ---- drm syncobj UAPI ----
 * 独立转写自 Linux include/uapi/drm/drm.h（linux master，v7.2 窗口），
 * 不参照内核侧 drm.rs —— 本套件的价值在于独立性：内核侧布局漂移会
 * 直接表现为命令字不匹配（ENOTTY）或字段错位，而不是两边一致地错。 */

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
struct drm_syncobj_array {
    uint64_t handles;
    uint32_t count_handles;
    uint32_t pad;
};
struct drm_syncobj_timeline_array {
    uint64_t handles;
    uint64_t points;
    uint32_t count_handles;
    uint32_t pad;
};
struct drm_syncobj_wait {
    uint64_t handles;
    int64_t timeout_nsec;
    uint32_t count_handles;
    uint32_t flags;
    uint32_t first_signaled; /* only valid on return */
    uint32_t pad;
    uint64_t deadline_nsec;
};
struct drm_syncobj_timeline_wait {
    uint64_t handles;
    uint64_t points;
    int64_t timeout_nsec;
    uint32_t count_handles;
    uint32_t flags;
    uint32_t first_signaled; /* only valid on return */
    uint32_t pad;
    uint64_t deadline_nsec;
};
struct drm_syncobj_eventfd {
    uint32_t handle;
    uint32_t flags;
    uint64_t point;
    int32_t fd;
    uint32_t pad;
};

_Static_assert(sizeof(struct drm_syncobj_wait) == 40,
               "drm_syncobj_wait must be the 40-byte Linux layout");
_Static_assert(sizeof(struct drm_syncobj_timeline_wait) == 48,
               "drm_syncobj_timeline_wait must be the 48-byte Linux layout");
_Static_assert(sizeof(struct drm_syncobj_eventfd) == 24,
               "drm_syncobj_eventfd must be the 24-byte Linux layout");

#define DRM_IOCTL_SYNCOBJ_CREATE \
    _IOWR('d', 0xBF, struct drm_syncobj_create)
#define DRM_IOCTL_SYNCOBJ_DESTROY \
    _IOWR('d', 0xC0, struct drm_syncobj_destroy)
#define DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD \
    _IOWR('d', 0xC1, struct drm_syncobj_handle)
#define DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE \
    _IOWR('d', 0xC2, struct drm_syncobj_handle)
#define DRM_IOCTL_SYNCOBJ_WAIT _IOWR('d', 0xC3, struct drm_syncobj_wait)
#define DRM_IOCTL_SYNCOBJ_RESET _IOWR('d', 0xC4, struct drm_syncobj_array)
#define DRM_IOCTL_SYNCOBJ_SIGNAL _IOWR('d', 0xC5, struct drm_syncobj_array)
#define DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT \
    _IOWR('d', 0xCA, struct drm_syncobj_timeline_wait)
#define DRM_IOCTL_SYNCOBJ_QUERY \
    _IOWR('d', 0xCB, struct drm_syncobj_timeline_array)
#define DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL \
    _IOWR('d', 0xCD, struct drm_syncobj_timeline_array)
#define DRM_IOCTL_SYNCOBJ_EVENTFD \
    _IOWR('d', 0xCF, struct drm_syncobj_eventfd)

/* 钉死到 libdrm 从同一头文件算出的命令字：打包尺寸是命令字的一部分，
 * 内核侧布局回退（例如 32 字节的 drm_syncobj_wait）会让这些 ioctl 落到
 * ENOTTY 而不是静默通过。 */
_Static_assert(DRM_IOCTL_SYNCOBJ_WAIT == 0xc02864c3u,
               "DRM_IOCTL_SYNCOBJ_WAIT must match libdrm (40-byte struct)");
_Static_assert(DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT == 0xc03064cau,
               "DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT must match libdrm (48-byte struct)");
_Static_assert(DRM_IOCTL_SYNCOBJ_EVENTFD == 0xc01864cfu,
               "DRM_IOCTL_SYNCOBJ_EVENTFD must match libdrm (24-byte struct)");

#define DRM_SYNCOBJ_CREATE_SIGNALED (1u << 0)
#define DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL (1u << 0)
#define DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT (1u << 1)
#define DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE (1u << 2)
#define DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE (1u << 0)
#define DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE (1u << 1)
#define DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE (1u << 0)
#define DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_TIMELINE (1u << 1)

static uint64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

static uint64_t query_point(int fd, uint32_t handle) {
    struct drm_syncobj_timeline_array q = {
        .handles = (uint64_t)(uintptr_t)&handle,
        .points = 0,
        .count_handles = 1,
    };
    uint64_t point = 0;
    q.points = (uint64_t)(uintptr_t)&point;
    if (ioctl(fd, DRM_IOCTL_SYNCOBJ_QUERY, &q) != 0) {
        return UINT64_MAX;
    }
    return point;
}

int main(void) {
    int fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
    CHECK(fd >= 0, "open card0");

    /* --- timeline 水位往返 --- */
    struct drm_syncobj_create create = {0};
    CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_CREATE, &create), 0, "SYNCOBJ_CREATE");
    uint32_t signaled = create.handle;
    CHECK(query_point(fd, signaled) == 0, "fresh timeline waterline is 0");

    struct drm_syncobj_create create_b = {0};
    CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_CREATE, &create_b), 0,
              "SYNCOBJ_CREATE (never signaled)");
    uint32_t pending = create_b.handle;

    uint64_t sig_point = 5;
    struct drm_syncobj_timeline_array sig = {
        .handles = (uint64_t)(uintptr_t)&signaled,
        .points = (uint64_t)(uintptr_t)&sig_point,
        .count_handles = 1,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL, &sig), 0,
              "TIMELINE_SIGNAL point 5");
    CHECK(query_point(fd, signaled) == 5, "QUERY reads the signaled waterline");

    struct drm_syncobj_timeline_array sigbad = {
        .handles = (uint64_t)(uintptr_t)&signaled,
        .points = (uint64_t)(uintptr_t)&sig_point,
        .count_handles = 1,
        .pad = 1,
    };
    CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL, &sigbad), EINVAL,
              "TIMELINE_SIGNAL with a nonzero tail word is EINVAL");

    /* --- TIMELINE_WAIT：绝对截止 + ETIME --- */
    {
        uint32_t handles[1] = { signaled };
        uint64_t points[1] = { 5 };
        struct drm_syncobj_timeline_wait w = {
            .handles = (uint64_t)(uintptr_t)handles,
            .points = (uint64_t)(uintptr_t)points,
            .timeout_nsec = 1, /* absolute: long since passed */
            .count_handles = 1,
            .flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL,
        };
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &w), 0,
                  "reached point succeeds even with a lapsed deadline");

        handles[0] = pending;
        points[0] = 9;
        w.flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL | DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT;
        uint64_t start = now_ns();
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &w), ETIME,
                  "lapsed absolute deadline is an immediate ETIME poll");
        CHECK(now_ns() - start < 100000000ull,
              "lapsed deadline returns without parking");

        w.timeout_nsec = (int64_t)(now_ns() + 400000000ull);
        start = now_ns();
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &w), ETIME,
                  "future absolute deadline expires with ETIME");
        uint64_t elapsed = now_ns() - start;
        CHECK(elapsed >= 300000000ull && elapsed < 5000000000ull,
              "future deadline waits its real remainder");

        w.flags = 1u << 4;
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &w), EINVAL,
                  "unknown wait flag rejected");

        w.flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL;
        w.count_handles = 0;
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &w), 0,
                  "empty handle list is immediate success");

        uint32_t bogus[1] = { 0xDEAD };
        w.handles = (uint64_t)(uintptr_t)bogus;
        w.points = (uint64_t)(uintptr_t)points;
        w.count_handles = 1;
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &w), ENOENT,
                  "unknown handle is ENOENT");

        handles[0] = pending;
        points[0] = 9;
        w.handles = (uint64_t)(uintptr_t)handles;
        w.timeout_nsec = (int64_t)(now_ns() + 100000000ull);
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &w), EINVAL,
                  "unsubmitted point without WAIT_FOR_SUBMIT is EINVAL");
    }

    /* --- 二进制 WAIT --- */
    {
        uint32_t handles[1] = { pending };
        struct drm_syncobj_wait w = {
            .handles = (uint64_t)(uintptr_t)handles,
            .count_handles = 1,
            .timeout_nsec = 1,
            .flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE,
        };
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_WAIT, &w), EINVAL,
                  "WAIT_AVAILABLE is illegal on the binary wait");

        w.flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL;
        w.count_handles = 0;
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_WAIT, &w), 0,
                  "binary wait with no handles succeeds");

        uint32_t bogus[1] = { 0xDEAD };
        w.handles = (uint64_t)(uintptr_t)bogus;
        w.count_handles = 1;
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_WAIT, &w), ENOENT,
                  "binary wait unknown handle is ENOENT");

        w.handles = (uint64_t)(uintptr_t)handles;
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_WAIT, &w), EINVAL,
                  "unsignaled binary without WAIT_FOR_SUBMIT is EINVAL");

        handles[0] = signaled;
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_WAIT, &w), 0,
                  "signaled binary syncobj waits successfully");
    }

    /* --- first_signaled 写回（非 WAIT_ALL 成功路径）--- */
    {
        uint32_t handles[2] = { pending, signaled };
        uint64_t points[2] = { 9, 4 };
        struct drm_syncobj_timeline_wait w = {
            .handles = (uint64_t)(uintptr_t)handles,
            .points = (uint64_t)(uintptr_t)points,
            .timeout_nsec = 1, /* absolute: long since passed */
            .count_handles = 2,
            /* WAIT_FOR_SUBMIT lets the unsatisfied handle stay in the list
             * (Linux resolves every handle up front otherwise). */
            .flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT,
        };
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &w), 0,
                  "any-handle timeline wait succeeds when the second is reached");
        CHECK(w.first_signaled == 1,
              "timeline wait writes back the first satisfied index");

        struct drm_syncobj_wait bw = {
            .handles = (uint64_t)(uintptr_t)handles,
            .timeout_nsec = 1,
            .count_handles = 2,
            .flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT,
        };
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_WAIT, &bw), 0,
                  "any-handle binary wait succeeds when the second is signaled");
        CHECK(bw.first_signaled == 1,
              "binary wait writes back the first satisfied index");
    }

    /* --- timeline wait 的 point 0 是二进制视图 --- */
    {
        uint32_t handles[1] = { pending };
        uint64_t points[1] = { 0 };
        struct drm_syncobj_timeline_wait w = {
            .handles = (uint64_t)(uintptr_t)handles,
            .points = (uint64_t)(uintptr_t)points,
            .timeout_nsec = 1,
            .count_handles = 1,
        };
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &w), EINVAL,
                  "point 0 on an unsignaled syncobj is the binary view, not always-reached");
        handles[0] = signaled;
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &w), 0,
                  "point 0 on a signaled syncobj reads as the binary view");
    }

    /* --- HANDLE_TO_FD / FD_TO_HANDLE 校验与往返 --- */
    {
        struct drm_syncobj_handle h = { .handle = signaled, .flags = 1u << 2 };
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &h), EINVAL,
                  "unknown export flag rejected");

        h.flags = DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE;
        h.pad = 1;
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &h), EINVAL,
                  "nonzero pad rejected");
        h.pad = 0;

        h.point = 3; /* without TIMELINE a nonzero point is invalid */
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &h), EINVAL,
                  "nonzero point without TIMELINE rejected");
        h.point = 0;

        h.handle = 0xDEAD;
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &h), ENOENT,
                  "export of unknown handle is ENOENT");
        h.handle = signaled;

        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &h), 0,
                  "export reached point as sync file");
        CHECK(h.fd >= 0, "export produced an fd");
        struct pollfd pfd = { .fd = h.fd, .events = POLLIN };
        CHECK(poll(&pfd, 1, 0) == 1, "signaled export is immediately readable");

        struct drm_syncobj_handle import = {
            .fd = h.fd,
            .flags = DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE,
        };
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE, &import), 0,
                  "import round trip");
        CHECK(import.handle != 0 && import.handle != signaled,
              "import assigns a fresh handle");
        CHECK(query_point(fd, import.handle) == 1,
              "binary import reads as signaled point 1");
        CHECK_RET(close(h.fd), 0, "close export fd");

        /* TIMELINE 点位导出/导入（v6.16 语义） */
        struct drm_syncobj_handle texp = {
            .handle = signaled,
            .flags = DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE |
                     DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE,
            .point = 5,
        };
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &texp), 0,
                  "timeline export at a reached point");
        CHECK(texp.fd >= 0, "timeline export produced an fd");
        struct drm_syncobj_handle timp = {
            .fd = texp.fd,
            .flags = DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE |
                     DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_TIMELINE,
            .point = 5,
        };
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE, &timp), 0,
                  "timeline import round trip");
        CHECK(query_point(fd, timp.handle) == 5,
              "timeline import lands at the exported point");
        CHECK_RET(close(texp.fd), 0, "close timeline export fd");

        struct drm_syncobj_handle badfd = {
            .fd = open("/dev/null", O_RDONLY),
            .flags = DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE,
        };
        CHECK(badfd.fd >= 0, "open /dev/null for the bad-fd case");
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE, &badfd), EINVAL,
                  "import of a non-sync-file fd is EINVAL");
        CHECK_RET(close(badfd.fd), 0, "close /dev/null");

        struct drm_syncobj_handle badflag = {
            .fd = 0,
            .flags = 1u << 2,
        };
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE, &badflag), EINVAL,
                  "unknown import flag rejected");
    }

    /* --- RESET / SIGNAL --- */
    {
        struct drm_syncobj_array bad = {
            .handles = (uint64_t)(uintptr_t)&signaled,
            .count_handles = 1,
            .pad = 1,
        };
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_SIGNAL, &bad), EINVAL,
                  "SIGNAL with a nonzero tail word is EINVAL");
        bad.pad = 0;
        bad.count_handles = 0;
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_SIGNAL, &bad), EINVAL,
                  "SIGNAL with an empty handle list is EINVAL");
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_RESET, &bad), EINVAL,
                  "RESET with an empty handle list is EINVAL");

        struct drm_syncobj_array arr = {
            .handles = (uint64_t)(uintptr_t)&signaled,
            .count_handles = 1,
        };
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_RESET, &arr), 0, "RESET");
        CHECK(query_point(fd, signaled) == 0, "reset clears the waterline");

        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_SIGNAL, &arr), 0, "SIGNAL");
        CHECK(query_point(fd, signaled) == 1, "binary signal reaches point 1");
    }

    /* --- EVENTFD 唤醒传播 --- */
    {
        int efd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
        CHECK(efd >= 0, "create eventfd");
        struct drm_syncobj_eventfd ev = {
            .handle = pending,
            .flags = 0,
            .point = 100,
            .fd = efd,
        };
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_EVENTFD, &ev), EINVAL,
                  "eventfd on an unbacked point without WAIT_AVAILABLE is EINVAL");
        ev.flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE;
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_EVENTFD, &ev), 0,
                  "register eventfd waiter");
        uint64_t point = 100;
        struct drm_syncobj_timeline_array sigp = {
            .handles = (uint64_t)(uintptr_t)&pending,
            .points = (uint64_t)(uintptr_t)&point,
            .count_handles = 1,
        };
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL, &sigp), 0,
                  "signal the waited point");
        struct pollfd pfd = { .fd = efd, .events = POLLIN };
        CHECK(poll(&pfd, 1, 2000) == 1, "eventfd woken by the signal");
        uint64_t val = 0;
        CHECK_RET(read(efd, &val, sizeof(val)), 8, "read eventfd payload");
        CHECK(val == 1, "eventfd payload is 1");
        CHECK_RET(close(efd), 0, "close eventfd");
    }

    /* --- SYNCOBJ_SIGNAL 唤醒 EVENTFD（二进制信号路径）--- */
    {
        struct drm_syncobj_create c2 = {0};
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_CREATE, &c2), 0,
                  "SYNCOBJ_CREATE (binary signal case)");
        uint32_t b = c2.handle;
        int efd2 = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
        CHECK(efd2 >= 0, "create eventfd for the binary case");
        struct drm_syncobj_eventfd ev = {
            .handle = b,
            .flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE,
            .point = 0, /* binary view */
            .fd = efd2,
        };
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_EVENTFD, &ev), 0,
                  "register eventfd on the binary view");
        struct drm_syncobj_array arr = {
            .handles = (uint64_t)(uintptr_t)&b,
            .count_handles = 1,
        };
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_SIGNAL, &arr), 0,
                  "SIGNAL the binary syncobj");
        struct pollfd pfd2 = { .fd = efd2, .events = POLLIN };
        CHECK(poll(&pfd2, 1, 2000) == 1, "SYNCOBJ_SIGNAL wakes the eventfd waiter");
        uint64_t val = 0;
        CHECK_RET(read(efd2, &val, sizeof(val)), 8,
                  "read the binary-signal payload");
        CHECK(val == 1, "binary-signal eventfd payload is 1");
        CHECK_RET(close(efd2), 0, "close the binary eventfd");
    }

    /* --- DESTROY --- */
    {
        struct drm_syncobj_destroy bad = { .handle = pending, .pad = 1 };
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_DESTROY, &bad), EINVAL,
                  "DESTROY with a nonzero pad is EINVAL");

        struct drm_syncobj_destroy d = { .handle = pending };
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_DESTROY, &d), 0, "DESTROY pending");
        CHECK(query_point(fd, pending) == UINT64_MAX,
              "query after destroy fails");
    }

    TEST_DONE();
}
