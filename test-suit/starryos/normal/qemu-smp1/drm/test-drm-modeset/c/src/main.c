/*
 * test-drm-modeset — 在 /dev/dri/card0 上跑一遍 KMS 平面 + 页面翻转 + vblank
 *
 * 覆盖：
 *   - MODE_GETPLANERESOURCES：plane 数量在 [1, 2]（只有 primary，或 primary + cursor）
 *   - MODE_GETPLANE：primary plane 报告支持 XRGB8888，possible_crtcs == 0b1
 *   - OBJ_GETPROPERTIES：plane 上必须有 type 属性，值为 PRIMARY
 *   - GETPROPERTY：type 描述为 ENUM，三个枚举值，第二个为 "Primary"
 *   - 跑一遍 SETCRTC + PAGE_FLIP_EVENT，poll 立即可读，read 拿到 drm_event_vblank
 *   - 空 read 返回 EAGAIN
 *   - WAIT_VBLANK 序列号单调递增
 *
 * 这些是 weston / mutter / Xorg-modesetting 启动时探测显卡 capabilities
 * 必走的 ioctl，覆盖到这里就能保证 simpledrm 节点对 KMS userspace 可用。
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

struct drm_mode_create_dumb {
    uint32_t height; uint32_t width; uint32_t bpp; uint32_t flags;
    uint32_t handle; uint32_t pitch; uint64_t size;
};
struct drm_mode_fb_cmd2 {
    uint32_t fb_id; uint32_t width; uint32_t height; uint32_t pixel_format;
    uint32_t flags; uint32_t handles[4]; uint32_t pitches[4];
    uint32_t offsets[4]; uint64_t modifier[4];
};
struct drm_mode_mode_info {
    uint32_t clock;
    uint16_t hdisplay, hsync_start, hsync_end, htotal, hskew;
    uint16_t vdisplay, vsync_start, vsync_end, vtotal, vscan;
    uint32_t vrefresh, flags, kind; char name[32];
};
struct drm_mode_crtc {
    uint64_t set_connectors_ptr; uint32_t count_connectors;
    uint32_t crtc_id; uint32_t fb_id; uint32_t x; uint32_t y;
    uint32_t gamma_size; uint32_t mode_valid;
    struct drm_mode_mode_info mode;
};
struct drm_mode_card_res {
    uint64_t fb_id_ptr; uint64_t crtc_id_ptr; uint64_t connector_id_ptr;
    uint64_t encoder_id_ptr;
    uint32_t count_fbs; uint32_t count_crtcs;
    uint32_t count_connectors; uint32_t count_encoders;
    uint32_t min_width, max_width, min_height, max_height;
};
struct drm_mode_get_connector {
    uint64_t encoders_ptr; uint64_t modes_ptr;
    uint64_t props_ptr; uint64_t prop_values_ptr;
    uint32_t count_modes; uint32_t count_props; uint32_t count_encoders;
    uint32_t encoder_id; uint32_t connector_id;
    uint32_t connector_type; uint32_t connector_type_id;
    uint32_t connection; uint32_t mm_width; uint32_t mm_height;
    uint32_t subpixel; uint32_t pad;
};
struct drm_mode_get_plane_res {
    uint64_t plane_id_ptr; uint32_t count_planes;
};
struct drm_mode_get_plane {
    uint32_t plane_id; uint32_t crtc_id; uint32_t fb_id;
    uint32_t possible_crtcs; uint32_t gamma_size; uint32_t count_format_types;
    uint64_t format_type_ptr;
};
struct drm_mode_obj_get_properties {
    uint64_t props_ptr; uint64_t prop_values_ptr;
    uint32_t count_props; uint32_t obj_id; uint32_t obj_type;
};
struct drm_mode_get_property {
    uint64_t values_ptr; uint64_t enum_blob_ptr;
    uint32_t prop_id; uint32_t flags; char name[32];
    uint32_t count_values; uint32_t count_enum_blobs;
};
struct drm_property_enum { uint64_t value; char name[32]; };
struct drm_mode_crtc_page_flip {
    uint32_t crtc_id; uint32_t fb_id; uint32_t flags; uint32_t reserved;
    uint64_t user_data;
};
struct drm_wait_vblank_reply {
    uint32_t type; uint32_t sequence; int64_t tv_sec; int64_t tv_usec;
};
union drm_wait_vblank {
    struct { uint32_t type; uint32_t sequence; uint64_t signal; uint64_t pad; } req;
    struct drm_wait_vblank_reply reply;
};
struct drm_event { uint32_t type; uint32_t length; };
struct drm_event_vblank {
    struct drm_event base;
    uint64_t user_data; uint32_t tv_sec; uint32_t tv_usec;
    uint32_t sequence; uint32_t crtc_id;
};

#define DRM_IOCTL_MODE_GETRESOURCES      _IOWR('d', 0xA0, struct drm_mode_card_res)
#define DRM_IOCTL_MODE_GETCRTC           _IOWR('d', 0xA1, struct drm_mode_crtc)
#define DRM_IOCTL_MODE_SETCRTC           _IOWR('d', 0xA2, struct drm_mode_crtc)
#define DRM_IOCTL_MODE_GETCONNECTOR      _IOWR('d', 0xA7, struct drm_mode_get_connector)
#define DRM_IOCTL_MODE_GETPROPERTY       _IOWR('d', 0xAA, struct drm_mode_get_property)
#define DRM_IOCTL_MODE_PAGE_FLIP         _IOWR('d', 0xB0, struct drm_mode_crtc_page_flip)
#define DRM_IOCTL_MODE_CREATE_DUMB       _IOWR('d', 0xB2, struct drm_mode_create_dumb)
#define DRM_IOCTL_MODE_GETPLANERESOURCES _IOWR('d', 0xB5, struct drm_mode_get_plane_res)
#define DRM_IOCTL_MODE_GETPLANE          _IOWR('d', 0xB6, struct drm_mode_get_plane)
#define DRM_IOCTL_MODE_ADDFB2            _IOWR('d', 0xB8, struct drm_mode_fb_cmd2)
#define DRM_IOCTL_MODE_OBJ_GETPROPERTIES _IOWR('d', 0xB9, struct drm_mode_obj_get_properties)
#define DRM_IOCTL_WAIT_VBLANK            _IOWR('d', 0x3A, union drm_wait_vblank)

#define DRM_MODE_OBJECT_PLANE       0xeeeeeeee
#define DRM_PLANE_TYPE_PRIMARY      1
#define DRM_MODE_PAGE_FLIP_EVENT    0x01
#define DRM_MODE_PROP_ENUM          (1 << 3)
#define DRM_EVENT_FLIP_COMPLETE     0x02
#define DRM_FORMAT_XRGB8888         0x34325258

int main(void)
{
    TEST_START("drm-modeset");

    int fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC | O_NONBLOCK);
    CHECK(fd >= 0, "open /dev/dri/card0");
    if (fd < 0) {
        TEST_DONE();
    }

    /* --- plane enumeration --- */
    struct drm_mode_get_plane_res pres = {0};
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETPLANERESOURCES, &pres), 0,
              "GETPLANERESOURCES probe");
    /* F+G+H+I 范围内 simpledrm 只暴露 1 个 primary plane；为了兼容后续
     * cursor 平面落地，接受 [1, 2] 区间。 */
    CHECK(pres.count_planes >= 1 && pres.count_planes <= 2,
          "plane count in [1, 2]");
    uint32_t plane_ids[2] = {0};
    pres.plane_id_ptr = (uint64_t)(uintptr_t)plane_ids;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETPLANERESOURCES, &pres), 0,
              "GETPLANERESOURCES fetch");

    uint32_t formats[4] = {0};
    struct drm_mode_get_plane pl = {0};
    pl.plane_id = plane_ids[0];
    pl.count_format_types = 4;
    pl.format_type_ptr = (uint64_t)(uintptr_t)formats;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETPLANE, &pl), 0, "GETPLANE primary");
    CHECK(pl.possible_crtcs == 1, "primary plane possible_crtcs == 0b1");
    CHECK(pl.count_format_types >= 1 && formats[0] == DRM_FORMAT_XRGB8888,
          "primary plane reports XRGB8888");

    /* --- plane properties --- */
    uint32_t prop_ids[32] = {0};
    uint64_t prop_vals[32] = {0};
    struct drm_mode_obj_get_properties props = {0};
    props.obj_id = plane_ids[0];
    props.obj_type = DRM_MODE_OBJECT_PLANE;
    props.count_props = 32;
    props.props_ptr = (uint64_t)(uintptr_t)prop_ids;
    props.prop_values_ptr = (uint64_t)(uintptr_t)prop_vals;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_OBJ_GETPROPERTIES, &props), 0,
              "OBJ_GETPROPERTIES on plane");
    CHECK(props.count_props >= 1, "plane reports >=1 prop");

    /* 找 type 属性，校验值是 PRIMARY。prop id 不在 stable uapi 中，靠名字匹配。 */
    uint32_t type_prop_id = 0;
    for (uint32_t i = 0; i < props.count_props; i++) {
        struct drm_mode_get_property probe = {0};
        probe.prop_id = prop_ids[i];
        if (ioctl(fd, DRM_IOCTL_MODE_GETPROPERTY, &probe) == 0
            && strcmp(probe.name, "type") == 0) {
            type_prop_id = prop_ids[i];
            CHECK(prop_vals[i] == DRM_PLANE_TYPE_PRIMARY,
                  "plane type value == PRIMARY");
            break;
        }
    }
    CHECK(type_prop_id != 0, "plane has 'type' property");

    /* 描述 plane 的 type property。 */
    struct drm_property_enum enums[3] = {0};
    struct drm_mode_get_property prop = {0};
    prop.prop_id = type_prop_id;
    prop.count_enum_blobs = 3;
    prop.enum_blob_ptr = (uint64_t)(uintptr_t)enums;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETPROPERTY, &prop), 0,
              "GETPROPERTY type");
    CHECK((prop.flags & DRM_MODE_PROP_ENUM) != 0, "type prop is ENUM");
    CHECK(prop.count_enum_blobs == 3, "type prop has 3 enum entries");
    CHECK(strcmp(enums[1].name, "Primary") == 0,
          "type enum[1].name == 'Primary'");

    /* --- prep a scanout fb so PAGE_FLIP has a target --- */
    struct drm_mode_card_res res = {0};
    (void)ioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, &res);
    uint32_t crtc_ids[1] = {0}, conn_ids[1] = {0};
    res.crtc_id_ptr = (uint64_t)(uintptr_t)crtc_ids;
    res.connector_id_ptr = (uint64_t)(uintptr_t)conn_ids;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, &res), 0,
              "GETRESOURCES");

    struct drm_mode_mode_info modes[1] = {0};
    struct drm_mode_get_connector conn = {0};
    conn.connector_id = conn_ids[0];
    conn.count_modes = 1;
    conn.modes_ptr = (uint64_t)(uintptr_t)modes;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETCONNECTOR, &conn), 0,
              "GETCONNECTOR");

    struct drm_mode_create_dumb cdumb = {
        .width = modes[0].hdisplay, .height = modes[0].vdisplay, .bpp = 32,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_CREATE_DUMB, &cdumb), 0, "CREATE_DUMB");
    struct drm_mode_fb_cmd2 fb = {
        .width = cdumb.width, .height = cdumb.height,
        .pixel_format = DRM_FORMAT_XRGB8888,
        .handles = { cdumb.handle, 0, 0, 0 },
        .pitches = { cdumb.pitch, 0, 0, 0 },
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_ADDFB2, &fb), 0, "ADDFB2");
    struct drm_mode_crtc setcrtc = {
        .crtc_id = crtc_ids[0], .fb_id = fb.fb_id,
        .mode_valid = 1, .mode = modes[0],
        .set_connectors_ptr = (uint64_t)(uintptr_t)conn_ids,
        .count_connectors = 1,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_SETCRTC, &setcrtc), 0, "SETCRTC");

    /* SETCRTC must reject an unknown fb_id with EINVAL — Linux DRM
     * looks up the fb in the per-card table and fails the modeset
     * if the lookup misses. */
    struct drm_mode_crtc bad_setcrtc = setcrtc;
    bad_setcrtc.fb_id = 0xdeadbeef;
    CHECK_ERR(ioctl(fd, DRM_IOCTL_MODE_SETCRTC, &bad_setcrtc), EINVAL,
              "SETCRTC with unknown fb_id rejected with EINVAL");

    /* GETCRTC must read back the connector bound by the preceding
     * SETCRTC. count_connectors reports the real bind count and
     * set_connectors_ptr (when provided) is filled with the ids. */
    uint32_t got_conns[2] = {0};
    struct drm_mode_crtc getcrtc = {
        .crtc_id = crtc_ids[0],
        .set_connectors_ptr = (uint64_t)(uintptr_t)got_conns,
        .count_connectors = 2,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETCRTC, &getcrtc), 0, "GETCRTC");
    CHECK(getcrtc.count_connectors == 1,
          "GETCRTC.count_connectors == 1 after SETCRTC");
    CHECK(got_conns[0] == conn_ids[0],
          "GETCRTC.set_connectors_ptr[0] == bound connector id");
    CHECK(getcrtc.fb_id == fb.fb_id, "GETCRTC.fb_id == post-SETCRTC fb_id");
    CHECK(getcrtc.mode_valid == 1, "GETCRTC.mode_valid == 1");

    /* SETCRTC with an unknown connector id must fail EINVAL — Linux
     * DRM rejects connector ids it can't look up. Accepting them
     * silently would pollute the modeset surface. */
    uint32_t bad_conns[1] = { 0xdeadbeef };
    struct drm_mode_crtc bad_conn_setcrtc = setcrtc;
    bad_conn_setcrtc.set_connectors_ptr = (uint64_t)(uintptr_t)bad_conns;
    bad_conn_setcrtc.count_connectors = 1;
    CHECK_ERR(ioctl(fd, DRM_IOCTL_MODE_SETCRTC, &bad_conn_setcrtc), EINVAL,
              "SETCRTC with unknown connector id rejected with EINVAL");

    /* GETPLANE must reflect the current bind state produced by the
     * preceding SETCRTC, not a hard-coded zero. */
    struct drm_mode_get_plane pl_bound = { .plane_id = pl.plane_id };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETPLANE, &pl_bound), 0,
              "GETPLANE after SETCRTC");
    CHECK(pl_bound.fb_id == fb.fb_id, "GETPLANE.fb_id == post-SETCRTC fb_id");
    CHECK(pl_bound.crtc_id == crtc_ids[0],
          "GETPLANE.crtc_id == post-SETCRTC crtc_id");

    /* --- page flip with event --- */
    struct drm_mode_crtc_page_flip flip = {
        .crtc_id = crtc_ids[0], .fb_id = fb.fb_id,
        .flags = DRM_MODE_PAGE_FLIP_EVENT,
        .user_data = 0xdeadbeefcafebabeULL,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_PAGE_FLIP, &flip), 0,
              "PAGE_FLIP (with event)");

    struct pollfd pfd = { .fd = fd, .events = POLLIN };
    int pr = poll(&pfd, 1, 2000);
    CHECK(pr == 1 && (pfd.revents & POLLIN), "poll returns POLLIN");

    struct drm_event_vblank ev = {0};
    ssize_t n = read(fd, &ev, sizeof(ev));
    CHECK(n == (ssize_t)sizeof(ev), "read returns full drm_event_vblank");
    CHECK(ev.base.type == DRM_EVENT_FLIP_COMPLETE,
          "event type == FLIP_COMPLETE");
    CHECK(ev.base.length == sizeof(ev), "event length == sizeof(event)");
    CHECK(ev.user_data == 0xdeadbeefcafebabeULL,
          "event user_data round-trips");
    CHECK(ev.crtc_id == crtc_ids[0], "event crtc_id matches");
    uint32_t seq1 = ev.sequence;

    /* 队列空时 read 应该 EAGAIN（fd 是 O_NONBLOCK）。 */
    char buf[64] = {0};
    CHECK_ERR(read(fd, buf, sizeof(buf)), EAGAIN, "empty read returns EAGAIN");

    /* --- WAIT_VBLANK 单调递增 --- */
    union drm_wait_vblank wv1 = {0}, wv2 = {0};
    CHECK_RET(ioctl(fd, DRM_IOCTL_WAIT_VBLANK, &wv1), 0, "WAIT_VBLANK 1");
    CHECK_RET(ioctl(fd, DRM_IOCTL_WAIT_VBLANK, &wv2), 0, "WAIT_VBLANK 2");
    CHECK(wv2.reply.sequence > wv1.reply.sequence, "vblank seq monotonic");
    CHECK(wv2.reply.sequence > seq1, "vblank seq > flip seq");

    close(fd);
    TEST_DONE();
}
