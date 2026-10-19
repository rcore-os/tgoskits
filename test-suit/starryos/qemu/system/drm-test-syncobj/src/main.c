/*
 * test-drm-syncobj — card0 syncobj 家族与 Linux UAPI 语义一致性测试
 *
 * 覆盖（对齐 Linux drm_syncobj.c 的可判定语义，全部无需 3D/venus 宿主）：
 *   - SYNCOBJ_CREATE / TIMELINE_SIGNAL / TIMELINE_QUERY 水位往返；
 *     QUERY 空列表 EINVAL、LAST_SUBMITTED 被接受、未知 flag EINVAL
 *   - TIMELINE_WAIT：绝对 CLOCK_MONOTONIC 截止（过期立即 ETIME、未来截止
 *     按真实剩余时间等待）、已信号点先查状态再判超时、ETIME 而非 ETIMEDOUT、
 *     未知 flag EINVAL、空句柄列表成功、未知句柄 ENOENT、
 *     无 WAIT_FOR_SUBMIT 且点未提交 → EINVAL
 *   - 二进制 WAIT：WAIT_AVAILABLE 非法（EINVAL）、空列表成功、未提交 → EINVAL、
 *     非 WAIT_ALL 成功时写回 first_signaled
 *   - HANDLE_TO_FD / FD_TO_HANDLE：flag/pad/point 校验、未知句柄 ENOENT、
 *     同步文件导出/导入往返（import 的 handle 是输入：装入既有 syncobj、
 *     不写回新句柄，含 TIMELINE 点位）、坏 fd EINVAL、
 *     无 IMPORT flag 的 sync-file fd 是 EINVAL（Linux 只认 syncobj fd）、
 *     未提交点位导出 EINVAL（不得假信号）、
 *     同点位双导出后关闭其一，另一 fd 仍可读
 *   - RESET / SIGNAL 水位清零与置一
 *   - 各数组承载 ioctl 的 count_handles 超上限（1024 + 1）→ EINVAL
 *     （在任何内核分配与用户内存访问之前拒绝）
 *   - EVENTFD：未背书点注册成功并在 TIMELINE_SIGNAL 后唤醒（Linux 语义：
 *     条目等 fence 挂上）、已到达点注册即触发、WAIT_AVAILABLE 注册、
 *     未知句柄 ENOENT、未知 flag / pad EINVAL
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
    uint32_t flags;
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
#define DRM_SYNCOBJ_QUERY_FLAGS_LAST_SUBMITTED (1u << 0)
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

    /* --- QUERY 校验：空列表 EINVAL、LAST_SUBMITTED、未知 flag --- */
    {
        struct drm_syncobj_timeline_array q = {
            .handles = (uint64_t)(uintptr_t)&signaled,
            .points = 0,
            .count_handles = 0,
        };
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_QUERY, &q), EINVAL,
                  "QUERY with an empty handle list is EINVAL");

        uint64_t point = 0;
        q.points = (uint64_t)(uintptr_t)&point;
        q.count_handles = 1;
        q.flags = 1u << 1;
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_QUERY, &q), EINVAL,
                  "QUERY with an unknown flag is EINVAL");

        q.flags = DRM_SYNCOBJ_QUERY_FLAGS_LAST_SUBMITTED;
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_QUERY, &q), 0,
                  "QUERY accepts LAST_SUBMITTED");
        CHECK(point == 5, "LAST_SUBMITTED reports the newest attached point");
    }

    struct drm_syncobj_timeline_array sigbad = {
        .handles = (uint64_t)(uintptr_t)&signaled,
        .points = (uint64_t)(uintptr_t)&sig_point,
        .count_handles = 1,
        .flags = 1,
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

        /* flags=0 在 Linux 是导出 syncobj fd 本身；本驱动不发放 syncobj fd，
         * 与 FD_TO_HANDLE 无 flag 方向同样 EINVAL。 */
        h.flags = 0;
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &h), EINVAL,
                  "export without EXPORT_SYNC_FILE rejected");
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

        /* import 的 handle 是输入（Linux drm_syncobj_import_sync_file_fence）：
         * fence 装入既有 syncobj、不写回新句柄。 */
        struct drm_syncobj_create imp_c = {0};
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_CREATE, &imp_c), 0,
                  "SYNCOBJ_CREATE (import target)");
        uint32_t imp_tgt = imp_c.handle;
        struct drm_syncobj_handle import = {
            .handle = imp_tgt,
            .fd = h.fd,
            .flags = DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE,
        };
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE, &import), 0,
                  "import round trip");
        CHECK(import.handle == imp_tgt,
              "import writes back no new handle");
        CHECK(query_point(fd, imp_tgt) == 1,
              "binary import signals the target at point 1");
        CHECK_RET(close(h.fd), 0, "close export fd");

        /* TIMELINE 点位导出/导入（v6.16 语义）：目标点由导入方指定，
         * 与导出点位无关。 */
        struct drm_syncobj_handle texp = {
            .handle = signaled,
            .flags = DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE |
                     DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE,
            .point = 5,
        };
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &texp), 0,
                  "timeline export at a reached point");
        CHECK(texp.fd >= 0, "timeline export produced an fd");
        struct drm_syncobj_create timp_c = {0};
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_CREATE, &timp_c), 0,
                  "SYNCOBJ_CREATE (timeline import target)");
        uint32_t timp_tgt = timp_c.handle;
        struct drm_syncobj_handle timp = {
            .handle = timp_tgt,
            .fd = texp.fd,
            .flags = DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE |
                     DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_TIMELINE,
            .point = 7,
        };
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE, &timp), 0,
                  "timeline import round trip");
        CHECK(query_point(fd, timp_tgt) == 7,
              "timeline import lands at the requested point");

        struct drm_syncobj_handle unknown = {
            .handle = 0xDEAD,
            .fd = texp.fd,
            .flags = DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE,
        };
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE, &unknown), ENOENT,
                  "import into an unknown handle is ENOENT");
        CHECK_RET(close(texp.fd), 0, "close timeline export fd");

        /* 无 IMPORT flag：Linux 只接受 syncobj fd（fops 检查）；sync-file
         * fd 在该方向上就是 EINVAL。 */
        struct drm_syncobj_handle fresh = {0};
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE, &fresh), EINVAL,
                  "FD_TO_HANDLE without IMPORT_SYNC_FILE is EINVAL (no syncobj fds exist)");

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

        struct drm_syncobj_handle badpad = {
            .handle = imp_tgt,
            .fd = 0,
            .flags = DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE,
            .pad = 1,
        };
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE, &badpad), EINVAL,
                  "nonzero pad rejected on import");

        /* 同点位双导出：关闭其一后另一 fd 仍可读（注册按文件身份回收，
         * 不按 fence id 连坐）。 */
        struct drm_syncobj_handle twin_a = {
            .handle = signaled,
            .flags = DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE,
        };
        struct drm_syncobj_handle twin_b = {
            .handle = signaled,
            .flags = DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE,
        };
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &twin_a), 0,
                  "twin export A");
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &twin_b), 0,
                  "twin export B");
        CHECK_RET(close(twin_a.fd), 0, "close twin export A");
        struct pollfd tpfd = { .fd = twin_b.fd, .events = POLLIN };
        CHECK(poll(&tpfd, 1, 0) == 1, "twin export B is still readable after A closes");
        CHECK_RET(close(twin_b.fd), 0, "close twin export B");

        /* 未提交点位导出：Linux drm_syncobj_find_fence 无 WAIT_FOR_SUBMIT
         * 语义 → EINVAL。旧实现把文件停靠在等待列表里，fence id 0 在首次
         * poll 即报已信号——没有 GPU 工作却宣告完成，此用例拦该回归。 */
        struct drm_syncobj_handle unbacked = {
            .handle = pending, /* 水位 0，无任何在途提交背书 */
            .flags = DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE,
        };
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &unbacked), EINVAL,
                  "export of an unsubmitted point is EINVAL, never a fake-signaled fd");
        unbacked.flags = DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE |
                         DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE;
        unbacked.point = 9;
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &unbacked), EINVAL,
                  "timeline export of an unsubmitted point is EINVAL");
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

    /* --- count_handles 限幅：超上限必须在分配前 EINVAL ---
     * 上限 1024，与 venus EXECBUFFER 的 syncobj 数组上限一致；填 1025 个
     * 未知句柄让未限幅实现可判定地停在句柄查找（ENOENT），而限幅实现
     * 在触碰用户内存、分配内核缓冲之前就返回 EINVAL。 */
    {
        enum { CAP = 1024 };
        static uint32_t many[CAP + 1];
        static uint64_t many_points[CAP + 1];
        for (int i = 0; i < CAP + 1; i++) {
            many[i] = 0xDEAD;
            many_points[i] = 1;
        }
        struct drm_syncobj_array arr = {
            .handles = (uint64_t)(uintptr_t)many,
            .count_handles = CAP + 1,
        };
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_SIGNAL, &arr), EINVAL,
                  "SIGNAL with an oversized handle list is EINVAL");
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_RESET, &arr), EINVAL,
                  "RESET with an oversized handle list is EINVAL");

        struct drm_syncobj_timeline_array tarr = {
            .handles = (uint64_t)(uintptr_t)many,
            .points = (uint64_t)(uintptr_t)many_points,
            .count_handles = CAP + 1,
        };
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL, &tarr), EINVAL,
                  "TIMELINE_SIGNAL with an oversized handle list is EINVAL");
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_QUERY, &tarr), EINVAL,
                  "QUERY with an oversized handle list is EINVAL");

        struct drm_syncobj_timeline_wait tw = {
            .handles = (uint64_t)(uintptr_t)many,
            .points = (uint64_t)(uintptr_t)many_points,
            .timeout_nsec = 1, /* absolute: lapsed, nothing may park */
            .count_handles = CAP + 1,
            .flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL,
        };
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &tw), EINVAL,
                  "TIMELINE_WAIT with an oversized handle list is EINVAL");

        struct drm_syncobj_wait bw = {
            .handles = (uint64_t)(uintptr_t)many,
            .timeout_nsec = 1,
            .count_handles = CAP + 1,
            .flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL,
        };
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_WAIT, &bw), EINVAL,
                  "WAIT with an oversized handle list is EINVAL");
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
        /* Linux 语义：未背书点也注册成功，条目等 fence 挂上后触发。 */
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_EVENTFD, &ev), 0,
                  "unbacked point registers without WAIT_AVAILABLE");
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

        /* 已到达点：注册即触发（Linux 找到的 fence 已信号，
         * dma_fence_add_callback 返回 -ENOENT 路径）。 */
        int efd_now = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
        CHECK(efd_now >= 0, "create eventfd for the reached point");
        struct drm_syncobj_eventfd ev_now = {
            .handle = signaled,
            .flags = 0,
            .point = 0, /* binary view, watermark 已到 1 */
            .fd = efd_now,
        };
        CHECK_RET(ioctl(fd, DRM_IOCTL_SYNCOBJ_EVENTFD, &ev_now), 0,
                  "register eventfd on an already-reached point");
        struct pollfd npfd = { .fd = efd_now, .events = POLLIN };
        CHECK(poll(&npfd, 1, 0) == 1, "reached-point eventfd fires immediately");
        CHECK_RET(close(efd_now), 0, "close the reached-point eventfd");

        /* 负例：未知句柄 ENOENT、未知 flag / pad EINVAL、坏 fd EINVAL。 */
        struct drm_syncobj_eventfd ev_bad = {
            .handle = 0xDEAD,
            .fd = 1,
        };
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_EVENTFD, &ev_bad), ENOENT,
                  "eventfd on an unknown handle is ENOENT");
        ev_bad.handle = pending;
        ev_bad.flags = 1u << 3;
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_EVENTFD, &ev_bad), EINVAL,
                  "eventfd with an unknown flag is EINVAL");
        ev_bad.flags = 0;
        ev_bad.pad = 1;
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_EVENTFD, &ev_bad), EINVAL,
                  "eventfd with a nonzero pad is EINVAL");
        ev_bad.pad = 0;
        ev_bad.fd = open("/dev/null", O_RDONLY);
        CHECK(ev_bad.fd >= 0, "open /dev/null for the eventfd bad-fd case");
        CHECK_ERR(ioctl(fd, DRM_IOCTL_SYNCOBJ_EVENTFD, &ev_bad), EINVAL,
                  "eventfd on a non-eventfd fd is EINVAL");
        CHECK_RET(close(ev_bad.fd), 0, "close the eventfd bad fd");
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
