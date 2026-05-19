/*
 * test-drm-version — /dev/dri/card0 smoke test
 *
 * 直接发 libdrm 的 drmOpen + drmGetVersion + drmGetCap 路径用到的 ioctl，
 * 不依赖 libdrm。验证内核把 card0 暴露为 simpledrm-class 节点：
 *   - DRM_IOCTL_VERSION 两遍调用（第一遍 probe size，第二遍读字符串）
 *   - DRM_IOCTL_GET_CAP（DUMB_BUFFER 必须为 1，未知 cap 返回 0 而不是错）
 *   - DRM_IOCTL_SET_CLIENT_CAP（UNIVERSAL_PLANES、ATOMIC 必须接受）
 *   - DRM_IOCTL_SET_MASTER / DROP_MASTER
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <stdint.h>
#include <sys/ioctl.h>
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
struct drm_get_cap {
    uint64_t capability;
    uint64_t value;
};
struct drm_set_client_cap {
    uint64_t capability;
    uint64_t value;
};

#define DRM_IOCTL_VERSION         _IOWR('d', 0x00, struct drm_version)
#define DRM_IOCTL_SET_MASTER      _IO('d', 0x1e)
#define DRM_IOCTL_DROP_MASTER     _IO('d', 0x1f)
#define DRM_IOCTL_GET_CAP         _IOWR('d', 0x0c, struct drm_get_cap)
#define DRM_IOCTL_SET_CLIENT_CAP  _IOW('d', 0x0d, struct drm_set_client_cap)

#define DRM_CAP_DUMB_BUFFER             0x1
#define DRM_CLIENT_CAP_UNIVERSAL_PLANES 2
#define DRM_CLIENT_CAP_ATOMIC           3

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
    CHECK(strcmp(name, "starry-simpledrm") == 0, "driver name == starry-simpledrm");
    CHECK(v.version_major == 1 && v.version_minor == 0, "driver version 1.0");

    /* DRM_IOCTL_GET_CAP — DUMB_BUFFER must report 1. */
    struct drm_get_cap cap = { .capability = DRM_CAP_DUMB_BUFFER };
    CHECK_RET(ioctl(fd, DRM_IOCTL_GET_CAP, &cap), 0, "GET_CAP DUMB_BUFFER");
    CHECK(cap.value == 1, "DUMB_BUFFER capability value == 1");

    /* Unknown caps must NOT error — Linux returns 0; libdrm probes a
     * dozen of them at startup. */
    cap.capability = 9999;
    cap.value = 0xdeadbeef;
    CHECK_RET(ioctl(fd, DRM_IOCTL_GET_CAP, &cap), 0, "GET_CAP unknown returns 0");
    CHECK(cap.value == 0, "unknown cap value cleared to 0");

    /* DRM_IOCTL_SET_CLIENT_CAP — UNIVERSAL_PLANES and ATOMIC must accept. */
    struct drm_set_client_cap scc = {
        .capability = DRM_CLIENT_CAP_UNIVERSAL_PLANES, .value = 1,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_SET_CLIENT_CAP, &scc), 0,
              "SET_CLIENT_CAP UNIVERSAL_PLANES");
    scc.capability = DRM_CLIENT_CAP_ATOMIC;
    CHECK_RET(ioctl(fd, DRM_IOCTL_SET_CLIENT_CAP, &scc), 0,
              "SET_CLIENT_CAP ATOMIC");

    /* SET_MASTER / DROP_MASTER — always succeed in the skeleton. */
    CHECK_RET(ioctl(fd, DRM_IOCTL_SET_MASTER), 0, "SET_MASTER");
    CHECK_RET(ioctl(fd, DRM_IOCTL_DROP_MASTER), 0, "DROP_MASTER");

    close(fd);
    TEST_DONE();
}
