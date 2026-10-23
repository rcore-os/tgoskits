/*
 * test-drm-version — /dev/dri/card0 smoke test
 *
 * 直接发 libdrm 的 drmOpen + drmGetVersion + drmGetCap 路径用到的 ioctl，
 * 不依赖 libdrm。验证内核把 card0 暴露为 virtio-gpu 节点：
 *   - DRM_IOCTL_VERSION 两遍调用（第一遍 probe size，第二遍读字符串）
 *   - DRM_IOCTL_GET_CAP（DUMB_BUFFER 必须为 1，未知 cap 返回 EINVAL）
 *   - DRM_IOCTL_SET_CLIENT_CAP（UNIVERSAL_PLANES、ATOMIC 必须接受）
 *   - DRM_IOCTL_SET_MASTER / DROP_MASTER
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <unistd.h>

/* From linux/uapi/drm/drm.h — minimal subset. */
struct drm_version {
    int version_major;
    int version_minor;
    int version_patchlevel;
    size_t name_len;
    char *name;
    size_t date_len;
    char *date;
    size_t desc_len;
    char *desc;
};
struct drm_virtgpu_getparam {
    uint64_t param;
    uint64_t value;
};
struct drm_get_cap {
    uint64_t capability;
    uint64_t value;
};
struct drm_set_client_cap {
    uint64_t capability;
    uint64_t value;
};
struct drm_client {
    int32_t idx;
    int32_t auth;
    uint64_t pid;
    uint64_t uid;
    uint64_t magic;
    uint64_t iocs;
};
struct drm_set_client_name { uint64_t name_len, name; };
struct drm_mode_get_plane_res { uint64_t plane_id_ptr; uint32_t count_planes; };

#define DRM_IOCTL_VERSION         _IOWR('d', 0x00, struct drm_version)
#define DRM_IOCTL_SET_MASTER      _IO('d', 0x1e)
#define DRM_IOCTL_DROP_MASTER     _IO('d', 0x1f)
#define DRM_IOCTL_VIRTGPU_GETPARAM _IOWR('d', 0x43, struct drm_virtgpu_getparam)
#define DRM_IOCTL_GET_CAP         _IOWR('d', 0x0c, struct drm_get_cap)
#define DRM_IOCTL_SET_CLIENT_CAP  _IOW('d', 0x0d, struct drm_set_client_cap)
#define DRM_IOCTL_GET_CLIENT      _IOWR('d', 0x05, struct drm_client)
#define DRM_IOCTL_SET_CLIENT_NAME _IOWR('d', 0xd1, struct drm_set_client_name)
#define DRM_IOCTL_MODE_GETPLANERESOURCES _IOWR('d', 0xb5, struct drm_mode_get_plane_res)
_Static_assert(sizeof(struct drm_client) == 40, "drm_client UAPI size");
_Static_assert(DRM_IOCTL_GET_CLIENT == 0xc0286405U, "GET_CLIENT ioctl encoding");
_Static_assert(DRM_IOCTL_SET_CLIENT_NAME == 0xc01064d1U, "SET_CLIENT_NAME ioctl encoding");

#define DRM_CAP_DUMB_BUFFER             0x1
#define DRM_CLIENT_CAP_UNIVERSAL_PLANES 2
#define DRM_CLIENT_CAP_ATOMIC           3

struct concurrent_client {
    int fd, errors;
    pthread_barrier_t *start;
};

static void *toggle_universal_planes(void *opaque)
{
    struct concurrent_client *client = opaque;
    pthread_barrier_wait(client->start);
    for (unsigned i = 0; i < 64; ++i) {
        struct drm_set_client_cap cap = { .capability = DRM_CLIENT_CAP_UNIVERSAL_PLANES,
                                          .value = i & 1 };
        if (syscall(SYS_ioctl, client->fd, DRM_IOCTL_SET_CLIENT_CAP, &cap) != 0)
            client->errors++;
    }
    return NULL;
}

static void *query_planes(void *opaque)
{
    struct concurrent_client *client = opaque;
    pthread_barrier_wait(client->start);
    for (unsigned i = 0; i < 64; ++i) {
        struct drm_mode_get_plane_res planes = {0};
        if (syscall(SYS_ioctl, client->fd, DRM_IOCTL_MODE_GETPLANERESOURCES, &planes) != 0
            || planes.count_planes > 1)
            client->errors++;
    }
    return NULL;
}

int main(void)
{
    TEST_START("drm-version");

    int fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
    CHECK(fd >= 0, "open /dev/dri/card0");
    if (fd < 0) {
        TEST_DONE();
    }

    /* DRM_IOCTL_VERSION pass 1 — kernel reports required string lengths. */
    struct drm_version v = {0};
    CHECK_RET(ioctl(fd, DRM_IOCTL_VERSION, &v), 0, "VERSION probe");
    CHECK(v.name_len > 0, "name_len reported nonzero");

    /* DRM_IOCTL_VERSION pass 2 — fetch the strings. */
    char name[64] = {0}, date[64] = {0}, desc[128] = {0};
    v.name = name; v.name_len = sizeof(name) - 1;
    v.date = date; v.date_len = sizeof(date) - 1;
    v.desc = desc; v.desc_len = sizeof(desc) - 1;
    CHECK_RET(ioctl(fd, DRM_IOCTL_VERSION, &v), 0, "VERSION fetch");
    printf("  driver name=%s date=%s desc=%s version=%d.%d.%d\n",
           name, date, desc, v.version_major, v.version_minor, v.version_patchlevel);
    CHECK(strcmp(name, "virtio_gpu") == 0, "driver name == virtio_gpu");
    CHECK(v.version_major == 0 && v.version_minor == 1, "driver version 0.1");

    /* DRM_IOCTL_GET_CLIENT reports the calling task's virtual PID and the
     * Linux overflowuid used by this kernel when UIDs are not mapped. */
    struct drm_client client = { .idx = 0 };
    CHECK_RET(ioctl(fd, DRM_IOCTL_GET_CLIENT, &client), 0, "GET_CLIENT idx 0");
    CHECK(client.pid == (uint64_t)getpid(), "GET_CLIENT PID matches getpid");
    CHECK(client.uid == 65534, "GET_CLIENT UID is overflowuid");

    /* A privileged second open is authenticated even while the first owns master. */
    int second = syscall(SYS_openat, AT_FDCWD, "/dev/dri/card0", O_RDWR | O_CLOEXEC, 0);
    CHECK(second >= 0, "second privileged primary open");
    /* Linux's hollowed-out GET_CLIENT idx 0 reports the calling file, not
     * another client's list index. */
    struct drm_client second_client = {0};
    CHECK_RET(syscall(SYS_ioctl, second, DRM_IOCTL_GET_CLIENT, &second_client), 0,
              "GET_CLIENT on second privileged open");
    CHECK(second_client.auth == 1, "CAP_SYS_ADMIN open is authenticated without becoming master");
    CHECK_ERR(syscall(SYS_ioctl, fd, _IO('x', 0x1f), 0), ENOTTY,
              "wrong ioctl namespace cannot DROP_MASTER");
    CHECK_ERR(syscall(SYS_ioctl, second, DRM_IOCTL_SET_MASTER, 0), EBUSY,
              "wrong namespace left the first file's master intact");
    close(second);

    const char client_name[] = "version-test";
    struct drm_set_client_name named = { .name_len = sizeof(client_name) - 1,
                                         .name = (uint64_t)(uintptr_t)client_name };
    CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_SET_CLIENT_NAME, &named), 0,
              "SET_CLIENT_NAME accepts a printable name");
    named.name_len = 1; named.name = 0;
    CHECK_ERR(syscall(SYS_ioctl, fd, DRM_IOCTL_SET_CLIENT_NAME, &named), EFAULT,
              "SET_CLIENT_NAME rejects a bad user pointer");
    named.name_len = 65;
    CHECK_ERR(syscall(SYS_ioctl, fd, DRM_IOCTL_SET_CLIENT_NAME, &named), EINVAL,
              "SET_CLIENT_NAME validates length before copying");
    named.name_len = 0;
    CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_SET_CLIENT_NAME, &named), 0,
              "SET_CLIENT_NAME clears the name");

    /* DRM_IOCTL_GET_CAP — DUMB_BUFFER must report 1. */
    struct drm_get_cap cap = { .capability = DRM_CAP_DUMB_BUFFER };
    CHECK_RET(ioctl(fd, DRM_IOCTL_GET_CAP, &cap), 0, "GET_CAP DUMB_BUFFER");
    CHECK(cap.value == 1, "DUMB_BUFFER capability value == 1");
    const uint64_t known_caps[] = {3, 4, 7, 0x11, 0x15};
    for (unsigned i = 0; i < sizeof(known_caps) / sizeof(known_caps[0]); ++i) {
        cap.capability = known_caps[i];
        CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_GET_CAP, &cap), 0,
                  "known KMS capability query succeeds, including unsupported zero values");
    }
    cap.capability = (1ULL << 32) | DRM_CAP_DUMB_BUFFER;
    CHECK_ERR(syscall(SYS_ioctl, fd, DRM_IOCTL_GET_CAP, &cap), EINVAL,
              "GET_CAP does not truncate an unknown u64 capability");

    /* Linux drm_getcap rejects unknown capabilities with EINVAL. */
    cap.capability = 9999;
    cap.value = 0xdeadbeef;
    errno = 0;
    CHECK_RET(ioctl(fd, DRM_IOCTL_GET_CAP, &cap), -1, "GET_CAP unknown fails");
    CHECK(errno == EINVAL, "GET_CAP unknown returns EINVAL");
    CHECK(cap.value == 0, "unknown cap value cleared to 0");

    /* VirtGPU's numbered driver ioctl must reach GETPARAM, not its neighbor.
     * Its pointer carries a Linux int output, not a 64-bit scalar. */
    struct { uint32_t value; uint32_t guard; } parameter = {
        .value = UINT32_MAX, .guard = 0x5a00fe12,
    };
    struct drm_virtgpu_getparam getparam = {
        .param = 1, /* VIRTGPU_PARAM_3D_FEATURES */
        .value = (uint64_t)(uintptr_t)&parameter.value,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_VIRTGPU_GETPARAM, &getparam), 0,
              "VirtGPU GETPARAM dispatches to the capability query");
    CHECK(parameter.value <= 1, "VirtGPU 3D capability is a boolean");
    CHECK(parameter.guard == 0x5a00fe12,
          "VirtGPU GETPARAM writes exactly a 32-bit int");

    /* DRM_IOCTL_SET_CLIENT_CAP — UNIVERSAL_PLANES and ATOMIC must accept. */
    struct drm_set_client_cap scc = {
        .capability = DRM_CLIENT_CAP_UNIVERSAL_PLANES, .value = 1,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_SET_CLIENT_CAP, &scc), 0,
              "SET_CLIENT_CAP UNIVERSAL_PLANES");
    scc.capability = DRM_CLIENT_CAP_ATOMIC;
    CHECK_RET(ioctl(fd, DRM_IOCTL_SET_CLIENT_CAP, &scc), 0,
              "SET_CLIENT_CAP ATOMIC");

    /* Both workers use aliases of the same open-file description. The writer
     * toggles the same universal_planes field that GETPLANERESOURCES reads;
     * the core's atomic type supplies the race proof. */
    pthread_barrier_t start;
    pthread_t writer, reader;
    struct concurrent_client writing = { .fd = fd, .start = &start };
    struct concurrent_client reading = { .fd = dup(fd), .start = &start };
    CHECK_RET(pthread_barrier_init(&start, NULL, 2), 0, "create shared-fd barrier");
    int writer_ret = pthread_create(&writer, NULL, toggle_universal_planes, &writing);
    CHECK_RET(writer_ret, 0, "start shared-fd capability writer");
    int reader_ret = pthread_create(&reader, NULL, query_planes, &reading);
    CHECK_RET(reader_ret, 0, "start shared-fd plane reader");
    if (writer_ret != 0 || reader_ret != 0) exit(1);
    CHECK_RET(pthread_join(writer, NULL), 0, "join capability writer");
    CHECK_RET(pthread_join(reader, NULL), 0, "join plane reader");
    CHECK(writing.errors == 0 && reading.errors == 0, "shared-fd updates and queries both complete");
    pthread_barrier_destroy(&start);
    close(reading.fd);

    /* With this test's sole card fd holding master permission, SET_MASTER is
     * idempotent and DROP_MASTER relinquishes that master. */
    CHECK_RET(ioctl(fd, DRM_IOCTL_SET_MASTER), 0, "SET_MASTER");
    CHECK_RET(ioctl(fd, DRM_IOCTL_DROP_MASTER), 0, "DROP_MASTER");

    close(fd);
    TEST_DONE();
}
