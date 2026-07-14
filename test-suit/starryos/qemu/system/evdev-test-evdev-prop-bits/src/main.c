/*
 * test-evdev-prop-bits
 *
 * 验证 /dev/input/event* 暴露的 EVIOCGPROP 与 EVIOCGABS 信息:
 *   - EVIOCGPROP 返回非空字节, 且 INPUT_PROP_POINTER (bit 0) 已置位
 *     (内核为带 EV_REL/EV_ABS 且非触摸屏的指针类设备合成)
 *   - 对带 EV_ABS 的设备, EVIOCGABS(ABS_X) 返回的 input_absinfo.maximum
 *     大于 minimum, 即 libinput 能用来归一化坐标
 *
 * qemu toml provides virtio-tablet, so missing /dev/input/event* is a
 * regression instead of a skip.
 */

#include "test_framework.h"

#include <dirent.h>
#include <fcntl.h>
#include <stdint.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <unistd.h>

/* Keep this test independent from linux-headers in the staging root. */
#define EV_REL 0x02
#define EV_ABS 0x03
#define EV_MAX 0x1f
#define ABS_X 0x00
#define ABS_MAX 0x3f

#ifndef INPUT_PROP_POINTER
#define INPUT_PROP_POINTER 0x00
#endif
#ifndef INPUT_PROP_DIRECT
#define INPUT_PROP_DIRECT  0x01
#endif
#ifndef EVIOCGBIT
#define EVIOCGBIT(ev, len) _IOC(_IOC_READ, 'E', 0x20 + (ev), len)
#endif
#ifndef EVIOCGPROP
#define EVIOCGPROP(len) _IOC(_IOC_READ, 'E', 0x09, len)
#endif
#ifndef EVIOCGABS
struct input_absinfo {
    int value;
    int minimum;
    int maximum;
    int fuzz;
    int flat;
    int resolution;
};
#define EVIOCGABS(axis) _IOC(_IOC_READ, 'E', 0x40 + (axis), sizeof(struct input_absinfo))
#endif
#ifndef EVIOCGABS_SMALL
#define EVIOCGABS_SMALL(axis) _IOC(_IOC_READ, 'E', 0x40 + (axis), sizeof(int))
#endif

static int has_bit(const unsigned char *bits, size_t nbits, size_t bit) {
    if (bit / 8 >= nbits) return 0;
    return (bits[bit / 8] >> (bit % 8)) & 1;
}

static int first_unsupported_abs_axis(const unsigned char *bits, size_t nbits) {
    for (int axis = 0; axis <= ABS_MAX; axis++) {
        if (!has_bit(bits, nbits, axis)) return axis;
    }
    return -1;
}

static int test_one_device(const char *path) {
    int fd = open(path, O_RDONLY | O_NONBLOCK);
    if (fd < 0) {
        printf("  SKIP | %s | open failed errno=%d (%s)\n",
               path, errno, strerror(errno));
        return 0;
    }

    /* 拉一份事件位图判断设备是否带 EV_ABS / EV_REL */
    unsigned char ev_bits[(EV_MAX + 7) / 8] = {0};
    int rc = ioctl(fd, EVIOCGBIT(0, sizeof(ev_bits)), ev_bits);
    CHECK(rc >= 0, "EVIOCGBIT(0)");
    int has_abs = has_bit(ev_bits, sizeof(ev_bits), EV_ABS);
    int has_rel = has_bit(ev_bits, sizeof(ev_bits), EV_REL);

    /* EVIOCGPROP — 内核应返回 4 字节属性图. 带轴非触摸屏需置 POINTER. */
    unsigned char prop_bits[8] = {0};
    rc = ioctl(fd, EVIOCGPROP(sizeof(prop_bits)), prop_bits);
    CHECK(rc >= 0, "EVIOCGPROP returns success");
    if (rc >= 0) {
        int is_touchscreen = has_bit(prop_bits, sizeof(prop_bits), INPUT_PROP_DIRECT);
        if ((has_abs || has_rel) && !is_touchscreen) {
            CHECK(has_bit(prop_bits, sizeof(prop_bits), INPUT_PROP_POINTER),
                  "INPUT_PROP_POINTER set on pointer device");
        } else {
            printf("  SKIP | %s | INPUT_PROP_POINTER check (touchscreen or no axes)\n",
                   path);
        }
    }

    /* EVIOCGABS(ABS_X) — 仅当设备带 EV_ABS 才有意义 */
    if (has_abs) {
        unsigned char abs_bits[(ABS_MAX + 7) / 8] = {0};
        rc = ioctl(fd, EVIOCGBIT(EV_ABS, sizeof(abs_bits)), abs_bits);
        CHECK(rc >= 0, "EVIOCGBIT(EV_ABS)");

        struct input_absinfo abs;
        memset(&abs, 0, sizeof(abs));
        rc = ioctl(fd, EVIOCGABS(ABS_X), &abs);
        CHECK(rc >= 0, "EVIOCGABS(ABS_X) returns success");
        int abs_x_ok = rc >= 0;
        if (abs_x_ok) {
            CHECK(abs.maximum > abs.minimum,
                  "absinfo.maximum > minimum (libinput can normalize)");

            int tiny_abs = 0;
            errno = 0;
            rc = ioctl(fd, EVIOCGABS_SMALL(ABS_X), &tiny_abs);
            CHECK(rc < 0 && errno == EINVAL,
                  "undersized EVIOCGABS(ABS_X) returns EINVAL");
        }

        if (abs_x_ok) {
            int unsupported_axis = first_unsupported_abs_axis(abs_bits, sizeof(abs_bits));
            if (unsupported_axis >= 0) {
                errno = 0;
                rc = ioctl(fd, EVIOCGABS(unsupported_axis), &abs);
                CHECK(rc < 0 && errno == EINVAL,
                      "unsupported EVIOCGABS(axis) returns EINVAL");
            } else {
                printf("  SKIP | %s | unsupported ABS axis check (all axes advertised)\n",
                       path);
            }
        }
    }

    close(fd);
    return 1;
}

int main(void) {
    TEST_START("evdev-prop-bits");

    DIR *d = opendir("/dev/input");
    if (!d) {
        CHECK(0, "/dev/input present");
        TEST_DONE();
    }

    int devices_seen = 0;
    struct dirent *ent;
    while ((ent = readdir(d)) != NULL) {
        if (strncmp(ent->d_name, "event", 5) != 0) continue;
        char path[64];
        snprintf(path, sizeof(path), "/dev/input/%s", ent->d_name);
        struct stat st;
        if (stat(path, &st) != 0) continue;
        devices_seen += test_one_device(path);
    }
    closedir(d);

    if (devices_seen == 0) {
        CHECK(0, "at least one /dev/input/event* device to probe");
    }
    CHECK(__pass > 0, "at least one evdev assertion ran");

    TEST_DONE();
}
