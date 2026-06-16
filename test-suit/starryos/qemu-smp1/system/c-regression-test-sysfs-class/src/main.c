#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <limits.h>

#ifndef PATH_MAX
#define PATH_MAX 4096
#endif

/*
 * test-sysfs-class: 验证 /sys/class/<subsystem>/<name> 的形状对得上
 * libudev 的 realpath()+dirname() 走法。重点：class 下面的条目必须是
 * 软链，且解析到 /sys/devices/virtual/<subsystem>/<name>。
 */

static int read_file(const char *path, char *buf, size_t cap)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    ssize_t n = read(fd, buf, cap - 1);
    close(fd);
    if (n < 0) return -1;
    buf[n] = '\0';
    return 0;
}

int main(void)
{
    TEST_START("sysfs-class");

    /* /sys/class/drm 必须是目录（subsystem 容器） */
    struct stat st;
    CHECK_RET(stat("/sys/class/drm", &st), 0, "/sys/class/drm 应可 stat");
    CHECK(S_ISDIR(st.st_mode), "/sys/class/drm 应是目录");

    /* card0 必须存在并且是软链 (libudev 的 realpath 要求) */
    CHECK_RET(lstat("/sys/class/drm/card0", &st), 0,
              "/sys/class/drm/card0 应可 lstat");
    CHECK(S_ISLNK(st.st_mode),
          "/sys/class/drm/card0 应是软链 (libudev realpath 依赖)");

    /* readlink 目标应指向 ../../devices/virtual/drm/card0 */
    char link_target[PATH_MAX];
    ssize_t n = readlink("/sys/class/drm/card0", link_target,
                         sizeof(link_target) - 1);
    CHECK(n > 0, "readlink /sys/class/drm/card0");
    if (n > 0) {
        link_target[n] = '\0';
        CHECK(strstr(link_target, "devices/virtual/drm/card0") != NULL,
              "card0 软链应指向 devices/virtual/drm/card0");
    }

    /* 解析后的真实目录里的 dev / uevent 必须可读 */
    CHECK_RET(stat("/sys/class/drm/card0/dev", &st), 0,
              "card0/dev 应可读 (libudev udev_device_new_from_devnum)");
    char buf[256];
    CHECK_RET(read_file("/sys/class/drm/card0/dev", buf, sizeof(buf)), 0,
              "card0/dev 内容可读");
    CHECK(strstr(buf, "226:0") != NULL,
          "card0/dev 内容应是 226:0 (DRM_MAJOR=226)");

    CHECK_RET(read_file("/sys/class/drm/card0/uevent", buf, sizeof(buf)), 0,
              "card0/uevent 内容可读");
    CHECK(strstr(buf, "MAJOR=226") != NULL,
          "card0/uevent 应含 MAJOR=226");
    CHECK(strstr(buf, "DEVNAME=") != NULL,
          "card0/uevent 应含 DEVNAME=");

    /* /sys/dev/char/<maj>:<min> 必须存在且解析到设备目录 -- libinput
     * 的 evdev_device_have_same_syspath 把 fstat().st_rdev 反查靠它 */
    CHECK_RET(lstat("/sys/dev/char/226:0", &st), 0,
              "/sys/dev/char/226:0 应存在");
    CHECK(S_ISLNK(st.st_mode),
          "/sys/dev/char/226:0 应是软链");

    /* /sys/class/graphics/fb0 同样应是软链 */
    CHECK_RET(lstat("/sys/class/graphics/fb0", &st), 0,
              "/sys/class/graphics/fb0 应可 lstat");
    CHECK(S_ISLNK(st.st_mode),
          "/sys/class/graphics/fb0 应是软链");

    /* /sys/fs/cgroup 必须由 sysfs 自身提供 —— sysfs 盖在 /sys 上后
     * rootfs 里的同名目录不可见，systemd 在这里挂 cgroup 层级 */
    CHECK_RET(stat("/sys/fs", &st), 0, "/sys/fs 应可 stat");
    CHECK(S_ISDIR(st.st_mode), "/sys/fs 应是目录");
    CHECK_RET(stat("/sys/fs/cgroup", &st), 0, "/sys/fs/cgroup 应可 stat");
    CHECK(S_ISDIR(st.st_mode), "/sys/fs/cgroup 应是目录");

    CHECK_RET(mount("none", "/sys/fs/cgroup", "tmpfs", 0, NULL), 0,
              "tmpfs 应能挂载到 /sys/fs/cgroup");
    CHECK_RET(umount2("/sys/fs/cgroup", 0), 0,
              "应能卸载 /sys/fs/cgroup 上的 tmpfs");

    TEST_DONE();
}
