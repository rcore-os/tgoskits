#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>

/*
 * test-evdev-minor: 验证 /dev/input/event0 的 minor 号匹配 Linux 的
 * EVDEV_MINOR_BASE = 64。否则 libinput 的 same_syspath 检查会失败：
 * sysfs 一直按 13:64 写，设备节点却是 13:1，对不上。
 */

int main(void)
{
    TEST_START("evdev-minor");

    struct stat st;
    int rc = stat("/dev/input/event0", &st);
    CHECK_RET(rc, 0,
              "/dev/input/event0 应存在 (QEMU config includes virtio-keyboard)");
    if (rc != 0) {
        TEST_DONE();
    }

    CHECK((st.st_mode & S_IFMT) == S_IFCHR,
          "/dev/input/event0 应是字符设备");
    CHECK(major(st.st_rdev) == 13,
          "/dev/input/event0 主设备号应为 13");
    CHECK(minor(st.st_rdev) == 64,
          "/dev/input/event0 次设备号应为 64 (EVDEV_MINOR_BASE)");

    TEST_DONE();
}
