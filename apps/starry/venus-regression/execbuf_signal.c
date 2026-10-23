#define _GNU_SOURCE
#include "execbuf_signal.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include <libdrm/drm.h>
#include <libdrm/virtgpu_drm.h>

/* libdrm's drm_mode.h does not pull in drm_fourcc.h. */
#ifndef DRM_FORMAT_XRGB8888
#define DRM_FORMAT_XRGB8888 0x34325258
#endif
#ifndef DRM_PLANE_TYPE_PRIMARY
#define DRM_PLANE_TYPE_PRIMARY 1
#endif
#ifndef DRM_MODE_CONNECTED
#define DRM_MODE_CONNECTED 1
#endif
#ifndef DRM_EVENT_FLIP_COMPLETE
#define DRM_EVENT_FLIP_COMPLETE 0x02
#endif

#define CHECK(condition, message) \
    do { \
        if (!(condition)) \
            fail_at(__LINE__, message); \
    } while (0)

static void fail_at(int line, const char *message)
{
    fprintf(stderr,
            "VENUS_REGRESSION_FAILED line=%d detail=%s errno=%d (%s)\n",
            line, message, errno, strerror(errno));
    exit(1);
}

static int64_t monotonic_ns(void)
{
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0)
        return -1;
    return (int64_t)now.tv_sec * INT64_C(1000000000) + now.tv_nsec;
}

static uint32_t find_prop(int fd, uint32_t object, uint32_t object_type,
                          const char *name)
{
    uint32_t ids[64] = {0};
    uint64_t values[64] = {0};
    struct drm_mode_obj_get_properties query = {
        .props_ptr = (uint64_t)(uintptr_t)ids,
        .prop_values_ptr = (uint64_t)(uintptr_t)values,
        .count_props = 64,
        .obj_id = object,
        .obj_type = object_type,
    };
    if (ioctl(fd, DRM_IOCTL_MODE_OBJ_GETPROPERTIES, &query) != 0)
        return 0;
    for (uint32_t i = 0; i < query.count_props && i < 64; i++) {
        struct drm_mode_get_property property = {.prop_id = ids[i]};
        if (ioctl(fd, DRM_IOCTL_MODE_GETPROPERTY, &property) == 0 &&
            strcmp(property.name, name) == 0)
            return ids[i];
    }
    return 0;
}

static uint64_t obj_prop_value(int fd, uint32_t object, uint32_t object_type,
                               uint32_t prop)
{
    uint32_t ids[64] = {0};
    uint64_t values[64] = {0};
    struct drm_mode_obj_get_properties query = {
        .props_ptr = (uint64_t)(uintptr_t)ids,
        .prop_values_ptr = (uint64_t)(uintptr_t)values,
        .count_props = 64,
        .obj_id = object,
        .obj_type = object_type,
    };
    if (ioctl(fd, DRM_IOCTL_MODE_OBJ_GETPROPERTIES, &query) != 0)
        return ~0ULL;
    for (uint32_t i = 0; i < query.count_props && i < 64; i++)
        if (ids[i] == prop)
            return values[i];
    return ~0ULL;
}

struct kms {
    int fd;
    uint32_t conn;
    uint32_t crtc;
    uint32_t plane;
    uint32_t fb[2];
    uint32_t width;
    uint32_t height;
    struct drm_mode_modeinfo mode;

    uint32_t conn_crtc;
    uint32_t crtc_active;
    uint32_t crtc_mode;
    uint32_t crtc_out_fence;
    uint32_t plane_fb;
    uint32_t plane_crtc;
    uint32_t plane_src_x;
    uint32_t plane_src_y;
    uint32_t plane_src_w;
    uint32_t plane_src_h;
    uint32_t plane_crtc_x;
    uint32_t plane_crtc_y;
    uint32_t plane_crtc_w;
    uint32_t plane_crtc_h;
};

static void kms_open(struct kms *kms)
{
    kms->fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC | O_NONBLOCK);
    CHECK(kms->fd >= 0, "open /dev/dri/card0");

    struct drm_set_client_cap cap = {
        .capability = DRM_CLIENT_CAP_UNIVERSAL_PLANES,
        .value = 1,
    };
    CHECK(ioctl(kms->fd, DRM_IOCTL_SET_CLIENT_CAP, &cap) == 0,
          "SET_CLIENT_CAP UNIVERSAL_PLANES");
    cap.capability = DRM_CLIENT_CAP_ATOMIC;
    CHECK(ioctl(kms->fd, DRM_IOCTL_SET_CLIENT_CAP, &cap) == 0,
          "SET_CLIENT_CAP ATOMIC");

    uint32_t crtcs[8] = {0};
    uint32_t connectors[8] = {0};
    struct drm_mode_card_res resources = {
        .crtc_id_ptr = (uint64_t)(uintptr_t)crtcs,
        .connector_id_ptr = (uint64_t)(uintptr_t)connectors,
        .count_crtcs = 8,
        .count_connectors = 8,
    };
    CHECK(ioctl(kms->fd, DRM_IOCTL_MODE_GETRESOURCES, &resources) == 0,
          "GETRESOURCES");
    CHECK(resources.count_crtcs >= 1 && resources.count_crtcs <= 8,
          "GETRESOURCES returns a bounded CRTC set");
    CHECK(resources.count_connectors >= 1 && resources.count_connectors <= 8,
          "GETRESOURCES returns a bounded connector set");
    kms->crtc = crtcs[0];

    for (uint32_t i = 0; i < resources.count_connectors; i++) {
        struct drm_mode_get_connector connector = {
            .connector_id = connectors[i],
        };
        CHECK(ioctl(kms->fd, DRM_IOCTL_MODE_GETCONNECTOR, &connector) == 0,
              "GETCONNECTOR probe");
        if (connector.connection != DRM_MODE_CONNECTED ||
            connector.count_modes == 0)
            continue;

        struct drm_mode_modeinfo modes[64] = {0};
        connector.count_modes = 64;
        connector.count_props = 0;
        connector.count_encoders = 0;
        connector.modes_ptr = (uint64_t)(uintptr_t)modes;
        CHECK(ioctl(kms->fd, DRM_IOCTL_MODE_GETCONNECTOR, &connector) == 0,
              "GETCONNECTOR modes");
        if (connector.count_modes == 0)
            continue;
        kms->conn = connectors[i];
        kms->mode = modes[0];
        kms->width = modes[0].hdisplay;
        kms->height = modes[0].vdisplay;
        break;
    }
    CHECK(kms->conn != 0 && kms->width != 0 && kms->height != 0,
          "connected KMS connector with a mode");

    uint32_t planes[8] = {0};
    struct drm_mode_get_plane_res plane_resources = {
        .plane_id_ptr = (uint64_t)(uintptr_t)planes,
        .count_planes = 8,
    };
    CHECK(ioctl(kms->fd, DRM_IOCTL_MODE_GETPLANERESOURCES,
                &plane_resources) == 0,
          "GETPLANERESOURCES");
    CHECK(plane_resources.count_planes >= 1 &&
              plane_resources.count_planes <= 8,
          "GETPLANERESOURCES returns a bounded plane set");
    for (uint32_t i = 0; i < plane_resources.count_planes; i++) {
        uint32_t type = find_prop(kms->fd, planes[i], DRM_MODE_OBJECT_PLANE,
                                  "type");
        if (type != 0 &&
            obj_prop_value(kms->fd, planes[i], DRM_MODE_OBJECT_PLANE,
                           type) == DRM_PLANE_TYPE_PRIMARY) {
            kms->plane = planes[i];
            break;
        }
    }
    CHECK(kms->plane != 0, "primary KMS plane");

    for (unsigned i = 0; i < 2; i++) {
        struct drm_mode_create_dumb dumb = {
            .width = kms->width,
            .height = kms->height,
            .bpp = 32,
        };
        CHECK(ioctl(kms->fd, DRM_IOCTL_MODE_CREATE_DUMB, &dumb) == 0,
              "CREATE_DUMB");
        struct drm_mode_fb_cmd2 framebuffer = {
            .width = kms->width,
            .height = kms->height,
            .pixel_format = DRM_FORMAT_XRGB8888,
            .handles = {dumb.handle, 0, 0, 0},
            .pitches = {dumb.pitch, 0, 0, 0},
        };
        CHECK(ioctl(kms->fd, DRM_IOCTL_MODE_ADDFB2, &framebuffer) == 0,
              "ADDFB2");
        kms->fb[i] = framebuffer.fb_id;
    }

    kms->conn_crtc = find_prop(kms->fd, kms->conn, DRM_MODE_OBJECT_CONNECTOR,
                               "CRTC_ID");
    kms->crtc_active = find_prop(kms->fd, kms->crtc, DRM_MODE_OBJECT_CRTC,
                                 "ACTIVE");
    kms->crtc_mode = find_prop(kms->fd, kms->crtc, DRM_MODE_OBJECT_CRTC,
                               "MODE_ID");
    kms->crtc_out_fence = find_prop(kms->fd, kms->crtc, DRM_MODE_OBJECT_CRTC,
                                    "OUT_FENCE_PTR");
    kms->plane_fb = find_prop(kms->fd, kms->plane, DRM_MODE_OBJECT_PLANE,
                              "FB_ID");
    kms->plane_crtc = find_prop(kms->fd, kms->plane, DRM_MODE_OBJECT_PLANE,
                                "CRTC_ID");
    kms->plane_src_x = find_prop(kms->fd, kms->plane, DRM_MODE_OBJECT_PLANE,
                                 "SRC_X");
    kms->plane_src_y = find_prop(kms->fd, kms->plane, DRM_MODE_OBJECT_PLANE,
                                 "SRC_Y");
    kms->plane_src_w = find_prop(kms->fd, kms->plane, DRM_MODE_OBJECT_PLANE,
                                 "SRC_W");
    kms->plane_src_h = find_prop(kms->fd, kms->plane, DRM_MODE_OBJECT_PLANE,
                                 "SRC_H");
    kms->plane_crtc_x = find_prop(kms->fd, kms->plane, DRM_MODE_OBJECT_PLANE,
                                  "CRTC_X");
    kms->plane_crtc_y = find_prop(kms->fd, kms->plane, DRM_MODE_OBJECT_PLANE,
                                  "CRTC_Y");
    kms->plane_crtc_w = find_prop(kms->fd, kms->plane, DRM_MODE_OBJECT_PLANE,
                                  "CRTC_W");
    kms->plane_crtc_h = find_prop(kms->fd, kms->plane, DRM_MODE_OBJECT_PLANE,
                                  "CRTC_H");
    CHECK(kms->conn_crtc != 0 && kms->crtc_active != 0 &&
              kms->crtc_mode != 0 && kms->crtc_out_fence != 0,
          "connector/CRTC properties");
    CHECK(kms->plane_fb != 0 && kms->plane_crtc != 0 &&
              kms->plane_src_x != 0 && kms->plane_src_y != 0 &&
              kms->plane_src_w != 0 && kms->plane_src_h != 0 &&
              kms->plane_crtc_x != 0 && kms->plane_crtc_y != 0 &&
              kms->plane_crtc_w != 0 && kms->plane_crtc_h != 0,
          "primary plane properties");
}

/*
 * The KMS backend programs RDIF with the connector resolution but derives its
 * software vblank period from the DRM mode timing. Give the CRTC the same
 * resolution with a low dot clock so the out-fence stays pending for roughly
 * two seconds: enough to observe a real blocked EXECBUF and deliver signals,
 * while staying inside the core's i32 frame-duration field.
 */
static uint32_t create_slow_mode_blob(struct kms *kms)
{
    struct drm_mode_modeinfo mode = kms->mode;
    uint64_t pixels = (uint64_t)kms->width * kms->height;
    uint64_t clock = (pixels + 1999) / 2000;

    if (clock < 100)
        clock = 100;
    mode.clock = (uint32_t)clock;
    mode.hdisplay = (uint16_t)kms->width;
    mode.hsync_start = (uint16_t)kms->width;
    mode.hsync_end = (uint16_t)kms->width;
    mode.htotal = (uint16_t)kms->width;
    mode.hskew = 0;
    mode.vdisplay = (uint16_t)kms->height;
    mode.vsync_start = (uint16_t)kms->height;
    mode.vsync_end = (uint16_t)kms->height;
    mode.vtotal = (uint16_t)kms->height;
    mode.vscan = 0;
    mode.vrefresh = 0;
    mode.flags = 0;
    mode.type = 0;
    memset(mode.name, 0, sizeof(mode.name));
    strncpy(mode.name, "execbuf-signal", sizeof(mode.name) - 1);

    struct drm_mode_create_blob blob = {
        .data = (uint64_t)(uintptr_t)&mode,
        .length = sizeof(mode),
    };
    CHECK(ioctl(kms->fd, DRM_IOCTL_MODE_CREATEPROPBLOB, &blob) == 0,
          "CREATEPROPBLOB slow mode");
    CHECK(blob.blob_id != 0, "slow mode blob id");
    return blob.blob_id;
}

static int atomic_enable(struct kms *kms, uint32_t mode_blob)
{
    int out_fence = -1;
    uint32_t objects[3] = {kms->conn, kms->crtc, kms->plane};
    uint32_t counts[3] = {1, 3, 10};
    uint32_t props[14] = {
        kms->conn_crtc,
        kms->crtc_active, kms->crtc_mode, kms->crtc_out_fence,
        kms->plane_fb, kms->plane_crtc,
        kms->plane_src_x, kms->plane_src_y, kms->plane_src_w,
        kms->plane_src_h,
        kms->plane_crtc_x, kms->plane_crtc_y, kms->plane_crtc_w,
        kms->plane_crtc_h,
    };
    uint64_t values[14] = {
        kms->crtc,
        1, mode_blob, (uint64_t)(uintptr_t)&out_fence,
        kms->fb[0], kms->crtc,
        0, 0, (uint64_t)kms->width << 16, (uint64_t)kms->height << 16,
        0, 0, kms->width, kms->height,
    };
    struct drm_mode_atomic atomic = {
        .flags = DRM_MODE_ATOMIC_NONBLOCK | DRM_MODE_ATOMIC_ALLOW_MODESET |
                 DRM_MODE_PAGE_FLIP_EVENT,
        .count_objs = 3,
        .objs_ptr = (uint64_t)(uintptr_t)objects,
        .count_props_ptr = (uint64_t)(uintptr_t)counts,
        .props_ptr = (uint64_t)(uintptr_t)props,
        .prop_values_ptr = (uint64_t)(uintptr_t)values,
        .user_data = 0xE1,
    };
    CHECK(syscall(SYS_ioctl, kms->fd, DRM_IOCTL_MODE_ATOMIC, &atomic) == 0,
          "nonblocking atomic enable");
    CHECK(out_fence >= 0, "atomic enable installs an out-fence");
    return out_fence;
}

static int atomic_flip(struct kms *kms, uint32_t fb)
{
    int out_fence = -1;
    uint32_t objects[2] = {kms->crtc, kms->plane};
    uint32_t counts[2] = {1, 1};
    uint32_t props[2] = {kms->crtc_out_fence, kms->plane_fb};
    uint64_t values[2] = {(uint64_t)(uintptr_t)&out_fence, fb};
    struct drm_mode_atomic atomic = {
        .flags = DRM_MODE_ATOMIC_NONBLOCK | DRM_MODE_PAGE_FLIP_EVENT,
        .count_objs = 2,
        .objs_ptr = (uint64_t)(uintptr_t)objects,
        .count_props_ptr = (uint64_t)(uintptr_t)counts,
        .props_ptr = (uint64_t)(uintptr_t)props,
        .prop_values_ptr = (uint64_t)(uintptr_t)values,
        .user_data = 0xE2,
    };
    CHECK(syscall(SYS_ioctl, kms->fd, DRM_IOCTL_MODE_ATOMIC, &atomic) == 0,
          "nonblocking atomic flip");
    CHECK(out_fence >= 0, "atomic flip installs an out-fence");
    return out_fence;
}

static int open_render_node(void)
{
    int fd = open("/dev/dri/renderD128", O_RDWR | O_CLOEXEC);
    CHECK(fd >= 0, "open /dev/dri/renderD128");

    uint64_t features = 0;
    struct drm_virtgpu_getparam getparam = {
        .param = VIRTGPU_PARAM_3D_FEATURES,
        .value = (uint64_t)(uintptr_t)&features,
    };
    CHECK(ioctl(fd, DRM_IOCTL_VIRTGPU_GETPARAM, &getparam) == 0,
          "GETPARAM 3D_FEATURES");
    CHECK(features == 1, "EXECBUF requires 3D features");

    struct drm_virtgpu_context_set_param params[2] = {
        {
            .param = VIRTGPU_CONTEXT_PARAM_CAPSET_ID,
            .value = VIRTGPU_DRM_CAPSET_VIRGL,
        },
        {
            .param = VIRTGPU_CONTEXT_PARAM_NUM_RINGS,
            .value = 1,
        },
    };
    struct drm_virtgpu_context_init context = {
        .num_params = 2,
        .ctx_set_params = (uint64_t)(uintptr_t)params,
    };
    CHECK(syscall(SYS_ioctl, fd, DRM_IOCTL_VIRTGPU_CONTEXT_INIT, &context) == 0,
          "CONTEXT_INIT virgl");
    return fd;
}

static uint32_t syncobj_create(int fd)
{
    struct drm_syncobj_create create = {0};
    CHECK(ioctl(fd, DRM_IOCTL_SYNCOBJ_CREATE, &create) == 0,
          "SYNCOBJ_CREATE");
    CHECK(create.handle != 0, "syncobj handle is nonzero");
    return create.handle;
}

static uint32_t syncobj_import(int fd, int sync_file_fd)
{
    uint32_t handle = syncobj_create(fd);
    struct drm_syncobj_handle request = {
        .handle = handle,
        .flags = DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE,
        .fd = sync_file_fd,
    };
    CHECK(ioctl(fd, DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE, &request) == 0,
          "SYNCOBJ_FD_TO_HANDLE imports the pending out-fence");
    return handle;
}

static void syncobj_destroy(int fd, uint32_t handle)
{
    struct drm_syncobj_destroy destroy = {.handle = handle};
    CHECK(ioctl(fd, DRM_IOCTL_SYNCOBJ_DESTROY, &destroy) == 0,
          "SYNCOBJ_DESTROY");
}

static int syncobj_wait_now(int fd, uint32_t handle)
{
    struct drm_syncobj_wait wait = {
        .handles = (uint64_t)(uintptr_t)&handle,
        .timeout_nsec = 0,
        .count_handles = 1,
    };
    errno = 0;
    return (int)syscall(SYS_ioctl, fd, DRM_IOCTL_SYNCOBJ_WAIT, &wait);
}

static void poll_readable(int fd, int timeout_ms, const char *message)
{
    struct pollfd descriptor = {.fd = fd, .events = POLLIN};
    int result;
    do {
        result = poll(&descriptor, 1, timeout_ms);
    } while (result < 0 && errno == EINTR);
    CHECK(result == 1 && (descriptor.revents & POLLIN) != 0, message);
}

static void drain_vblank_event(int fd, uint64_t expected_user_data,
                               const char *message)
{
    poll_readable(fd, 10000, message);
    struct drm_event_vblank event = {0};
    CHECK(read(fd, &event, sizeof(event)) == (ssize_t)sizeof(event), message);
    CHECK(event.base.type == DRM_EVENT_VBLANK ||
              event.base.type == DRM_EVENT_FLIP_COMPLETE,
          "expected a vblank/flip event");
    CHECK(event.user_data == expected_user_data, "vblank event user_data");
}

static void read_exact(int fd, void *buffer, size_t length, int timeout_ms,
                       const char *message)
{
    struct pollfd descriptor = {.fd = fd, .events = POLLIN};
    int result;
    do {
        result = poll(&descriptor, 1, timeout_ms);
    } while (result < 0 && errno == EINTR);
    CHECK(result == 1 && (descriptor.revents & POLLIN) != 0, message);
    CHECK(read(fd, buffer, length) == (ssize_t)length, message);
}

/* After the readiness byte the child executes only EXECBUF. Observe it
 * sleeping through procfs before delivering the signal: the pending KMS
 * out-fence is the only thing that can keep this wait asleep. */
static int child_is_sleeping(pid_t child, int timeout_ms)
{
    char path[64];
    char contents[512];
    snprintf(path, sizeof(path), "/proc/%ld/stat", (long)child);
    int64_t deadline = monotonic_ns() + (int64_t)timeout_ms * 1000000;
    while (monotonic_ns() < deadline) {
        int fd = open(path, O_RDONLY | O_CLOEXEC);
        if (fd < 0)
            return 0;
        ssize_t length = read(fd, contents, sizeof(contents) - 1);
        close(fd);
        if (length > 0) {
            contents[length] = '\0';
            char *end_name = strrchr(contents, ')');
            if (end_name && end_name[1] == ' ' && end_name[2] == 'S')
                return 1;
        }
        sched_yield();
    }
    return 0;
}

static int handler_write_fd = -1;

static void signal_handler(int signo)
{
    (void)signo;
    char byte = 'H';
    (void)write(handler_write_fd, &byte, 1);
}

static void child_ensure_context(int render_fd)
{
    struct drm_virtgpu_context_set_param params[2] = {
        {
            .param = VIRTGPU_CONTEXT_PARAM_CAPSET_ID,
            .value = VIRTGPU_DRM_CAPSET_VIRGL,
        },
        {
            .param = VIRTGPU_CONTEXT_PARAM_NUM_RINGS,
            .value = 1,
        },
    };
    struct drm_virtgpu_context_init context = {
        .num_params = 2,
        .ctx_set_params = (uint64_t)(uintptr_t)params,
    };
    errno = 0;
    if (syscall(SYS_ioctl, render_fd, DRM_IOCTL_VIRTGPU_CONTEXT_INIT,
                &context) == 0)
        return;
    /* fork may share the parent's already-initialized virgl context. */
    if (errno == EEXIST)
        return;
    fprintf(stderr, "  child CONTEXT_INIT failed errno=%d (%s)\n", errno,
            strerror(errno));
    _exit(23);
}

static void child_run(int render_fd, int sync_file_fd, uint32_t syncobj,
                      int use_syncobj, int restart, int ready_fd,
                      int result_fd, int signal_fd)
{
    handler_write_fd = signal_fd;
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_handler = signal_handler;
    sigemptyset(&action.sa_mask);
    action.sa_flags = restart ? SA_RESTART : 0;
    if (sigaction(SIGUSR1, &action, NULL) != 0)
        _exit(20);
    child_ensure_context(render_fd);

    uint32_t command = 0;
    struct drm_virtgpu_execbuffer_syncobj input = {
        .handle = syncobj,
        .flags = VIRTGPU_EXECBUF_SYNCOBJ_RESET,
    };
    struct drm_virtgpu_execbuffer request = {
        .size = sizeof(command),
        .command = (uint64_t)(uintptr_t)&command,
        .fence_fd = -1,
        .syncobj_stride = sizeof(input),
    };
    if (use_syncobj) {
        request.num_in_syncobjs = 1;
        request.in_syncobjs = (uint64_t)(uintptr_t)&input;
    } else {
        request.flags = VIRTGPU_EXECBUF_FENCE_FD_IN;
        request.fence_fd = sync_file_fd;
    }

    char ready = 'R';
    if (write(ready_fd, &ready, 1) != 1)
        _exit(21);
    errno = 0;
    long result = syscall(SYS_ioctl, render_fd,
                          DRM_IOCTL_VIRTGPU_EXECBUFFER, &request);
    int status[2] = {(int)result, errno};
    if (write(result_fd, status, sizeof(status)) != sizeof(status))
        _exit(22);
    _exit(0);
}

static void run_case(const char *name, int render_fd, int sync_file_fd,
                     uint32_t syncobj, int use_syncobj, int restart)
{
    if (sync_file_fd >= 0) {
        struct pollfd pending = {.fd = sync_file_fd, .events = POLLIN};
        CHECK(poll(&pending, 1, 0) == 0,
              "EXECBUF input fence is pending before the child starts");
    }
    int ready[2];
    int result[2];
    int handler[2];
    CHECK(pipe2(ready, O_CLOEXEC) == 0, "ready pipe");
    CHECK(pipe2(result, O_CLOEXEC) == 0, "result pipe");
    CHECK(pipe2(handler, O_CLOEXEC) == 0, "handler pipe");

    pid_t child = fork();
    CHECK(child >= 0, "fork");
    if (child == 0) {
        close(ready[0]);
        close(result[0]);
        close(handler[0]);
        child_run(render_fd, sync_file_fd, syncobj, use_syncobj, restart,
                  ready[1], result[1], handler[1]);
        _exit(30);
    }
    close(ready[1]);
    close(result[1]);
    close(handler[1]);

    char byte = 0;
    read_exact(ready[0], &byte, 1, 2000, "child announces EXECBUF entry");
    CHECK(byte == 'R', "child readiness byte");
    if (!child_is_sleeping(child, 2000)) {
        struct pollfd early = {.fd = result[0], .events = POLLIN};
        if (poll(&early, 1, 0) == 1) {
            int status[2] = {-2, 0};
            ssize_t count = read(result[0], status, sizeof(status));
            fprintf(stderr,
                    "  case=%s early EXECBUF result bytes=%zd ret=%d errno=%d\n",
                    name, count, status[0], status[1]);
        } else {
            fprintf(stderr, "  case=%s no early EXECBUF result yet\n", name);
        }
        int child_status = 0;
        pid_t reaped = waitpid(child, &child_status, WNOHANG);
        fprintf(stderr, "  case=%s child reaped=%ld status=%d\n", name,
                (long)reaped, child_status);
        fail_at(__LINE__,
                "EXECBUF is blocked on the unfinished input fence");
    }

    CHECK(syscall(SYS_kill, child, SIGUSR1) == 0,
          "deliver signal to blocked EXECBUF");
    read_exact(handler[0], &byte, 1, 2000,
               "signal handler runs while EXECBUF waits");
    CHECK(byte == 'H', "first handler byte");

    if (restart) {
        CHECK(child_is_sleeping(child, 1000),
              "restarted EXECBUF is blocked again before the fence completes");
        CHECK(syscall(SYS_kill, child, SIGUSR1) == 0,
              "deliver a second signal to the restarted wait");
        read_exact(handler[0], &byte, 1, 2000,
                   "second signal handler runs after restart");
        CHECK(byte == 'H', "second handler byte");
    }

    int status[2] = {-2, 0};
    read_exact(result[0], status, sizeof(status), 15000,
               restart ? "restarted EXECBUF completes" :
                         "interrupted EXECBUF returns");
    int child_status = 0;
    CHECK(waitpid(child, &child_status, 0) == child, "reap EXECBUF child");

    if (restart) {
        if (status[0] != 0)
            fprintf(stderr, "  case=%s restart ret=%d errno=%d\n", name,
                    status[0], status[1]);
        CHECK(status[0] == 0,
              "restarted EXECBUF completes successfully after the fence");
    } else {
        if (status[0] != -1 || status[1] != EINTR)
            fprintf(stderr, "  case=%s interrupt ret=%d errno=%d\n", name,
                    status[0], status[1]);
        CHECK(status[0] == -1 && status[1] == EINTR,
              "interrupted EXECBUF returns -1/EINTR");
    }
    CHECK(WIFEXITED(child_status) && WEXITSTATUS(child_status) == 0,
          "EXECBUF child exits cleanly");

    close(ready[0]);
    close(result[0]);
    close(handler[0]);
    printf("EXECBUF_SIGNAL_CASE_OK name=%s restart=%d use_syncobj=%d\n", name,
           restart, use_syncobj);
}

int execbuf_signal_test(void)
{
    struct kms kms = {0};
    int render_fd = open_render_node();
    kms_open(&kms);

    uint32_t mode_blob = create_slow_mode_blob(&kms);
    int initial_fence = atomic_enable(&kms, mode_blob);

    /* The initial modeset installs the out-fence while simple-KMS is still
     * before drm_crtc_vblank_on(), so that commit's event/fence is delivered
     * immediately. Consume it; the block/restart cases below use a page flip
     * on the now-active CRTC, which is the path that arms at vblank. */
    drain_vblank_event(kms.fd, 0xE1,
                       "initial mode-set completion event");
    close(initial_fence);

    int case_fence = atomic_flip(&kms, kms.fb[1]);
    uint32_t imported = syncobj_import(render_fd, case_fence);

    run_case("fd-eintr", render_fd, case_fence, 0, 0, 0);
    /* The backing fence lets the parent assert it is pending; the child
     * itself waits through the imported syncobj. */
    run_case("syncobj-eintr", render_fd, case_fence, imported, 1, 0);

    /* EINTR may leave the imported syncobj pending, but it must never have
     * consumed the fence: the reset happens only after a successful wait. */
    int pending = syncobj_wait_now(render_fd, imported);
    CHECK(pending == 0 || (pending == -1 && errno == ETIME),
          "interrupted input syncobj was not reset");
    struct pollfd fence_poll = {.fd = case_fence, .events = POLLIN};
    CHECK(poll(&fence_poll, 1, 0) == 0,
          "page-flip out-fence is still pending before the restart case");

    run_case("fd-restart", render_fd, case_fence, 0, 0, 1);
    poll_readable(case_fence, 10000, "first page-flip out-fence completes");
    drain_vblank_event(kms.fd, 0xE2,
                       "the first page flip is presented on a vblank");
    close(case_fence);
    syncobj_destroy(render_fd, imported);

    /* A fresh page flip gives the second restart case its own fence. It is
     * issued immediately after the previous vblank, leaving a full software
     * frame for the blocked signal/restart sequence. */
    int second_fence = atomic_flip(&kms, kms.fb[0]);
    uint32_t imported_second = syncobj_import(render_fd, second_fence);

    run_case("syncobj-restart", render_fd, second_fence, imported_second, 1, 1);
    poll_readable(second_fence, 10000,
                  "second display out-fence completes after restart");
    drain_vblank_event(kms.fd, 0xE2,
                       "the second page flip is presented on a later vblank");

    /* VIRTGPU_EXECBUF_SYNCOBJ_RESET consumed the input fence after the last
     * successful wait. An empty binary syncobj reports EINVAL, not ETIME: the
     * command was submitted exactly once, after the restarted wait, and no
     * later retry could re-use the cleared syncobj. */
    pending = syncobj_wait_now(render_fd, imported_second);
    CHECK(pending == -1 && errno == EINVAL,
          "successful EXECBUF reset the imported input syncobj");

    close(second_fence);
    syncobj_destroy(render_fd, imported_second);
    close(kms.fd);
    close(render_fd);
    puts("EXECBUF_SIGNAL_PASSED fd_eintr=1 syncobj_eintr=1 fd_restart=1 "
         "syncobj_restart=1");
    return 0;
}
