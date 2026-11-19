/*
 * test-desktop-virtio-devices
 *
 * Boots the same class of devices used by a basic graphical desktop and
 * verifies the public userspace contracts they must expose together:
 *
 *   - /dev/dri/card0 is a usable KMS node with a connected mode.
 *   - a dumb framebuffer can be created, mapped, written, and destroyed.
 *   - /dev/input/event* exposes both a keyboard-like device and a
 *     pointer-like device through standard evdev ioctls.
 *
 * The test intentionally avoids depending on driver internals. It is a
 * normal desktop-device behavior test, not a patch-shape regression test.
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <dirent.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <unistd.h>

struct drm_mode_create_dumb {
    uint32_t height;
    uint32_t width;
    uint32_t bpp;
    uint32_t flags;
    uint32_t handle;
    uint32_t pitch;
    uint64_t size;
};
struct drm_mode_map_dumb {
    uint32_t handle;
    uint32_t pad;
    uint64_t offset;
};
struct drm_mode_destroy_dumb {
    uint32_t handle;
};
struct drm_mode_mode_info {
    uint32_t clock;
    uint16_t hdisplay, hsync_start, hsync_end, htotal, hskew;
    uint16_t vdisplay, vsync_start, vsync_end, vtotal, vscan;
    uint32_t vrefresh, flags, kind;
    char name[32];
};
struct drm_mode_card_res {
    uint64_t fb_id_ptr, crtc_id_ptr, connector_id_ptr, encoder_id_ptr;
    uint32_t count_fbs, count_crtcs, count_connectors, count_encoders;
    uint32_t min_width, max_width, min_height, max_height;
};
struct drm_mode_get_connector {
    uint64_t encoders_ptr, modes_ptr, props_ptr, prop_values_ptr;
    uint32_t count_modes, count_props, count_encoders;
    uint32_t encoder_id, connector_id, connector_type, connector_type_id;
    uint32_t connection, mm_width, mm_height, subpixel, pad;
};

#define DRM_IOCTL_MODE_GETRESOURCES _IOWR('d', 0xA0, struct drm_mode_card_res)
#define DRM_IOCTL_MODE_GETCONNECTOR _IOWR('d', 0xA7, struct drm_mode_get_connector)
#define DRM_IOCTL_MODE_CREATE_DUMB  _IOWR('d', 0xB2, struct drm_mode_create_dumb)
#define DRM_IOCTL_MODE_MAP_DUMB     _IOWR('d', 0xB3, struct drm_mode_map_dumb)
#define DRM_IOCTL_MODE_DESTROY_DUMB _IOWR('d', 0xB4, struct drm_mode_destroy_dumb)

#define EV_KEY 0x01
#define EV_REL 0x02
#define EV_ABS 0x03
#define EV_MAX 0x1f
#define KEY_ENTER 28
#define KEY_A 30
#define REL_X 0x00
#define REL_Y 0x01
#define ABS_X 0x00
#define ABS_Y 0x01
#define KEY_MAX 0x2ff
#define REL_MAX 0x0f
#define ABS_MAX 0x3f
#define INPUT_PROP_POINTER 0x00

#ifndef EVIOCGNAME
#define EVIOCGNAME(len) _IOC(_IOC_READ, 'E', 0x06, len)
#endif
#ifndef EVIOCGID
struct input_id {
    uint16_t bustype;
    uint16_t vendor;
    uint16_t product;
    uint16_t version;
};
#define EVIOCGID _IOR('E', 0x02, struct input_id)
#endif
#ifndef EVIOCGBIT
#define EVIOCGBIT(ev, len) _IOC(_IOC_READ, 'E', 0x20 + (ev), len)
#endif
#ifndef EVIOCGPROP
#define EVIOCGPROP(len) _IOC(_IOC_READ, 'E', 0x09, len)
#endif

static int has_bit(const unsigned char *bits, size_t nbytes, size_t bit)
{
    if (bit / 8 >= nbytes) {
        return 0;
    }
    return (bits[bit / 8] >> (bit % 8)) & 1;
}

static void test_drm_device(void)
{
    int fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC | O_NONBLOCK);
    CHECK(fd >= 0, "open /dev/dri/card0");
    if (fd < 0) {
        return;
    }

    struct stat st;
    CHECK_RET(fstat(fd, &st), 0, "fstat /dev/dri/card0");
    CHECK((st.st_mode & S_IFMT) == S_IFCHR, "/dev/dri/card0 is a character device");

    uint32_t crtcs[4] = {0};
    uint32_t connectors[4] = {0};
    struct drm_mode_card_res res = {
        .crtc_id_ptr = (uint64_t)(uintptr_t)crtcs,
        .connector_id_ptr = (uint64_t)(uintptr_t)connectors,
        .count_crtcs = 4,
        .count_connectors = 4,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, &res), 0, "DRM GETRESOURCES");
    CHECK(res.count_crtcs >= 1, "DRM reports at least one CRTC");
    CHECK(res.count_connectors >= 1, "DRM reports at least one connector");

    struct drm_mode_mode_info mode = {0};
    if (res.count_connectors > 0) {
        struct drm_mode_get_connector conn = {
            .connector_id = connectors[0],
            .count_modes = 1,
            .modes_ptr = (uint64_t)(uintptr_t)&mode,
        };
        CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETCONNECTOR, &conn), 0, "DRM GETCONNECTOR");
        CHECK(conn.count_modes >= 1, "DRM connector has a mode");
        CHECK(mode.hdisplay > 0 && mode.vdisplay > 0, "DRM mode has nonzero size");
    }

    uint32_t width = mode.hdisplay ? mode.hdisplay : 64;
    uint32_t height = mode.vdisplay ? mode.vdisplay : 64;
    struct drm_mode_create_dumb dumb = {
        .width = width,
        .height = height,
        .bpp = 32,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_CREATE_DUMB, &dumb), 0, "DRM CREATE_DUMB");
    CHECK(dumb.handle != 0, "DRM dumb buffer handle is nonzero");
    CHECK(dumb.pitch >= width * 4 && dumb.size >= (uint64_t)dumb.pitch * height,
          "DRM dumb buffer pitch and size cover the mode");

    struct drm_mode_map_dumb map = { .handle = dumb.handle };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_MAP_DUMB, &map), 0, "DRM MAP_DUMB");

    uint8_t *pixels = MAP_FAILED;
    if (dumb.size > 0) {
        pixels = mmap(NULL, dumb.size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, map.offset);
        CHECK(pixels != MAP_FAILED, "mmap DRM dumb buffer");
    }
    if (pixels != MAP_FAILED) {
        pixels[0] = 0x12;
        pixels[1] = 0x34;
        pixels[dumb.size - 1] = 0x56;
        CHECK(pixels[0] == 0x12 && pixels[1] == 0x34 && pixels[dumb.size - 1] == 0x56,
              "mapped DRM dumb buffer stores user writes");
        munmap(pixels, dumb.size);
    }

    struct drm_mode_destroy_dumb destroy = { .handle = dumb.handle };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_DESTROY_DUMB, &destroy), 0, "DRM DESTROY_DUMB");
    close(fd);
}

static int classify_event_device(const char *path)
{
    int fd = open(path, O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    if (fd < 0) {
        printf("  INFO | %s | open failed errno=%d (%s)\n", path, errno, strerror(errno));
        return 0;
    }

    struct stat st;
    CHECK_RET(fstat(fd, &st), 0, "fstat event device");
    CHECK((st.st_mode & S_IFMT) == S_IFCHR, "event device is a character device");
    CHECK(major(st.st_rdev) == 13 && minor(st.st_rdev) >= 64,
          "event device uses Linux evdev major/minor range");

    char name[128] = {0};
    int rc = ioctl(fd, EVIOCGNAME(sizeof(name)), name);
    CHECK(rc >= 0, "EVIOCGNAME succeeds");
    if (rc >= 0) {
        printf("  INFO | %s | name=%s\n", path, name);
    }

    struct input_id id = {0};
    CHECK_RET(ioctl(fd, EVIOCGID, &id), 0, "EVIOCGID succeeds");

    unsigned char ev_bits[(EV_MAX + 8) / 8] = {0};
    rc = ioctl(fd, EVIOCGBIT(0, sizeof(ev_bits)), ev_bits);
    CHECK(rc >= 0, "EVIOCGBIT(0) succeeds");
    int has_keys = rc >= 0 && has_bit(ev_bits, sizeof(ev_bits), EV_KEY);
    int has_rel = rc >= 0 && has_bit(ev_bits, sizeof(ev_bits), EV_REL);
    int has_abs = rc >= 0 && has_bit(ev_bits, sizeof(ev_bits), EV_ABS);

    int is_keyboard = 0;
    if (has_keys) {
        unsigned char key_bits[(KEY_MAX + 8) / 8] = {0};
        rc = ioctl(fd, EVIOCGBIT(EV_KEY, sizeof(key_bits)), key_bits);
        CHECK(rc >= 0, "EVIOCGBIT(EV_KEY) succeeds");
        is_keyboard = rc >= 0
            && (has_bit(key_bits, sizeof(key_bits), KEY_A)
                || has_bit(key_bits, sizeof(key_bits), KEY_ENTER));
    }

    int has_pointer_axes = 0;
    if (has_rel) {
        unsigned char rel_bits[(REL_MAX + 8) / 8] = {0};
        rc = ioctl(fd, EVIOCGBIT(EV_REL, sizeof(rel_bits)), rel_bits);
        CHECK(rc >= 0, "EVIOCGBIT(EV_REL) succeeds");
        has_pointer_axes = rc >= 0
            && has_bit(rel_bits, sizeof(rel_bits), REL_X)
            && has_bit(rel_bits, sizeof(rel_bits), REL_Y);
    }
    if (!has_pointer_axes && has_abs) {
        unsigned char abs_bits[(ABS_MAX + 8) / 8] = {0};
        rc = ioctl(fd, EVIOCGBIT(EV_ABS, sizeof(abs_bits)), abs_bits);
        CHECK(rc >= 0, "EVIOCGBIT(EV_ABS) succeeds");
        has_pointer_axes = rc >= 0
            && has_bit(abs_bits, sizeof(abs_bits), ABS_X)
            && has_bit(abs_bits, sizeof(abs_bits), ABS_Y);
    }

    int is_pointer = 0;
    if (has_pointer_axes) {
        unsigned char prop_bits[8] = {0};
        rc = ioctl(fd, EVIOCGPROP(sizeof(prop_bits)), prop_bits);
        CHECK(rc >= 0, "EVIOCGPROP succeeds on pointer candidate");
        is_pointer = rc >= 0 && has_bit(prop_bits, sizeof(prop_bits), INPUT_PROP_POINTER);
    }

    struct pollfd pfd = { .fd = fd, .events = POLLIN };
    rc = poll(&pfd, 1, 0);
    CHECK(rc >= 0, "poll(event device, timeout=0) succeeds");

    close(fd);
    return (is_keyboard ? 1 : 0) | (is_pointer ? 2 : 0);
}

static void test_evdev_devices(void)
{
    DIR *dir = opendir("/dev/input");
    CHECK(dir != NULL, "open /dev/input");
    if (!dir) {
        return;
    }

    int event_devices = 0;
    int keyboard_devices = 0;
    int pointer_devices = 0;
    struct dirent *ent;
    while ((ent = readdir(dir)) != NULL) {
        if (strncmp(ent->d_name, "event", 5) != 0) {
            continue;
        }
        char path[64];
        snprintf(path, sizeof(path), "/dev/input/%s", ent->d_name);
        int kind = classify_event_device(path);
        event_devices++;
        if (kind & 1) {
            keyboard_devices++;
        }
        if (kind & 2) {
            pointer_devices++;
        }
    }
    closedir(dir);

    CHECK(event_devices >= 2, "at least two /dev/input/event* devices are present");
    CHECK(keyboard_devices >= 1, "at least one keyboard-like evdev device is present");
    CHECK(pointer_devices >= 1, "at least one pointer-like evdev device is present");
}

int main(void)
{
    TEST_START("desktop virtio devices");
    test_drm_device();
    test_evdev_devices();
    TEST_DONE();
}
