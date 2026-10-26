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
 *   - 非法用户数组必须 EFAULT，且不能发布部分状态
 *   - DESTROY_PROPBLOB 之后 GET_PROPBLOB 必须 ENOENT
 *
 * 这条路径是 weston / mutter atomic backend 的核心路径，缺一会卡住合成器。
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <sys/socket.h>
#include <unistd.h>

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
#define DRM_IOCTL_MODE_GETCRTC _IOWR('d', 0xA1, struct drm_mode_crtc)
#define DRM_IOCTL_MODE_SETCRTC _IOWR('d', 0xA2, struct drm_mode_crtc)
struct drm_mode_crtc_page_flip {
    uint32_t crtc_id; uint32_t fb_id; uint32_t flags; uint32_t reserved;
    uint64_t user_data;
};
#define DRM_IOCTL_MODE_PAGE_FLIP _IOWR('d', 0xB0, struct drm_mode_crtc_page_flip)
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
struct sync_fence_info {
    char obj_name[32], driver_name[32];
    int32_t status; uint32_t flags; uint64_t timestamp_ns;
};
struct sync_file_info {
    char name[32]; int32_t status; uint32_t flags, num_fences, pad;
    uint64_t sync_fence_info;
};
#define SYNC_IOC_FILE_INFO _IOWR('>', 4, struct sync_file_info)

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
#define DRM_MODE_OBJECT_FB          0xfbfbfbfb
#define DRM_MODE_OBJECT_CONNECTOR   0xc0c0c0c0
#define DRM_MODE_OBJECT_PLANE       0xeeeeeeee
#define DRM_PLANE_TYPE_PRIMARY      1
#define DRM_MODE_ATOMIC_TEST_ONLY   0x0100
#define DRM_MODE_ATOMIC_NONBLOCK    0x0200
#define DRM_MODE_ATOMIC_ALLOW_MODESET 0x0400
#define DRM_MODE_PAGE_FLIP_EVENT    0x01
#define DRM_MODE_PROP_SIGNED_RANGE  (1u << 7)
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
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, &res), 0,
              "GETRESOURCES probe");
    /* Counts are input capacities on each call, not just probe results. */
    res.count_fbs = 0;
    res.count_encoders = 0;
    res.count_crtcs = 1;
    res.count_connectors = 1;
    res.crtc_id_ptr = (uint64_t)(uintptr_t)crtcs;
    res.connector_id_ptr = (uint64_t)(uintptr_t)conns;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, &res), 0, "GETRESOURCES");

    uint32_t planes[8] = {0};
    struct drm_mode_get_plane_res pres = {0};
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETPLANERESOURCES, &pres), 0,
              "GETPLANERESOURCES probe");
    CHECK(pres.count_planes >= 1 && pres.count_planes <= 8,
          "plane resources include a bounded set");
    pres.plane_id_ptr = (uint64_t)(uintptr_t)planes;
    pres.count_planes = 8;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETPLANERESOURCES, &pres), 0,
              "GETPLANERESOURCES fetch");
    uint32_t primary_plane = 0;
    for (uint32_t i = 0; i < pres.count_planes && i < 8; i++) {
        uint32_t type_prop = find_prop(fd, planes[i], DRM_MODE_OBJECT_PLANE, "type");
        if (type_prop && obj_prop_value(fd, planes[i], DRM_MODE_OBJECT_PLANE,
                                        type_prop) == DRM_PLANE_TYPE_PRIMARY) {
            primary_plane = planes[i];
            break;
        }
    }
    CHECK(primary_plane != 0, "find primary plane by KMS type property");
    planes[0] = primary_plane;

    struct drm_mode_mode_info modes[64] = {0};
    struct drm_mode_get_connector conn = {0};
    conn.connector_id = conns[0];
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETCONNECTOR, &conn), 0,
              "GETCONNECTOR probe");
    CHECK(conn.count_modes > 0 && conn.count_modes <= 64,
          "connector reports a bounded mode list");
    conn.count_props = 0;
    conn.count_encoders = 0;
    conn.count_modes = 64;
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
    struct drm_mode_fb_cmd2 fb2 = fb;
    fb2.fb_id = 0;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_ADDFB2, &fb2), 0, "ADDFB2 second framebuffer");

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
    uint32_t P_PLANE_CRTC_H  = find_prop(fd, planes[0], DRM_MODE_OBJECT_PLANE, "CRTC_H");
    CHECK(P_CONN_CRTC_ID && P_CRTC_ACTIVE && P_CRTC_MODE_ID,
          "connector/crtc props found");
    CHECK(P_PLANE_FB_ID && P_PLANE_CRTC_ID,
          "plane FB_ID/CRTC_ID props found");
    CHECK(P_PLANE_SRC_X && P_PLANE_SRC_Y && P_PLANE_SRC_W && P_PLANE_SRC_H,
          "plane SRC_* props found");
    CHECK(P_PLANE_CRTC_X && P_PLANE_CRTC_Y && P_PLANE_CRTC_W &&
          P_PLANE_CRTC_H,
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

    /* OBJECT 属性必须返回 count_values==1 且 values[0] 为对象类型。
     * drm-rs / libdrm 在此直接索引 values[0]；返回空值列表会让
     * smithay/合成器在枚举属性时越界崩溃。 */
    {
        struct drm_mode_get_property gp = {0};
        uint64_t vals[2] = {0};
        gp.prop_id = P_CONN_CRTC_ID;
        gp.values_ptr = (uint64_t)(uintptr_t)vals;
        gp.count_values = 2;
        CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETPROPERTY, &gp), 0,
                  "GETPROPERTY connector.CRTC_ID");
        CHECK(gp.count_values == 1, "CRTC_ID exposes exactly one value");
        CHECK(vals[0] == DRM_MODE_OBJECT_CRTC,
              "CRTC_ID value type == DRM_MODE_OBJECT_CRTC");
    }
    {
        struct drm_mode_get_property gp = {0};
        uint64_t vals[2] = {0};
        gp.prop_id = P_PLANE_FB_ID;
        gp.values_ptr = (uint64_t)(uintptr_t)vals;
        gp.count_values = 2;
        CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETPROPERTY, &gp), 0,
                  "GETPROPERTY plane.FB_ID");
        CHECK(gp.count_values == 1, "FB_ID exposes exactly one value");
        CHECK(vals[0] == DRM_MODE_OBJECT_FB,
              "FB_ID value type == DRM_MODE_OBJECT_FB");
    }
    {
        struct drm_mode_get_property gp = {0};
        uint64_t vals[2] = {0};
        gp.prop_id = P_CRTC_MODE_ID;
        gp.values_ptr = (uint64_t)(uintptr_t)vals;
        gp.count_values = 2;
        CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETPROPERTY, &gp), 0,
                  "GETPROPERTY crtc.MODE_ID");
        CHECK(gp.count_values == 0, "BLOB metadata has no values");
        CHECK(gp.count_enum_blobs == 0, "BLOB metadata has no enum payload");
        CHECK(vals[0] == 0 && vals[1] == 0, "BLOB leaves value buffer untouched");
    }

    /* Linux distinguishes an existing framebuffer without a property
     * container (EINVAL) from an unknown framebuffer (ENOENT). */
    {
        uint32_t ids[8] = {0};
        uint64_t vals[8] = {0};
        struct drm_mode_obj_get_properties q = {0};
        q.obj_id = fb.fb_id;
        q.obj_type = DRM_MODE_OBJECT_FB;
        q.count_props = 8;
        q.props_ptr = (uint64_t)(uintptr_t)ids;
        q.prop_values_ptr = (uint64_t)(uintptr_t)vals;
        CHECK_ERR(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_OBJ_GETPROPERTIES, &q), EINVAL,
                  "framebuffer without property container returns EINVAL");
        q.obj_id = UINT32_MAX;
        CHECK_ERR(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_OBJ_GETPROPERTIES, &q), ENOENT,
                  "unknown framebuffer returns ENOENT");
    }

    /* PRIME 导出的 dma-buf 必须支持 lseek(SEEK_END) 返回缓冲大小。
     * Mesa/gbm 在 dmabuf 导入时用 lseek 探测大小，ESPIPE 会让
     * eglCreateImageKHR 以 BAD_ALLOC 失败（llvmpipe/swrast 路径）。 */
    {
        struct drm_prime_handle { uint32_t handle; uint32_t flags; int32_t fd; };
#define DRM_IOCTL_PRIME_HANDLE_TO_FD _IOWR('d', 0x2d, struct drm_prime_handle)
        struct drm_prime_handle ph = { .handle = cd.handle, .flags = 0x80000 };
        CHECK_RET(ioctl(fd, DRM_IOCTL_PRIME_HANDLE_TO_FD, &ph), 0,
                  "PRIME_HANDLE_TO_FD");
        CHECK(ph.fd >= 0, "PRIME export returns an fd");
        off_t sz = syscall(SYS_lseek, ph.fd, 0, SEEK_END);
        CHECK(sz == (off_t)cd.size, "dmabuf lseek SEEK_END == buffer size");
        CHECK_RET(syscall(SYS_lseek, ph.fd, 0, SEEK_SET), 0, "dmabuf SEEK_SET(0)");
        CHECK_ERR(syscall(SYS_lseek, ph.fd, 0, SEEK_CUR), EINVAL, "dmabuf rejects SEEK_CUR");
        CHECK_ERR(syscall(SYS_lseek, ph.fd, 1, SEEK_SET), EINVAL, "dmabuf rejects nonzero SET");
        CHECK_ERR(syscall(SYS_lseek, ph.fd, -1L, SEEK_END), EINVAL, "dmabuf rejects nonzero END");
        CHECK_RET(syscall(SYS_lseek, ph.fd, 0, SEEK_END), (long)cd.size,
                  "rejected seeks preserve size probe");
        close(ph.fd);
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
    uint32_t obj_count[3] = { 1, 2, 10 };
    uint32_t props[13] = {
        P_CONN_CRTC_ID,
        P_CRTC_ACTIVE, P_CRTC_MODE_ID,
        P_PLANE_FB_ID, P_PLANE_CRTC_ID,
        P_PLANE_SRC_X, P_PLANE_SRC_Y, P_PLANE_SRC_W, P_PLANE_SRC_H,
        P_PLANE_CRTC_X, P_PLANE_CRTC_Y, P_PLANE_CRTC_W, P_PLANE_CRTC_H,
    };
    uint64_t values[13] = {
        crtcs[0],
        1, cb.blob_id,
        fb.fb_id, crtcs[0],
        0, 0, (uint64_t)w << 16, (uint64_t)h << 16,
        0, 0, w, h,
    };

    struct drm_mode_atomic atom = {
        .flags = DRM_MODE_ATOMIC_TEST_ONLY | DRM_MODE_ATOMIC_ALLOW_MODESET,
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
    atom.flags = DRM_MODE_PAGE_FLIP_EVENT | DRM_MODE_ATOMIC_ALLOW_MODESET;
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


    /* GETPLANE（legacy 结构体）也必须回读 atomic 提交后的实时状态。
     * deniald 的初始扫描验证提交后用 GETPLANE 检查 primary plane 是否
     * 真的在扫描 framebuffer；硬编码 fb_id=0 会让合成器直接失败。 */
    {
        struct drm_mode_get_plane {
            uint32_t plane_id;
            uint32_t crtc_id; uint32_t fb_id; uint32_t crtcs_possible;
            uint32_t gamma_size; uint32_t count_format_types;
            uint64_t format_type_ptr;
        };
#define DRM_IOCTL_MODE_GETPLANE _IOWR('d', 0xB6, struct drm_mode_get_plane)
        uint32_t fmts[8] = {0};
        struct drm_mode_get_plane gp2 = {0};
        gp2.plane_id = planes[0];
        gp2.count_format_types = 8;
        gp2.format_type_ptr = (uint64_t)(uintptr_t)fmts;
        CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETPLANE, &gp2), 0,
                  "GETPLANE after atomic commit");
        CHECK(gp2.fb_id == fb.fb_id, "GETPLANE reports committed fb_id");
        CHECK(gp2.crtc_id == crtcs[0], "GETPLANE reports committed crtc_id");
        struct drm_mode_crtc crtc = { .crtc_id = crtcs[0] };
        CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_GETCRTC, &crtc), 0,
                  "GETCRTC after atomic commit");
        CHECK(crtc.fb_id == fb.fb_id && crtc.mode_valid == 1 &&
              crtc.mode.hdisplay == modes[0].hdisplay, "GETCRTC observes atomic binding and mode");
        struct drm_mode_crtc_page_flip flip = { .crtc_id = crtcs[0], .fb_id = fb2.fb_id,
                                                .flags = DRM_MODE_PAGE_FLIP_EVENT,
                                                .user_data = 0x1e9ac9ULL };
        CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_PAGE_FLIP, &flip), 0,
                  "legacy flip after atomic modeset");
        CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_GETPLANE, &gp2), 0, "GETPLANE after mixed flip");
        CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_GETCRTC, &crtc), 0, "GETCRTC after mixed flip");
        CHECK(gp2.fb_id == fb2.fb_id && crtc.fb_id == fb2.fb_id &&
              obj_prop_value(fd, planes[0], DRM_MODE_OBJECT_PLANE, P_PLANE_FB_ID) == fb2.fb_id,
              "legacy flip updates both queries and atomic FB_ID");
        struct drm_event_vblank mixed = {0};
        pfd.revents = 0;
        CHECK(poll(&pfd, 1, 2000) == 1 && (pfd.revents & POLLIN),
              "wait for the mixed legacy flip before submitting another");
        CHECK(read(fd, &mixed, sizeof(mixed)) == (ssize_t)sizeof(mixed)
              && mixed.user_data == flip.user_data && mixed.sequence > ev.sequence,
              "mixed flip completes on a later vblank");
        flip.fb_id = fb.fb_id;
        flip.flags = 0;
        CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_PAGE_FLIP, &flip), 0, "restore original framebuffer");
    }

    /* IN_FENCE_FD borrows a sync-file; it never owns the caller's fd.
     * Starry currently has no sync-file producer, so only -1 is valid. */
    uint32_t P_PLANE_IN_FENCE_FD = find_prop(fd, planes[0],
                                             DRM_MODE_OBJECT_PLANE,
                                             "IN_FENCE_FD");
    CHECK(P_PLANE_IN_FENCE_FD != 0, "plane advertises IN_FENCE_FD");
    {
        struct drm_mode_get_property g = {0};
        g.prop_id = P_PLANE_IN_FENCE_FD;
        uint64_t vals[2] = {0};
        g.count_values = 2;
        g.values_ptr = (uint64_t)(uintptr_t)vals;
        CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETPROPERTY, &g), 0,
                  "GETPROPERTY IN_FENCE_FD");
        CHECK(g.flags & DRM_MODE_PROP_SIGNED_RANGE,
              "IN_FENCE_FD is SIGNED_RANGE");
        CHECK(g.count_values == 2, "IN_FENCE_FD reports two range values");
        CHECK(vals[0] == (uint64_t)-1, "IN_FENCE_FD min == -1");
        CHECK(vals[1] == 2147483647ULL, "IN_FENCE_FD max == INT_MAX");
        CHECK(obj_prop_value(fd, planes[0], DRM_MODE_OBJECT_PLANE,
                             P_PLANE_IN_FENCE_FD) == (uint64_t)-1,
              "IN_FENCE_FD idle value == -1");
    }
    {
        int probe = open("/dev/zero", O_RDONLY | O_CLOEXEC);
        int sock = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
        int card = dup(fd);
        int closed = dup(fd);
        CHECK(probe >= 0 && sock >= 0 && card >= 0 && closed >= 0,
              "create non-sync-file descriptors");
        close(closed);
        CHECK_ERR(syscall(SYS_lseek, sock, 0L, SEEK_SET), ESPIPE,
                  "nonseekable socket retains ESPIPE");
        uint32_t t_objs[] = { planes[0] };
        uint32_t t_counts[] = { 2 };
        uint32_t t_props[] = { P_PLANE_FB_ID, P_PLANE_IN_FENCE_FD };
        uint64_t old_fb = obj_prop_value(fd, planes[0], DRM_MODE_OBJECT_PLANE, P_PLANE_FB_ID);
        uint64_t new_fb = old_fb == fb.fb_id ? fb2.fb_id : fb.fb_id;
        /* Both FBs have identical format/full-screen geometry; changing only FB_ID
         * isolates the IN_FENCE_FD sentinel without invalidating SIMPLE_KMS bounds. */
        uint64_t t_values[] = { new_fb, (uint64_t)-1 };
        struct drm_mode_atomic t = {
            .count_objs = 1,
            .objs_ptr = (uint64_t)(uintptr_t)t_objs,
            .count_props_ptr = (uint64_t)(uintptr_t)t_counts,
            .props_ptr = (uint64_t)(uintptr_t)t_props,
            .prop_values_ptr = (uint64_t)(uintptr_t)t_values,
        };
        const uint64_t invalid[] = { (uint64_t)-2, 2147483648ULL,
                                    (uint64_t)closed, (uint64_t)probe,
                                    (uint64_t)sock, (uint64_t)card };
        for (unsigned test_only = 0; test_only < 2; test_only++) {
            t.flags = test_only ? DRM_MODE_ATOMIC_TEST_ONLY : 0;
            for (unsigned i = 0; i < sizeof(invalid) / sizeof(invalid[0]); i++) {
                t_values[1] = invalid[i];
                CHECK_ERR(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_ATOMIC, &t), EINVAL,
                          "invalid fence rejected before atomic publication");
                CHECK(obj_prop_value(fd, planes[0], DRM_MODE_OBJECT_PLANE, P_PLANE_FB_ID) == old_fb,
                      "invalid fence leaves earlier properties unchanged");
                CHECK(fcntl(probe, F_GETFD) >= 0 && fcntl(sock, F_GETFD) >= 0 &&
                      fcntl(card, F_GETFD) >= 0, "invalid fence preserves caller descriptors");
            }
        }
        t_values[1] = (uint64_t)-1;
        CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_ATOMIC, &t), 0,
                  "TEST_ONLY FB_ID with no fence accepted");
        CHECK(obj_prop_value(fd, planes[0], DRM_MODE_OBJECT_PLANE, P_PLANE_FB_ID) == old_fb,
              "TEST_ONLY without fence preserves state");
        t.flags = 0;
        CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_ATOMIC, &t), 0,
                  "real FB_ID commit with no fence accepted");
        CHECK(obj_prop_value(fd, planes[0], DRM_MODE_OBJECT_PLANE, P_PLANE_FB_ID) == new_fb,
              "real no-fence commit publishes framebuffer");
        close(probe);
        close(sock);
        close(card);
    }

    /* Command completion must not masquerade as presentation. The controlled
     * edge test in ax-drm checks the pending interval; this userspace path
     * checks the installed sync_file and flip event share a vblank timestamp. */
    uint32_t out_property = find_prop(fd, crtcs[0], DRM_MODE_OBJECT_CRTC,
                                      "OUT_FENCE_PTR");
    CHECK(out_property != 0, "CRTC OUT_FENCE_PTR is exposed");
    int32_t out_fd = -1;
    uint32_t flip_objects[] = {crtcs[0], planes[0]};
    uint32_t flip_counts[] = {1, 1};
    uint32_t flip_properties[] = {out_property, P_PLANE_FB_ID};
    uint64_t current_fb = obj_prop_value(fd, planes[0], DRM_MODE_OBJECT_PLANE,
                                         P_PLANE_FB_ID);
    uint64_t flip_values[] = {(uint64_t)(uintptr_t)&out_fd,
                             current_fb == fb.fb_id ? fb2.fb_id : fb.fb_id};
    struct drm_mode_atomic flip = {
        .flags = DRM_MODE_ATOMIC_NONBLOCK | DRM_MODE_PAGE_FLIP_EVENT,
        .count_objs = 2,
        .objs_ptr = (uint64_t)(uintptr_t)flip_objects,
        .count_props_ptr = (uint64_t)(uintptr_t)flip_counts,
        .props_ptr = (uint64_t)(uintptr_t)flip_properties,
        .prop_values_ptr = (uint64_t)(uintptr_t)flip_values,
        .user_data = 0xF3CEULL,
    };
    CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_ATOMIC, &flip), 0,
              "NONBLOCK atomic flip exports an out-fence");
    CHECK(out_fd >= 0, "OUT_FENCE_PTR installs a real descriptor");
    struct pollfd fence_poll = {.fd = out_fd, .events = POLLIN};
    CHECK(poll(&fence_poll, 1, 2000) == 1 && (fence_poll.revents & POLLIN),
          "display out-fence becomes readable");
    pfd.revents = 0;
    CHECK(poll(&pfd, 1, 2000) == 1 && (pfd.revents & POLLIN),
          "out-fence completion also makes the flip event readable");
    struct drm_event_vblank presented = {0};
    CHECK(read(fd, &presented, sizeof(presented)) == (ssize_t)sizeof(presented),
          "read out-fence flip event");
    CHECK(presented.user_data == flip.user_data && presented.sequence > ev.sequence,
          "nonblocking flip is presented on a later vblank");
    struct sync_fence_info member = {0};
    struct sync_file_info sync = {.num_fences = 1,
                                  .sync_fence_info = (uint64_t)(uintptr_t)&member};
    CHECK_RET(syscall(SYS_ioctl, out_fd, SYNC_IOC_FILE_INFO, &sync), 0,
              "query display out-fence completion");
    CHECK(sync.status == 1 && member.status == 1, "out-fence reports success");
    CHECK(member.timestamp_ns / 1000 == (uint64_t)presented.tv_sec * 1000000
                                        + presented.tv_usec,
          "out-fence and flip event have the same presentation timestamp");
    close(out_fd);

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
    CHECK_ERR(ioctl(fd, DRM_IOCTL_MODE_ATOMIC, &bad), ENOENT,
              "unknown property rejected with ENOENT");
    CHECK(obj_prop_value(fd, crtcs[0], DRM_MODE_OBJECT_CRTC, P_CRTC_ACTIVE) == 1,
          "rejected commit left state intact");

    /* The shared Linux ioctl walks user arrays; it does not retain the old
     * private implementation's arbitrary 1024/8192 batch caps. Bad user
     * arrays must still fail transactionally, without allocating from an
     * untrusted aggregate count or publishing partial object state. */
    {
        struct drm_mode_atomic malformed = bad;
        malformed.flags = DRM_MODE_ATOMIC_TEST_ONLY;
        malformed.objs_ptr = 0;
        CHECK_ERR(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_ATOMIC, &malformed), EFAULT,
                  "invalid object array rejected with EFAULT");
        malformed.objs_ptr = (uint64_t)(uintptr_t)bad_objs;
        malformed.props_ptr = 0;
        CHECK_ERR(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_ATOMIC, &malformed), EFAULT,
                  "invalid property array rejected with EFAULT");
        CHECK(obj_prop_value(fd, crtcs[0], DRM_MODE_OBJECT_CRTC, P_CRTC_ACTIVE) == 1,
              "invalid user arrays leave CRTC state intact");
    }

    /* --- destroy-after-commit blob lifecycle ---
     * Linux DRM keeps a kernel reference on a `MODE_ID` blob from the
     * CRTC state that committed it. A user `DESTROYPROPBLOB` only
     * releases the user-publish handle; `GETPROPBLOB` on the same id
     * keeps succeeding until a later atomic commit replaces the
     * `MODE_ID` (or clears it). */
    struct drm_mode_destroy_blob db = { .blob_id = cb.blob_id };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_DESTROYPROPBLOB, &db), 0,
              "DESTROYPROPBLOB releases user publish");
    /* OBJ_GETPROPERTIES still reports the committed MODE_ID. */
    CHECK(obj_prop_value(fd, crtcs[0], DRM_MODE_OBJECT_CRTC, P_CRTC_MODE_ID)
              == cb.blob_id,
          "MODE_ID survives DESTROYPROPBLOB while CRTC state references it");
    /* GETPROPBLOB on the still-referenced id keeps working. */
    struct drm_mode_get_blob gb_after = {
        .blob_id = cb.blob_id, .length = sizeof(struct drm_mode_mode_info),
    };
    struct drm_mode_mode_info readback = {0};
    gb_after.data = (uint64_t)(uintptr_t)&readback;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETPROPBLOB, &gb_after), 0,
              "GETPROPBLOB on destroyed-but-committed blob succeeds");
    CHECK(gb_after.length == sizeof(readback),
          "post-destroy GETPROPBLOB returns the full mode payload");

    /* A blob that was never referenced by committed state should
     * disappear on DESTROYPROPBLOB. Build a second blob, do not commit
     * it, then destroy and confirm ENOENT. */
    struct drm_mode_create_blob cb_orphan = {
        .data = (uint64_t)(uintptr_t)&modes[0],
        .length = sizeof(modes[0]),
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_CREATEPROPBLOB, &cb_orphan), 0,
              "CREATEPROPBLOB (orphan)");
    struct drm_mode_destroy_blob db_orphan = { .blob_id = cb_orphan.blob_id };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_DESTROYPROPBLOB, &db_orphan), 0,
              "DESTROYPROPBLOB (orphan)");
    struct drm_mode_get_blob gb_orphan = {
        .blob_id = cb_orphan.blob_id, .length = sizeof(struct drm_mode_mode_info),
        .data = (uint64_t)(uintptr_t)&readback,
    };
    CHECK_ERR(ioctl(fd, DRM_IOCTL_MODE_GETPROPBLOB, &gb_orphan), ENOENT,
              "orphan blob is ENOENT after destroy");

    struct drm_mode_crtc disable = { .crtc_id = crtcs[0] };
    CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_SETCRTC, &disable), 0,
              "legacy disable after atomic modeset");
    CHECK(obj_prop_value(fd, planes[0], DRM_MODE_OBJECT_PLANE, P_PLANE_FB_ID) == 0 &&
          obj_prop_value(fd, planes[0], DRM_MODE_OBJECT_PLANE, P_PLANE_CRTC_ID) == 0 &&
          obj_prop_value(fd, crtcs[0], DRM_MODE_OBJECT_CRTC, P_CRTC_ACTIVE) == 0 &&
          obj_prop_value(fd, crtcs[0], DRM_MODE_OBJECT_CRTC, P_CRTC_MODE_ID) == 0,
          "legacy disable clears shared atomic bindings and mode");
    CHECK_ERR(ioctl(fd, DRM_IOCTL_MODE_GETPROPBLOB, &gb_after), ENOENT,
              "disable releases the last committed mode blob reference");

    close(fd);
    TEST_DONE();
}
