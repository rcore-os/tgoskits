/*
 * test-drm-perbuf-dumb — 验证 /dev/dri/card0 的 per-buffer dumb 分配
 *
 * 覆盖：
 *   - 两次 CREATE_DUMB 各拿到独立 handle
 *   - 两次 MAP_DUMB 返回不同 offset
 *   - 两个 buffer 的 mmap 区域互不干扰（写一个不影响另一个）
 *   - 在 buf1 上画一个梯度
 *   - ADDFB2 两次拿到不同 fb_id
 *   - 两次 atomic commit 在 fb1 / fb2 之间翻页（双缓冲流程）
 *
 * 这条路径在 F+G+H+I 的"所有 dumb buffer 共享 scanout"基线下会失败：
 * 两个 mmap 指向同一段物理内存，写 buf2 会覆盖 buf1。L 引入的 per-buffer
 * GlobalPage 分配让两个 buffer 各自有独立物理页。
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <stdint.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

struct drm_mode_create_dumb {
    uint32_t height, width, bpp, flags;
    uint32_t handle, pitch;
    uint64_t size;
};
struct drm_mode_map_dumb {
    uint32_t handle, pad;
    uint64_t offset;
};
struct drm_mode_destroy_dumb {
    uint32_t handle;
};
struct drm_clip_rect {
    uint16_t x1, y1, x2, y2;
};
struct drm_mode_dirtyfb {
    uint32_t fb_id, flags, color, num_clips;
    uint64_t clips_ptr;
};
struct drm_mode_fb_cmd2 {
    uint32_t fb_id, width, height, pixel_format, flags;
    uint32_t handles[4], pitches[4], offsets[4];
    uint64_t modifier[4];
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
struct drm_mode_obj_get_properties {
    uint64_t props_ptr, prop_values_ptr;
    uint32_t count_props, obj_id, obj_type;
};
struct drm_mode_get_property {
    uint64_t values_ptr, enum_blob_ptr;
    uint32_t prop_id, flags;
    char name[32];
    uint32_t count_values, count_enum_blobs;
};
struct drm_mode_atomic {
    uint32_t flags, count_objs;
    uint64_t objs_ptr, count_props_ptr, props_ptr, prop_values_ptr;
    uint64_t reserved, user_data;
};
struct drm_mode_create_blob {
    uint64_t data;
    uint32_t length, blob_id;
};

#define DRM_IOCTL_MODE_GETRESOURCES      _IOWR('d', 0xA0, struct drm_mode_card_res)
#define DRM_IOCTL_MODE_GETCONNECTOR      _IOWR('d', 0xA7, struct drm_mode_get_connector)
#define DRM_IOCTL_MODE_GETPROPERTY       _IOWR('d', 0xAA, struct drm_mode_get_property)
#define DRM_IOCTL_MODE_DIRTYFB           _IOWR('d', 0xB1, struct drm_mode_dirtyfb)
#define DRM_IOCTL_MODE_CREATE_DUMB       _IOWR('d', 0xB2, struct drm_mode_create_dumb)
#define DRM_IOCTL_MODE_MAP_DUMB          _IOWR('d', 0xB3, struct drm_mode_map_dumb)
#define DRM_IOCTL_MODE_DESTROY_DUMB      _IOWR('d', 0xB4, struct drm_mode_destroy_dumb)
#define DRM_IOCTL_MODE_ADDFB2            _IOWR('d', 0xB8, struct drm_mode_fb_cmd2)
#define DRM_IOCTL_MODE_OBJ_GETPROPERTIES _IOWR('d', 0xB9, struct drm_mode_obj_get_properties)
#define DRM_IOCTL_MODE_ATOMIC            _IOWR('d', 0xBC, struct drm_mode_atomic)
#define DRM_IOCTL_MODE_CREATEPROPBLOB    _IOWR('d', 0xBD, struct drm_mode_create_blob)

#define DRM_MODE_OBJECT_CRTC      0xcccccccc
#define DRM_MODE_OBJECT_CONNECTOR 0xc0c0c0c0
#define DRM_MODE_OBJECT_PLANE     0xeeeeeeee
#define DRM_FORMAT_XRGB8888       0x34325258

static uint32_t find_prop(int fd, uint32_t obj, uint32_t ty, const char *name) {
    uint32_t ids[32] = {0};
    uint64_t vals[32] = {0};
    struct drm_mode_obj_get_properties q = {
        .obj_id = obj,
        .obj_type = ty,
        .count_props = 32,
        .props_ptr = (uint64_t)(uintptr_t)ids,
        .prop_values_ptr = (uint64_t)(uintptr_t)vals,
    };
    if (ioctl(fd, DRM_IOCTL_MODE_OBJ_GETPROPERTIES, &q) != 0) {
        return 0;
    }
    for (uint32_t i = 0; i < q.count_props; i++) {
        struct drm_mode_get_property p = { .prop_id = ids[i] };
        if (ioctl(fd, DRM_IOCTL_MODE_GETPROPERTY, &p) == 0
            && strcmp(p.name, name) == 0) {
            return ids[i];
        }
    }
    return 0;
}

int main(void) {
    TEST_START("test-drm-perbuf-dumb");

    int fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
    CHECK(fd >= 0, "open /dev/dri/card0");
    if (fd < 0) {
        TEST_DONE();
    }

    /* --- 枚举 connector / crtc，拿到屏幕分辨率 --- */
    uint32_t crtcs[4] = {0}, conns[4] = {0};
    struct drm_mode_card_res res = {
        .crtc_id_ptr = (uint64_t)(uintptr_t)crtcs,
        .connector_id_ptr = (uint64_t)(uintptr_t)conns,
        .count_crtcs = 4,
        .count_connectors = 4,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, &res), 0, "GETRESOURCES");
    CHECK(res.count_crtcs >= 1 && res.count_connectors >= 1, "at least one crtc + connector");
    uint32_t crtc = crtcs[0], conn = conns[0];

    struct drm_mode_mode_info mode = {0};
    struct drm_mode_get_connector c = {
        .connector_id = conn,
        .count_modes = 1,
        .modes_ptr = (uint64_t)(uintptr_t)&mode,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_GETCONNECTOR, &c), 0, "GETCONNECTOR");
    uint32_t w = mode.hdisplay, h = mode.vdisplay;
    CHECK(w > 0 && h > 0, "mode reports nonzero resolution");

    /* --- 两次 CREATE_DUMB，期望拿到独立 handle --- */
    struct drm_mode_create_dumb d1 = { .width = w, .height = h, .bpp = 32 };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_CREATE_DUMB, &d1), 0, "CREATE_DUMB d1");
    struct drm_mode_create_dumb d2 = { .width = w, .height = h, .bpp = 32 };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_CREATE_DUMB, &d2), 0, "CREATE_DUMB d2");
    CHECK(d1.handle != d2.handle, "distinct dumb handles");
    CHECK(d1.pitch == d2.pitch && d1.size == d2.size, "consistent pitch/size");

    /* --- 两次 MAP_DUMB，期望拿到不同的 offset --- */
    struct drm_mode_map_dumb m1 = { .handle = d1.handle };
    struct drm_mode_map_dumb m2 = { .handle = d2.handle };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_MAP_DUMB, &m1), 0, "MAP_DUMB m1");
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_MAP_DUMB, &m2), 0, "MAP_DUMB m2");
    CHECK(m1.offset != m2.offset, "distinct mmap offsets");

    /* --- 各自 mmap，验证物理隔离 --- */
    uint8_t *p1 = mmap(NULL, d1.size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, m1.offset);
    uint8_t *p2 = mmap(NULL, d2.size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, m2.offset);
    CHECK(p1 != MAP_FAILED, "mmap p1");
    CHECK(p2 != MAP_FAILED, "mmap p2");
    if (p1 == MAP_FAILED || p2 == MAP_FAILED) {
        close(fd);
        TEST_DONE();
    }

    memset(p1, 0xAA, d1.size);
    memset(p2, 0x55, d2.size);
    /* 关键断言：两个 buffer 真的是独立物理内存 */
    CHECK(p1[0] == 0xAA && p1[d1.size - 1] == 0xAA, "p1 untouched after p2 write");
    CHECK(p2[0] == 0x55 && p2[d2.size - 1] == 0x55, "p2 untouched after p1 write");

    /* 在 p1 上画一个梯度，验证用户态写穿透到内核 GlobalPage */
    uint32_t *px1 = (uint32_t *)p1;
    for (uint32_t y = 0; y < h; y++) {
        for (uint32_t x = 0; x < w; x++) {
            px1[y * (d1.pitch / 4) + x] =
                ((x * 255 / w) << 16) | ((y * 255 / h) << 8) | 0x80;
        }
    }
    CHECK(px1[0] == ((0 << 16) | (0 << 8) | 0x80), "gradient corner pixel");

    /* --- ADDFB2 两次，期望不同 fb_id --- */
    struct drm_mode_fb_cmd2 fb1 = {
        .width = w, .height = h, .pixel_format = DRM_FORMAT_XRGB8888,
        .handles = { d1.handle }, .pitches = { d1.pitch },
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_ADDFB2, &fb1), 0, "ADDFB2 fb1");
    struct drm_mode_fb_cmd2 fb2 = {
        .width = w, .height = h, .pixel_format = DRM_FORMAT_XRGB8888,
        .handles = { d2.handle }, .pitches = { d2.pitch },
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_ADDFB2, &fb2), 0, "ADDFB2 fb2");
    CHECK(fb1.fb_id != fb2.fb_id, "distinct fb_ids");

    /* --- 把 mode 包成 propblob，构造原子提交 --- */
    struct drm_mode_create_blob cb = {
        .data = (uint64_t)(uintptr_t)&mode,
        .length = sizeof(mode),
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_CREATEPROPBLOB, &cb), 0, "CREATEPROPBLOB");

    uint32_t plane = 0x40;  /* fixed primary plane id */
    uint32_t P_CRTC_MODE   = find_prop(fd, crtc, DRM_MODE_OBJECT_CRTC, "MODE_ID");
    uint32_t P_CRTC_ACTIVE = find_prop(fd, crtc, DRM_MODE_OBJECT_CRTC, "ACTIVE");
    uint32_t P_CONN_CRTC   = find_prop(fd, conn, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID");
    uint32_t P_PLANE_FB    = find_prop(fd, plane, DRM_MODE_OBJECT_PLANE, "FB_ID");
    uint32_t P_PLANE_CRTC  = find_prop(fd, plane, DRM_MODE_OBJECT_PLANE, "CRTC_ID");
    uint32_t P_SX = find_prop(fd, plane, DRM_MODE_OBJECT_PLANE, "SRC_X");
    uint32_t P_SY = find_prop(fd, plane, DRM_MODE_OBJECT_PLANE, "SRC_Y");
    uint32_t P_SW = find_prop(fd, plane, DRM_MODE_OBJECT_PLANE, "SRC_W");
    uint32_t P_SH = find_prop(fd, plane, DRM_MODE_OBJECT_PLANE, "SRC_H");
    uint32_t P_CX = find_prop(fd, plane, DRM_MODE_OBJECT_PLANE, "CRTC_X");
    uint32_t P_CY = find_prop(fd, plane, DRM_MODE_OBJECT_PLANE, "CRTC_Y");
    uint32_t P_CW = find_prop(fd, plane, DRM_MODE_OBJECT_PLANE, "CRTC_W");
    uint32_t P_CH = find_prop(fd, plane, DRM_MODE_OBJECT_PLANE, "CRTC_H");
    CHECK(P_CRTC_MODE && P_CRTC_ACTIVE && P_CONN_CRTC && P_PLANE_FB,
          "core props discoverable by name");

    uint32_t objs[3]   = { conn, crtc, plane };
    uint32_t counts[3] = { 1, 2, 10 };
    uint32_t props[13] = {
        P_CONN_CRTC,
        P_CRTC_ACTIVE, P_CRTC_MODE,
        P_PLANE_FB, P_PLANE_CRTC,
        P_SX, P_SY, P_SW, P_SH,
        P_CX, P_CY, P_CW, P_CH,
    };
    uint64_t values[13] = {
        crtc,
        1, cb.blob_id,
        fb1.fb_id, crtc,
        0, 0, (uint64_t)w << 16, (uint64_t)h << 16,
        0, 0, w, h,
    };
    struct drm_mode_atomic atom = {
        .count_objs = 3,
        .objs_ptr = (uint64_t)(uintptr_t)objs,
        .count_props_ptr = (uint64_t)(uintptr_t)counts,
        .props_ptr = (uint64_t)(uintptr_t)props,
        .prop_values_ptr = (uint64_t)(uintptr_t)values,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_ATOMIC, &atom), 0, "atomic commit fb1");

    struct drm_clip_rect clip = {
        .x1 = 0,
        .y1 = 0,
        .x2 = w < 32 ? (uint16_t)w : 32,
        .y2 = h < 32 ? (uint16_t)h : 32,
    };
    struct drm_mode_dirtyfb dirty = {
        .fb_id = fb1.fb_id,
        .num_clips = 1,
        .clips_ptr = (uint64_t)(uintptr_t)&clip,
    };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_DIRTYFB, &dirty), 0,
              "DIRTYFB accepts valid clipped framebuffer damage");

    /* 翻到 fb2 — 这是双缓冲翻页的核心路径 */
    values[3] = fb2.fb_id;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_ATOMIC, &atom), 0, "atomic commit fb2");

    /* DESTROY_DUMB 应该接受合法 handle */
    struct drm_mode_destroy_dumb dd1 = { .handle = d1.handle };
    struct drm_mode_destroy_dumb dd2 = { .handle = d2.handle };
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_DESTROY_DUMB, &dd1), 0, "DESTROY_DUMB d1");
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_DESTROY_DUMB, &dd2), 0, "DESTROY_DUMB d2");

    /* GEM lifecycle: DESTROY_DUMB drops only the handle. Existing mmaps
     * keep their backing alive via the per-mapping retainer, and any
     * `fb_id` built over the destroyed handle keeps its own backing
     * ref. Verify both: read+write the existing mmap, then re-flip
     * between fb1 / fb2 via atomic commit. */
    p1[0] = 0x5Au;
    p1[1] = 0xA5u;
    p2[0] = 0x3Cu;
    p2[1] = 0xC3u;
    CHECK(p1[0] == 0x5Au && p1[1] == 0xA5u,
          "existing mmap p1 readable after DESTROY_DUMB");
    CHECK(p2[0] == 0x3Cu && p2[1] == 0xC3u,
          "existing mmap p2 readable after DESTROY_DUMB");

    /* Re-flip fb1 then fb2 — fb backing must survive handle destroy. */
    values[3] = fb1.fb_id;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_ATOMIC, &atom), 0,
              "atomic commit fb1 after DESTROY_DUMB");
    values[3] = fb2.fb_id;
    CHECK_RET(ioctl(fd, DRM_IOCTL_MODE_ATOMIC, &atom), 0,
              "atomic commit fb2 after DESTROY_DUMB");

    munmap(p1, d1.size);
    munmap(p2, d2.size);
    close(fd);
    TEST_DONE();
}
