/*
 * test-drm-atomic — 原子 KMS 提交端到端测试
 *
 * 覆盖：
 *   - SET_CLIENT_CAP UNIVERSAL_PLANES + ATOMIC
 *   - 枚举 connector / crtc / plane
 *   - 用 OBJ_GETPROPERTIES + GETPROPERTY 通过名字找属性 ID
 *   - CREATE_PROPBLOB / GET_PROPBLOB 字节级回环
 *   - DRM_MODE_ATOMIC_TEST_ONLY 不应改状态
 *   - 真正一次原子提交（带 PAGE_FLIP_EVENT），应改状态、并产生 vblank 事件
 *   - 故意构造无效 prop id 的提交必须 EINVAL 拒绝且不动状态
 *   - DESTROY_PROPBLOB 之后 GET_PROPBLOB 必须 ENOENT
 *
 * 这条路径是 weston / mutter atomic backend 的核心路径，缺一会卡住合成器。
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <sys/ioctl.h>
#include <unistd.h>

struct drm_mode_mode_info {
    uint32_t clock;
    uint16_t hdisplay, hsync_start, hsync_end, htotal, hskew;
    uint16_t vdisplay, vsync_start, vsync_end, vtotal, vscan;
    uint32_t vrefresh, flags, kind; char name[32];
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
struct drm_mode_get_plane_res { uint64_t plane_id_ptr; uint32_t count_planes; };
struct drm_mode_obj_get_properties {
    uint64_t props_ptr; uint64_t prop_values_ptr;
    uint32_t count_props; uint32_t obj_id; uint32_t obj_type;
};
struct drm_mode_get_property {
    uint64_t values_ptr; uint64_t enum_blob_ptr;
    uint32_t prop_id; uint32_t flags; char name[32];
    uint32_t count_values; uint32_t count_enum_blobs;
};
struct drm_mode_create_dumb {
    uint32_t height; uint32_t width; uint32_t bpp; uint32_t flags;
    uint32_t handle; uint32_t pitch; uint64_t size;
};
struct drm_mode_fb_cmd2 {
    uint32_t fb_id; uint32_t width; uint32_t height; uint32_t pixel_format;
    uint32_t flags; uint32_t handles[4]; uint32_t pitches[4];
    uint32_t offsets[4]; uint64_t modifier[4];
};
struct drm_set_client_cap { uint64_t capability; uint64_t value; };
struct drm_mode_atomic {
    uint32_t flags; uint32_t count_objs;
    uint64_t objs_ptr; uint64_t count_props_ptr;
    uint64_t props_ptr; uint64_t prop_values_ptr;
    uint64_t reserved; uint64_t user_data;
};
struct drm_mode_create_blob { uint64_t data; uint32_t length; uint32_t blob_id; };
struct drm_mode_destroy_blob { uint32_t blob_id; };
struct drm_mode_get_blob { uint32_t blob_id; uint32_t length; uint64_t data; };
struct drm_event { uint32_t type; uint32_t length; };
struct drm_event_vblank {
    struct drm_event base;
    uint64_t user_data; uint32_t tv_sec; uint32_t tv_usec;
    uint32_t sequence; uint32_t crtc_id;
};

#define DRM_IOCTL_MODE_GETRESOURCES       _IOWR('d', 0xA0, struct drm_mode_card_res)
#define DRM_IOCTL_MODE_GETCONNECTOR       _IOWR('d', 0xA7, struct drm_mode_get_connector)
#define DRM_IOCTL_MODE_GETPROPERTY        _IOWR('d', 0xAA, struct drm_mode_get_property)
#define DRM_IOCTL_MODE_GETPROPBLOB        _IOWR('d', 0xAC, struct drm_mode_get_blob)
#define DRM_IOCTL_MODE_CREATE_DUMB        _IOWR('d', 0xB2, struct drm_mode_create_dumb)
#define DRM_IOCTL_MODE_GETPLANERESOURCES  _IOWR('d', 0xB5, struct drm_mode_get_plane_res)
#define DRM_IOCTL_MODE_ADDFB2             _IOWR('d', 0xB8, struct drm_mode_fb_cmd2)
#define DRM_IOCTL_MODE_OBJ_GETPROPERTIES  _IOWR('d', 0xB9, struct drm_mode_obj_get_properties)
#define DRM_IOCTL_MODE_ATOMIC             _IOWR('d', 0xBC, struct drm_mode_atomic)
#define DRM_IOCTL_MODE_CREATEPROPBLOB     _IOWR('d', 0xBD, struct drm_mode_create_blob)
#define DRM_IOCTL_MODE_DESTROYPROPBLOB    _IOWR('d', 0xBE, struct drm_mode_destroy_blob)
#define DRM_IOCTL_SET_CLIENT_CAP          _IOW('d', 0x0d, struct drm_set_client_cap)

#define DRM_MODE_OBJECT_CRTC        0xcccccccc
#define DRM_MODE_OBJECT_CONNECTOR   0xc0c0c0c0
#define DRM_MODE_OBJECT_PLANE       0xeeeeeeee
#define DRM_MODE_ATOMIC_TEST_ONLY   0x0100
#define DRM_MODE_PAGE_FLIP_EVENT    0x01
#define DRM_CLIENT_CAP_UNIVERSAL_PLANES 2
#define DRM_CLIENT_CAP_ATOMIC       3
#define DRM_FORMAT_XRGB8888         0x34325258
#define DRM_EVENT_FLIP_COMPLETE     0x02

/* 在对象上按名字找属性 id。找不到返回 0。 */
static uint32_t find_prop(int fd, uint32_t obj_id, uint32_t obj_type,
                          const char *name)
{
    uint32_t ids[64] = {0};
    uint64_t vals[64] = {0};
    struct drm_mode_obj_get_properties q = {0};
    q.obj_id = obj_id;
    q.obj_type = obj_type;
    q.count_props = 64;
    q.props_ptr = (uint64_t)(uintptr_t)ids;
    q.prop_values_ptr = (uint64_t)(uintptr_t)vals;
    if (ioctl(fd, DRM_IOCTL_MODE_OBJ_GETPROPERTIES, &q) != 0)
        return 0;
    for (uint32_t i = 0; i < q.count_props; i++) {
        struct drm_mode_get_property p = {0};
        p.prop_id = ids[i];
        if (ioctl(fd, DRM_IOCTL_MODE_GETPROPERTY, &p) == 0
            && strcmp(p.name, name) == 0)
            return ids[i];
    }
    return 0;
}

static uint64_t obj_prop_value(int fd, uint32_t obj_id, uint32_t obj_type,
                               uint32_t prop_id)
{
    uint32_t ids[64] = {0};
    uint64_t vals[64] = {0};
    struct drm_mode_obj_get_properties q = {0};
    q.obj_id = obj_id;
    q.obj_type = obj_type;
    q.count_props = 64;
    q.props_ptr = (uint64_t)(uintptr_t)ids;
    q.prop_values_ptr = (uint64_t)(uintptr_t)vals;
    if (ioctl(fd, DRM_IOCTL_MODE_OBJ_GETPROPERTIES, &q) != 0)
        return ~0ULL;
    for (uint32_t i = 0; i < q.count_props; i++)
        if (ids[i] == prop_id) return vals[i];
    return ~0ULL;
}

int main(void)
{
    TEST_START("drm-atomic");

    int fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC | O_NONBLOCK);
    CHECK(fd >= 0, "open /dev/dri/card0");
    if (fd < 0) {
        TEST_DONE();
    }

    struct drm_set_client_cap up = {
        .capability = DRM_CLIENT_CAP_UNIVERSAL_PLANES, .value = 1,
    };
    struct drm_set_client_cap at = {
        .capability = DRM_CLIENT_CAP_ATOMIC, .value = 1,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_SET_CLIENT_CAP, &up), 0,
              "SET_CLIENT_CAP UNIVERSAL_PLANES");
    CHECK_RET(ioctl(fd, DRM_IOCTL_SET_CLIENT_CAP, &at), 0,
              "SET_CLIENT_CAP ATOMIC");

    /* Enumerate objects. */
    uint32_t crtcs[1] = {0}, conns[1] = {0};
    struct drm_mode_card_res res = {0};
    (void)ioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, &res);
    res.crtc_id_ptr = (uint64_t)(uintptr_t)crtcs;
    res.connector_id_ptr = (uint64_t)(uintptr_t)conns;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, &res), 0, "GETRESOURCES");

    uint32_t planes[1] = {0};
    struct drm_mode_get_plane_res pres = {0};
    pres.plane_id_ptr = (uint64_t)(uintptr_t)planes;
    pres.count_planes = 1;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETPLANERESOURCES, &pres), 0,
              "GETPLANERESOURCES");

    struct drm_mode_mode_info modes[1] = {0};
    struct drm_mode_get_connector conn = {0};
    conn.connector_id = conns[0];
    conn.count_modes = 1;
    conn.modes_ptr = (uint64_t)(uintptr_t)modes;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETCONNECTOR, &conn), 0, "GETCONNECTOR");
    uint32_t w = modes[0].hdisplay, h = modes[0].vdisplay;
    CHECK(w > 0 && h > 0, "connector reports nonzero mode");

    /* Prepare a dumb fb to hand to the commit. */
    struct drm_mode_create_dumb cd = { .width = w, .height = h, .bpp = 32 };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_CREATE_DUMB, &cd), 0, "CREATE_DUMB");
    struct drm_mode_fb_cmd2 fb = {
        .width = w, .height = h, .pixel_format = DRM_FORMAT_XRGB8888,
        .handles = { cd.handle, 0, 0, 0 },
        .pitches = { cd.pitch, 0, 0, 0 },
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_ADDFB2, &fb), 0, "ADDFB2");

    /* --- 按名字找属性 id --- */
    uint32_t P_CONN_CRTC_ID  = find_prop(fd, conns[0], DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID");
    uint32_t P_CRTC_ACTIVE   = find_prop(fd, crtcs[0], DRM_MODE_OBJECT_CRTC, "ACTIVE");
    uint32_t P_CRTC_MODE_ID  = find_prop(fd, crtcs[0], DRM_MODE_OBJECT_CRTC, "MODE_ID");
    uint32_t P_PLANE_FB_ID   = find_prop(fd, planes[0], DRM_MODE_OBJECT_PLANE, "FB_ID");
    uint32_t P_PLANE_CRTC_ID = find_prop(fd, planes[0], DRM_MODE_OBJECT_PLANE, "CRTC_ID");
    uint32_t P_PLANE_SRC_X   = find_prop(fd, planes[0], DRM_MODE_OBJECT_PLANE, "SRC_X");
    uint32_t P_PLANE_SRC_Y   = find_prop(fd, planes[0], DRM_MODE_OBJECT_PLANE, "SRC_Y");
    uint32_t P_PLANE_SRC_W   = find_prop(fd, planes[0], DRM_MODE_OBJECT_PLANE, "SRC_W");
    uint32_t P_PLANE_SRC_H   = find_prop(fd, planes[0], DRM_MODE_OBJECT_PLANE, "SRC_H");
    uint32_t P_PLANE_CRTC_X  = find_prop(fd, planes[0], DRM_MODE_OBJECT_PLANE, "CRTC_X");
    uint32_t P_PLANE_CRTC_Y  = find_prop(fd, planes[0], DRM_MODE_OBJECT_PLANE, "CRTC_Y");
    uint32_t P_PLANE_CRTC_W  = find_prop(fd, planes[0], DRM_MODE_OBJECT_PLANE, "CRTC_W");
    CHECK(P_CONN_CRTC_ID && P_CRTC_ACTIVE && P_CRTC_MODE_ID,
          "connector/crtc props found");
    CHECK(P_PLANE_FB_ID && P_PLANE_CRTC_ID,
          "plane FB_ID/CRTC_ID props found");
    CHECK(P_PLANE_SRC_X && P_PLANE_SRC_Y && P_PLANE_SRC_W && P_PLANE_SRC_H,
          "plane SRC_* props found");
    CHECK(P_PLANE_CRTC_X && P_PLANE_CRTC_Y && P_PLANE_CRTC_W,
          "plane CRTC_* props found");

    /* CRTC.ACTIVE 必须是 RANGE [0, 1]，weston 会拒绝其他形态。 */
    {
        struct drm_mode_get_property gp = {0};
        gp.prop_id = P_CRTC_ACTIVE;
        uint64_t vals[2] = {0};
        gp.values_ptr = (uint64_t)(uintptr_t)vals;
        gp.count_values = 2;
        CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETPROPERTY, &gp), 0,
                  "GETPROPERTY CRTC.ACTIVE");
        /* DRM_MODE_PROP_RANGE = (1 << 1)。 */
        CHECK((gp.flags & (1u << 1)) != 0, "CRTC.ACTIVE is RANGE");
        CHECK(gp.count_values == 2, "CRTC.ACTIVE has 2 range bounds");
        CHECK(vals[0] == 0 && vals[1] == 1, "CRTC.ACTIVE range == [0, 1]");
    }

    /* --- blob round-trip --- */
    struct drm_mode_create_blob cb = {
        .data = (uint64_t)(uintptr_t)&modes[0],
        .length = sizeof(modes[0]),
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_CREATEPROPBLOB, &cb), 0,
              "CREATEPROPBLOB");
    CHECK(cb.blob_id != 0, "blob id nonzero");

    struct drm_mode_mode_info modes_back;
    memset(&modes_back, 0xa5, sizeof(modes_back));
    struct drm_mode_get_blob gb = {
        .blob_id = cb.blob_id,
        .length = sizeof(modes_back),
        .data = (uint64_t)(uintptr_t)&modes_back,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETPROPBLOB, &gb), 0, "GETPROPBLOB");
    CHECK(gb.length == sizeof(modes[0]), "blob length round-trips");
    CHECK(memcmp(&modes_back, &modes[0], sizeof(modes[0])) == 0,
          "blob bytes round-trip");

    /* --- 构造原子提交 --- */
    uint32_t obj_ids[3] = { conns[0], crtcs[0], planes[0] };
    uint32_t obj_count[3] = { 1, 2, 9 };
    uint32_t props[12] = {
        P_CONN_CRTC_ID,
        P_CRTC_ACTIVE, P_CRTC_MODE_ID,
        P_PLANE_FB_ID, P_PLANE_CRTC_ID,
        P_PLANE_SRC_X, P_PLANE_SRC_Y, P_PLANE_SRC_W, P_PLANE_SRC_H,
        P_PLANE_CRTC_X, P_PLANE_CRTC_Y, P_PLANE_CRTC_W,
    };
    uint64_t values[12] = {
        crtcs[0],
        1, cb.blob_id,
        fb.fb_id, crtcs[0],
        0, 0, (uint64_t)w << 16, (uint64_t)h << 16,
        0, 0, w,
    };

    struct drm_mode_atomic atom = {
        .flags = DRM_MODE_ATOMIC_TEST_ONLY,
        .count_objs = 3,
        .objs_ptr = (uint64_t)(uintptr_t)obj_ids,
        .count_props_ptr = (uint64_t)(uintptr_t)obj_count,
        .props_ptr = (uint64_t)(uintptr_t)props,
        .prop_values_ptr = (uint64_t)(uintptr_t)values,
        .user_data = 0xA70BADBADBADC0DEULL,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_ATOMIC, &atom), 0,
              "ATOMIC TEST_ONLY accepted");

    /* TEST_ONLY 不能改状态。 */
    CHECK(obj_prop_value(fd, crtcs[0], DRM_MODE_OBJECT_CRTC, P_CRTC_ACTIVE) == 0,
          "TEST_ONLY left ACTIVE == 0");

    /* 真正一次提交，带 page-flip event。 */
    atom.flags = DRM_MODE_PAGE_FLIP_EVENT;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_ATOMIC, &atom), 0, "ATOMIC commit");

    /* 状态应反映 commit。 */
    CHECK(obj_prop_value(fd, crtcs[0], DRM_MODE_OBJECT_CRTC, P_CRTC_ACTIVE) == 1,
          "commit set CRTC.ACTIVE == 1");
    CHECK(obj_prop_value(fd, crtcs[0], DRM_MODE_OBJECT_CRTC, P_CRTC_MODE_ID)
              == cb.blob_id,
          "commit set CRTC.MODE_ID == blob");
    CHECK(obj_prop_value(fd, planes[0], DRM_MODE_OBJECT_PLANE, P_PLANE_FB_ID)
              == fb.fb_id,
          "commit set plane.FB_ID");
    CHECK(obj_prop_value(fd, conns[0], DRM_MODE_OBJECT_CONNECTOR, P_CONN_CRTC_ID)
              == crtcs[0],
          "commit set connector.CRTC_ID");

    /* page flip event 应可读。 */
    struct pollfd pfd = { .fd = fd, .events = POLLIN };
    CHECK(poll(&pfd, 1, 2000) == 1, "poll POLLIN after atomic");
    struct drm_event_vblank ev;
    ssize_t n = read(fd, &ev, sizeof(ev));
    CHECK(n == (ssize_t)sizeof(ev), "read flip event size");
    CHECK(ev.base.type == DRM_EVENT_FLIP_COMPLETE, "event is FLIP_COMPLETE");
    CHECK(ev.user_data == 0xA70BADBADBADC0DEULL, "event user_data round-trip");

    /* --- 无效 prop id 必须 EINVAL，且不动状态 --- */
    uint32_t bad_props[1] = { 0xDEADBEEF };
    uint64_t bad_values[1] = { 42 };
    uint32_t bad_objs[1] = { planes[0] };
    uint32_t bad_counts[1] = { 1 };
    struct drm_mode_atomic bad = {
        .flags = 0,
        .count_objs = 1,
        .objs_ptr = (uint64_t)(uintptr_t)bad_objs,
        .count_props_ptr = (uint64_t)(uintptr_t)bad_counts,
        .props_ptr = (uint64_t)(uintptr_t)bad_props,
        .prop_values_ptr = (uint64_t)(uintptr_t)bad_values,
    };
    CHECK_ERR(ioctl(fd, DRM_IOCTL_MODE_ATOMIC, &bad), EINVAL,
              "bad prop id rejected with EINVAL");
    CHECK(obj_prop_value(fd, crtcs[0], DRM_MODE_OBJECT_CRTC, P_CRTC_ACTIVE) == 1,
          "rejected commit left state intact");

    /* --- destroy blob，再读应该 ENOENT --- */
    struct drm_mode_destroy_blob db = { .blob_id = cb.blob_id };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_DESTROYPROPBLOB, &db), 0,
              "DESTROYPROPBLOB");
    CHECK_ERR(ioctl(fd, DRM_IOCTL_MODE_GETPROPBLOB, &gb), ENOENT,
              "destroyed blob is ENOENT");

    close(fd);
    TEST_DONE();
}
