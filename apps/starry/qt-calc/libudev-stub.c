/*
 * Thin interposition library for libudev under StarryOS.
 *
 * StarryOS has no netlink socket (no udevd), which causes real libudev's
 * udev_monitor_new_from_netlink() to fail.  This .so interposes ONLY on
 * the two monitor functions that need netlink, forwarding everything else
 * to the real libudev.
 *
 * Install via LD_PRELOAD:
 *   LD_PRELOAD=/usr/lib/libudev-stub.so weston --backend=drm-backend.so ...
 *
 * Compile (with real libudev available for linking):
 *   /opt/x86_64-linux-musl-cross/bin/x86_64-linux-musl-gcc \
 *       -shared -fPIC -nostdlib \
 *       -o libudev-stub.so libudev-stub.c \
 *       -Wl,-soname,libudev-stub.so
 */
#include <stdlib.h>
#include <unistd.h>

struct udev_monitor { int pipefd[2]; };

struct udev_monitor *
udev_monitor_new_from_netlink(struct udev *u, const char *name)
{
    (void)u; (void)name;
    struct udev_monitor *m = calloc(1, sizeof(*m));
    if (m) {
        if (pipe(m->pipefd) != 0) {
            m->pipefd[0] = -1;
            m->pipefd[1] = -1;
        }
    }
    return m;
}

struct udev_monitor *udev_monitor_ref(struct udev_monitor *m)   { return m; }

struct udev_monitor *udev_monitor_unref(struct udev_monitor *m) {
    if (m) {
        if (m->pipefd[0] >= 0) close(m->pipefd[0]);
        if (m->pipefd[1] >= 0) close(m->pipefd[1]);
        free(m);
    }
    return NULL;
}

int udev_monitor_filter_add_match_subsystem_devtype(
        struct udev_monitor *m, const char *sub, const char *dev)
    { (void)m; (void)sub; (void)dev; return 0; }

int udev_monitor_enable_receiving(struct udev_monitor *m)  { (void)m; return 0; }

int udev_monitor_get_fd(struct udev_monitor *m)            { return m ? m->pipefd[0] : -1; }

struct udev_device *udev_monitor_receive_device(struct udev_monitor *m)
    { (void)m; return NULL; }
