/*
 * Minimal stub libudev for Weston under StarryOS.
 *
 * Weston's DRM backend and libinput require libudev for device
 * enumeration and hotplug monitoring.  StarryOS has no udevd,
 * and the full libudev from Alpine needs netlink.
 *
 * This stub:
 *  1. Returns a working udev monitor (pipe-based, no netlink)
 *  2. Returns the DRM device so Weston can find the GPU
 *  3. Returns ZERO input devices — Weston can start without them
 *
 * Compile:
 *   /opt/x86_64-linux-musl-cross/bin/x86_64-linux-musl-gcc \
 *       -shared -fPIC -nostdlib -o libudev.so.1 stub_libudev.c \
 *       -Wl,-soname,libudev.so.1 -Wl,--version-script=stub_libudev.map
 */
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/stat.h>
#include <unistd.h>

struct udev          { int _d; };
struct udev_device   { int _d; };
struct udev_monitor  { int pipefd[2]; };
struct udev_enumerate{ int _d; };
struct udev_list_entry { struct udev_list_entry *next; const char *name; };

/* DRM device only — always return this for any subsystem */
static struct udev_list_entry entry_drm0 = { NULL, "/sys/class/drm/card0" };
static struct udev_device dev_drm = { 0 };

/* ---- udev context ---- */
struct udev *udev_new(void)               { return calloc(1, 64); }
struct udev *udev_ref(struct udev *u)     { return u; }
struct udev *udev_unref(struct udev *u)   { free(u); return NULL; }
int udev_get_fd(struct udev *u)           { (void)u; return -1; }

/* ---- udev monitor (pipe-based, no netlink) ---- */
struct udev_monitor *
udev_monitor_new_from_netlink(struct udev *u, const char *name)
{
    (void)u; (void)name;
    struct udev_monitor *m = calloc(1, sizeof(*m));
    if (m && pipe(m->pipefd) != 0) {
        m->pipefd[0] = -1;
        m->pipefd[1] = -1;
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

/* ---- udev enumerate — DRM only ---- */
struct udev_enumerate *udev_enumerate_new(struct udev *u)
    { (void)u; return calloc(1, 64); }
struct udev_enumerate *udev_enumerate_ref(struct udev_enumerate *e)   { return e; }
struct udev_enumerate *udev_enumerate_unref(struct udev_enumerate *e) { free(e); return NULL; }
int udev_enumerate_add_match_subsystem(struct udev_enumerate *e, const char *s)
    { (void)e; (void)s; return 0; }
int udev_enumerate_add_match_sysname(struct udev_enumerate *e, const char *s)
    { (void)e; (void)s; return 0; }
int udev_enumerate_scan_devices(struct udev_enumerate *e)  { (void)e; return 0; }
struct udev_list_entry *udev_enumerate_get_list_entry(struct udev_enumerate *e)
    { (void)e; return &entry_drm0; }

/* ---- list entry ---- */
struct udev_list_entry *udev_list_entry_get_next(struct udev_list_entry *e)
    { return e ? e->next : NULL; }
const char *udev_list_entry_get_name(struct udev_list_entry *e)
    { return e ? e->name : NULL; }

/* ---- udev device (DRM only) ---- */
struct udev_device *udev_device_new_from_syspath(struct udev *u, const char *p)
    { (void)u; (void)p; return p ? &dev_drm : NULL; }
struct udev_device *udev_device_new_from_subsystem_sysname(
        struct udev *u, const char *sub, const char *sysname)
    { (void)u; (void)sub; (void)sysname; return &dev_drm; }
struct udev_device *udev_device_new_from_devnum(struct udev *u, char t, dev_t d)
    { (void)u; (void)t; (void)d; return &dev_drm; }
struct udev_device *udev_device_ref(struct udev_device *d)   { return d; }
struct udev_device *udev_device_unref(struct udev_device *d) { (void)d; return NULL; }
const char *udev_device_get_action(struct udev_device *d)    { (void)d; return NULL; }
const char *udev_device_get_devnode(struct udev_device *d)
    { (void)d; return "/dev/dri/card0"; }
const char *udev_device_get_subsystem(struct udev_device *d)
    { (void)d; return "drm"; }
const char *udev_device_get_sysname(struct udev_device *d)
    { (void)d; return "card0"; }
const char *udev_device_get_syspath(struct udev_device *d)
    { (void)d; return "/sys/class/drm/card0"; }
const char *udev_device_get_sysnum(struct udev_device *d)    { (void)d; return "0"; }
const char *udev_device_get_devtype(struct udev_device *d)
    { (void)d; return "drm_minor"; }
const char *udev_device_get_driver(struct udev_device *d)
    { (void)d; return "virtio_gpu"; }
const char *udev_device_get_sysattr_value(struct udev_device *d, const char *a)
    { (void)d; if (a && strcmp(a, "dev") == 0) return "226:0"; return NULL; }
const char *udev_device_get_property_value(struct udev_device *d, const char *k)
    { (void)d; (void)k; return NULL; }
dev_t udev_device_get_devnum(struct udev_device *d)          { (void)d; return 0; }
unsigned long long udev_device_get_seqnum(struct udev_device *d) { (void)d; return 0; }
int udev_device_get_is_initialized(struct udev_device *d)    { (void)d; return 1; }
int udev_device_has_tag(struct udev_device *d, const char *t) { (void)d; (void)t; return 0; }
struct udev *udev_device_get_udev(struct udev_device *d)     { (void)d; return NULL; }
struct udev_device *udev_device_get_parent(struct udev_device *d) { (void)d; return NULL; }
struct udev_device *udev_device_get_parent_with_subsystem_devtype(
        struct udev_device *d, const char *sub, const char *dt)
    { (void)d; (void)sub; (void)dt; return NULL; }
struct udev_list_entry *udev_device_get_devlinks_list_entry(struct udev_device *d)
    { (void)d; return NULL; }
struct udev_list_entry *udev_device_get_properties_list_entry(struct udev_device *d)
    { (void)d; return NULL; }
struct udev_list_entry *udev_device_get_sysattr_list_entry(struct udev_device *d)
    { (void)d; return NULL; }
