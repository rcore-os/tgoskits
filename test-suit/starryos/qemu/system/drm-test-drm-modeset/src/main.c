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
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
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
#define DRM_IOCTL_MODE_RMFB              _IOWR('d', 0xAF, uint32_t)
#define DRM_IOCTL_MODE_GETCONNECTOR      _IOWR('d', 0xA7, struct drm_mode_get_connector)
#define DRM_IOCTL_MODE_GETPROPERTY       _IOWR('d', 0xAA, struct drm_mode_get_property)
#define DRM_IOCTL_MODE_PAGE_FLIP         _IOWR('d', 0xB0, struct drm_mode_crtc_page_flip)
#define DRM_IOCTL_MODE_CREATE_DUMB       _IOWR('d', 0xB2, struct drm_mode_create_dumb)
#define DRM_IOCTL_MODE_GETPLANERESOURCES _IOWR('d', 0xB5, struct drm_mode_get_plane_res)
#define DRM_IOCTL_MODE_GETPLANE          _IOWR('d', 0xB6, struct drm_mode_get_plane)
#define DRM_IOCTL_MODE_ADDFB2            _IOWR('d', 0xB8, struct drm_mode_fb_cmd2)
#define DRM_IOCTL_MODE_OBJ_GETPROPERTIES _IOWR('d', 0xB9, struct drm_mode_obj_get_properties)
#define DRM_IOCTL_WAIT_VBLANK            _IOWR('d', 0x3A, union drm_wait_vblank)
#define DRM_IOCTL_CRTC_GET_SEQUENCE      _IOWR('d', 0x3B, struct drm_crtc_get_sequence)
#define DRM_IOCTL_CRTC_QUEUE_SEQUENCE    _IOWR('d', 0x3C, struct drm_crtc_queue_sequence)

#define DRM_MODE_OBJECT_PLANE       0xeeeeeeee
#define DRM_PLANE_TYPE_PRIMARY      1
#define DRM_MODE_PAGE_FLIP_EVENT    0x01
#define DRM_MODE_PROP_ENUM          (1 << 3)
#define DRM_EVENT_FLIP_COMPLETE     0x02
#define DRM_FORMAT_XRGB8888         0x34325258

/* CRTC vblank sequence clock uapi（include/uapi/drm/drm.h，Linux 4.12+）。 */
struct drm_crtc_get_sequence {
    uint32_t crtc_id; uint32_t active;
    uint64_t sequence; int64_t sequence_ns;
};
struct drm_crtc_queue_sequence {
    uint32_t crtc_id; uint32_t flags;
    uint64_t sequence; uint64_t user_data;
};
struct drm_event_crtc_sequence {
    struct drm_event base; int64_t user_data; int64_t tv_ns; uint64_t sequence;
};

#define DRM_CRTC_SEQUENCE_RELATIVE     0x00000001
#define DRM_CRTC_SEQUENCE_NEXT_ON_MISS 0x00000002
#define DRM_EVENT_VBLANK               0x01
#define DRM_EVENT_CRTC_SEQUENCE        0x03
#define _DRM_VBLANK_RELATIVE           0x0000001
#define _DRM_VBLANK_EVENT              0x4000000

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

    /* --- WAIT_VBLANK 查询不回退 ---
     * type=0（absolute，target=0）是纯查询：真实时钟下同一周期内两次
     * 查询返回相同序列号，跨周期则 +1，因此只要求不回退。 */
    union drm_wait_vblank wv1 = {0}, wv2 = {0};
    CHECK_RET(ioctl(fd, DRM_IOCTL_WAIT_VBLANK, &wv1), 0, "WAIT_VBLANK 1");
    CHECK_RET(ioctl(fd, DRM_IOCTL_WAIT_VBLANK, &wv2), 0, "WAIT_VBLANK 2");
    CHECK(wv2.reply.sequence >= wv1.reply.sequence, "vblank seq monotonic");
    CHECK(wv2.reply.sequence >= seq1, "vblank seq >= flip seq");

    /* --- CRTC_GET_SEQUENCE：active 报告 + 序列号推进率 --- */
    struct timespec ts1, ts2;
    struct drm_crtc_get_sequence gseq1 = { .crtc_id = crtc_ids[0] };
    CHECK_RET(ioctl(fd, DRM_IOCTL_CRTC_GET_SEQUENCE, &gseq1), 0,
              "CRTC_GET_SEQUENCE works");
    CHECK(gseq1.active == 1, "GET_SEQUENCE active == 1 after SETCRTC");
    CHECK(gseq1.sequence_ns > 0, "GET_SEQUENCE timestamp positive");
    CHECK(gseq1.sequence >= seq1, "GET_SEQUENCE sequence >= flip seq");

    usleep(120000); /* 约 7 个 vblank 周期 */
    struct drm_crtc_get_sequence gseq2 = { .crtc_id = crtc_ids[0] };
    CHECK_RET(ioctl(fd, DRM_IOCTL_CRTC_GET_SEQUENCE, &gseq2), 0,
              "CRTC_GET_SEQUENCE second query");
    uint64_t seq_delta = gseq2.sequence - gseq1.sequence;
    CHECK(seq_delta >= 5 && seq_delta <= 9,
          "sequence advances at ~60 Hz over 120 ms");
    CHECK(gseq2.sequence_ns > gseq1.sequence_ns, "sequence_ns monotonic");

    struct drm_crtc_get_sequence gseq_bad = { .crtc_id = 0xdeadbeef };
    CHECK_ERR(ioctl(fd, DRM_IOCTL_CRTC_GET_SEQUENCE, &gseq_bad), ENOENT,
              "GET_SEQUENCE rejects unknown crtc");

    /* --- CRTC_QUEUE_SEQUENCE：相对目标两个周期后投递事件 --- */
    struct drm_crtc_queue_sequence qseq = {
        .crtc_id = crtc_ids[0],
        .flags = DRM_CRTC_SEQUENCE_RELATIVE,
        .sequence = 2,
        .user_data = 0xfeedfacefeedfaceULL,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_CRTC_QUEUE_SEQUENCE, &qseq), 0,
              "QUEUE_SEQUENCE accepts relative target");
    struct pollfd qpfd = { .fd = fd, .events = POLLIN };
    pr = poll(&qpfd, 1, 1000);
    CHECK(pr == 1, "poll wakes for queued sequence event");
    struct drm_event_crtc_sequence seq_ev = {0};
    n = read(fd, &seq_ev, sizeof(seq_ev));
    CHECK(n == (ssize_t)sizeof(seq_ev), "read returns drm_event_crtc_sequence");
    CHECK(seq_ev.base.type == DRM_EVENT_CRTC_SEQUENCE,
          "event type == CRTC_SEQUENCE");
    CHECK(seq_ev.base.length == sizeof(seq_ev),
          "crtc_sequence event length == sizeof(struct)");
    CHECK((uint64_t)seq_ev.user_data == 0xfeedfacefeedfaceULL,
          "sequence event user_data round-trips");
    CHECK(seq_ev.sequence >= qseq.sequence,
          "sequence event fired at or after target");
    CHECK(seq_ev.tv_ns >= gseq2.sequence_ns, "sequence event timestamp monotonic");

    /* --- QUEUE_SEQUENCE 错误路径 --- */
    struct drm_crtc_queue_sequence bad_flags = {
        .crtc_id = crtc_ids[0], .flags = 0x80000000, .sequence = 1,
    };
    CHECK_ERR(ioctl(fd, DRM_IOCTL_CRTC_QUEUE_SEQUENCE, &bad_flags), EINVAL,
              "QUEUE_SEQUENCE rejects unknown flags");
    struct drm_crtc_queue_sequence bad_crtc = {
        .crtc_id = 0xdeadbeef, .flags = DRM_CRTC_SEQUENCE_RELATIVE, .sequence = 1,
    };
    CHECK_ERR(ioctl(fd, DRM_IOCTL_CRTC_QUEUE_SEQUENCE, &bad_crtc), ENOENT,
              "QUEUE_SEQUENCE rejects unknown crtc");

    /* --- WAIT_VBLANK _DRM_VBLANK_EVENT：入队而非阻塞 --- */
    union drm_wait_vblank wev = {0};
    wev.req.type = _DRM_VBLANK_EVENT | _DRM_VBLANK_RELATIVE;
    wev.req.sequence = 1;
    wev.req.signal = 0xcafef00dcafebabeULL;
    CHECK_RET(ioctl(fd, DRM_IOCTL_WAIT_VBLANK, &wev), 0,
              "WAIT_VBLANK EVENT returns immediately");
    struct pollfd wpfd = { .fd = fd, .events = POLLIN };
    pr = poll(&wpfd, 1, 1000);
    CHECK(pr == 1, "poll wakes for vblank event");
    struct drm_event_vblank vev = {0};
    n = read(fd, &vev, sizeof(vev));
    CHECK(n == (ssize_t)sizeof(vev), "read returns drm_event_vblank");
    CHECK(vev.base.type == DRM_EVENT_VBLANK, "event type == VBLANK");
    CHECK(vev.user_data == 0xcafef00dcafebabeULL,
          "vblank event user_data == request.signal");
    CHECK(vev.crtc_id == crtc_ids[0], "vblank event crtc_id matches");

    /* --- 相对阻塞等待按周期睡眠 --- */
    union drm_wait_vblank wblock = {0};
    wblock.req.type = _DRM_VBLANK_RELATIVE;
    wblock.req.sequence = 2;
    clock_gettime(CLOCK_MONOTONIC, &ts1);
    CHECK_RET(ioctl(fd, DRM_IOCTL_WAIT_VBLANK, &wblock), 0,
              "WAIT_VBLANK relative 2 blocks");
    clock_gettime(CLOCK_MONOTONIC, &ts2);
    long elapsed_ms = (ts2.tv_sec - ts1.tv_sec) * 1000
                      + (ts2.tv_nsec - ts1.tv_nsec) / 1000000;
    /* 相对等待语义与真实 DRM 一致：从当前时刻数 N 个边沿。若调用发生在
     * 边沿刚过后，第 N 个边沿不足 N 个整周期（最短 ≈(N-1) 周期），
     * 因此接受 [1, 3.5] 个周期的时间窗。 */
    CHECK(elapsed_ms >= 16 && elapsed_ms <= 60,
          "relative wait 2 spans 1-2 vblank periods");
    CHECK(wblock.reply.sequence > wev.reply.sequence,
          "blocking wait advanced the counter");

    /* --- 队列再次清空 --- */
    CHECK_ERR(read(fd, buf, sizeof(buf)), EAGAIN, "event queue drained");

    /* --- legacy GETCRTC readback matches the SETCRTC we ran above --- */
    uint32_t readback_conns[4] = {0};
    struct drm_mode_crtc getc = {
        .crtc_id = crtc_ids[0],
        .set_connectors_ptr = (uint64_t)(uintptr_t)readback_conns,
        .count_connectors = 4,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETCRTC, &getc), 0, "GETCRTC readback");
    CHECK(getc.fb_id == fb.fb_id, "GETCRTC fb_id matches SETCRTC");
    CHECK(getc.count_connectors == 1, "GETCRTC count_connectors == 1");
    CHECK(readback_conns[0] == conn_ids[0],
          "GETCRTC reports the connector we set");
    CHECK(getc.mode_valid == 1, "GETCRTC mode_valid == 1");
    CHECK(getc.mode.hdisplay == modes[0].hdisplay,
          "GETCRTC mode.hdisplay matches");

    /* --- SETCRTC with an unknown connector id must fail with EINVAL --- */
    uint32_t bogus_conn = 0xdeadbeef;
    struct drm_mode_crtc bad_conn = {
        .crtc_id = crtc_ids[0], .fb_id = fb.fb_id,
        .mode_valid = 1, .mode = modes[0],
        .set_connectors_ptr = (uint64_t)(uintptr_t)&bogus_conn,
        .count_connectors = 1,
    };
    CHECK_ERR(ioctl(fd, DRM_IOCTL_MODE_SETCRTC, &bad_conn), EINVAL,
              "SETCRTC rejects unknown connector");

    /* GETCRTC should still report the previous good binding. */
    memset(readback_conns, 0, sizeof(readback_conns));
    getc.count_connectors = 4;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETCRTC, &getc), 0,
              "GETCRTC after failed SETCRTC");
    CHECK(getc.fb_id == fb.fb_id,
          "GETCRTC fb_id unchanged after failed SETCRTC");

    /* --- SETCRTC referencing a removed fb must fail with EINVAL --- */
    uint32_t old_fb_id = fb.fb_id;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_RMFB, &old_fb_id), 0, "RMFB");
    /* The legacy binding pointed at this fb; GETCRTC must reflect the
     * unbinding so userspace doesn't keep seeing a dangling fb_id. */
    getc.count_connectors = 4;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETCRTC, &getc), 0,
              "GETCRTC after RMFB");
    CHECK(getc.fb_id == 0, "GETCRTC fb_id == 0 after RMFB clears binding");
    CHECK(getc.count_connectors == 0,
          "GETCRTC count_connectors == 0 after RMFB");

    /* CRTC 失活后 vblank 时钟应拒绝服务（Linux drm_vblank_get 失败 → EINVAL）。 */
    struct drm_crtc_get_sequence gseq_off = { .crtc_id = crtc_ids[0] };
    CHECK_ERR(ioctl(fd, DRM_IOCTL_CRTC_GET_SEQUENCE, &gseq_off), EINVAL,
              "GET_SEQUENCE rejects inactive CRTC");

    struct drm_mode_crtc bad_fb = {
        .crtc_id = crtc_ids[0], .fb_id = old_fb_id,
        .mode_valid = 1, .mode = modes[0],
        .set_connectors_ptr = (uint64_t)(uintptr_t)conn_ids,
        .count_connectors = 1,
    };
    CHECK_ERR(ioctl(fd, DRM_IOCTL_MODE_SETCRTC, &bad_fb), EINVAL,
              "SETCRTC rejects nonexistent fb_id");

    close(fd);
    TEST_DONE();
}
