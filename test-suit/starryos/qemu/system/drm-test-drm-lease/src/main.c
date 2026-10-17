/*
 * DRM lease: a lessee gets its own master file description that only sees the
 * leased CRTC, connector and plane. Revoking empties the lease and hides it
 * from LIST_LESSEES; closing the lessee file ends it.
 */
#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <stdint.h>
#include <sys/ioctl.h>
#include <unistd.h>

struct drm_mode_card_res {
    uint64_t fb_id_ptr; uint64_t crtc_id_ptr; uint64_t connector_id_ptr;
    uint64_t encoder_id_ptr;
    uint32_t count_fbs; uint32_t count_crtcs;
    uint32_t count_connectors; uint32_t count_encoders;
    uint32_t min_width, max_width, min_height, max_height;
};
struct drm_mode_get_plane_res {
    uint64_t plane_id_ptr; uint32_t count_planes;
};
struct drm_set_client_cap {
    uint64_t capability;
    uint64_t value;
};
struct drm_mode_create_lease {
    uint64_t object_ids; uint32_t object_count; uint32_t flags;
    uint32_t lessee_id; uint32_t fd;
};
struct drm_mode_list_lessees {
    uint32_t count_lessees; uint32_t pad; uint64_t lessees_ptr;
};
struct drm_mode_get_lease {
    uint32_t count_objects; uint32_t pad; uint64_t objects_ptr;
};
struct drm_mode_revoke_lease {
    uint32_t lessee_id;
};

#define DRM_CLIENT_CAP_UNIVERSAL_PLANES  2
#define DRM_IOCTL_SET_CLIENT_CAP         _IOW('d', 0x0d, struct drm_set_client_cap)
#define DRM_IOCTL_MODE_GETRESOURCES      _IOWR('d', 0xA0, struct drm_mode_card_res)
#define DRM_IOCTL_MODE_GETPLANERESOURCES _IOWR('d', 0xB5, struct drm_mode_get_plane_res)
#define DRM_IOCTL_MODE_CREATE_LEASE      _IOWR('d', 0xC6, struct drm_mode_create_lease)
#define DRM_IOCTL_MODE_LIST_LESSEES      _IOWR('d', 0xC7, struct drm_mode_list_lessees)
#define DRM_IOCTL_MODE_GET_LEASE         _IOWR('d', 0xC8, struct drm_mode_get_lease)
#define DRM_IOCTL_MODE_REVOKE_LEASE      _IOWR('d', 0xC9, struct drm_mode_revoke_lease)

static int create_lease(int fd, uint32_t *objects, uint32_t count, uint32_t flags,
                        struct drm_mode_create_lease *out)
{
    memset(out, 0, sizeof(*out));
    out->object_ids = (uint64_t)(uintptr_t)objects;
    out->object_count = count;
    out->flags = flags;
    return ioctl(fd, DRM_IOCTL_MODE_CREATE_LEASE, out);
}

static uint32_t count_lessees(int fd, uint32_t *first)
{
    uint32_t ids[4] = {0};
    struct drm_mode_list_lessees list = {
        .count_lessees = 4,
        .lessees_ptr = (uint64_t)(uintptr_t)ids,
    };
    if (ioctl(fd, DRM_IOCTL_MODE_LIST_LESSEES, &list) != 0) {
        return UINT32_MAX;
    }
    if (first) {
        *first = ids[0];
    }
    return list.count_lessees;
}

static int contains(const uint32_t *ids, uint32_t count, uint32_t id)
{
    for (uint32_t i = 0; i < count; i++) {
        if (ids[i] == id) {
            return 1;
        }
    }
    return 0;
}

int main(void)
{
    TEST_START("drm-lease");

    int master = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
    CHECK(master >= 0, "open /dev/dri/card0 as the lessor");
    if (master < 0) {
        TEST_DONE();
    }
    struct drm_set_client_cap cap = {
        .capability = DRM_CLIENT_CAP_UNIVERSAL_PLANES,
        .value = 1,
    };
    CHECK_RET(ioctl(master, DRM_IOCTL_SET_CLIENT_CAP, &cap), 0,
              "lessor enables universal planes so planes can be leased");

    uint32_t crtcs[4] = {0}, connectors[4] = {0};
    struct drm_mode_card_res res = {
        .crtc_id_ptr = (uint64_t)(uintptr_t)crtcs,
        .connector_id_ptr = (uint64_t)(uintptr_t)connectors,
        .count_crtcs = 4,
        .count_connectors = 4,
    };
    CHECK_RET(ioctl(master, DRM_IOCTL_MODE_GETRESOURCES, &res), 0,
              "lessor GETRESOURCES");
    uint32_t planes[4] = {0};
    struct drm_mode_get_plane_res pres = {
        .plane_id_ptr = (uint64_t)(uintptr_t)planes,
        .count_planes = 4,
    };
    CHECK_RET(ioctl(master, DRM_IOCTL_MODE_GETPLANERESOURCES, &pres), 0,
              "lessor GETPLANERESOURCES");
    CHECK(res.count_crtcs >= 1 && res.count_connectors >= 1 && pres.count_planes >= 1,
          "device exposes a CRTC, a connector and a plane to lease");

    uint32_t objects[3] = { crtcs[0], connectors[0], planes[0] };
    struct drm_mode_create_lease cl;
    CHECK_ERR(create_lease(master, objects, 3, O_APPEND, &cl), EINVAL,
              "CREATE_LEASE rejects flags other than O_CLOEXEC and O_NONBLOCK");

    CHECK_RET(create_lease(master, objects, 3, O_CLOEXEC, &cl), 0, "CREATE_LEASE");
    int lessee = (int)cl.fd;
    CHECK(cl.lessee_id != 0, "lease reports a lessee id");
    CHECK(lessee >= 0 && lessee != master, "lease returns a new descriptor");
    CHECK(fcntl(lessee, F_GETFD) == FD_CLOEXEC, "lessee descriptor honours O_CLOEXEC");

    uint32_t leased[4] = {0};
    struct drm_mode_get_lease gl = {
        .count_objects = 4,
        .objects_ptr = (uint64_t)(uintptr_t)leased,
    };
    CHECK_RET(ioctl(lessee, DRM_IOCTL_MODE_GET_LEASE, &gl), 0, "lessee GET_LEASE");
    CHECK(gl.count_objects == 3 && contains(leased, 3, crtcs[0]) &&
          contains(leased, 3, connectors[0]) && contains(leased, 3, planes[0]),
          "lessee holds exactly the leased objects");

    uint32_t lessee_crtcs[4] = {0};
    struct drm_mode_card_res lres = {
        .crtc_id_ptr = (uint64_t)(uintptr_t)lessee_crtcs,
        .count_crtcs = 4,
    };
    CHECK_RET(ioctl(lessee, DRM_IOCTL_MODE_GETRESOURCES, &lres), 0,
              "lessee GETRESOURCES");
    CHECK(lres.count_crtcs == 1 && lessee_crtcs[0] == crtcs[0],
          "lessee only sees its leased CRTC");

    struct drm_mode_create_lease sublease;
    CHECK_ERR(create_lease(lessee, objects, 3, 0, &sublease), EINVAL,
              "lessee is a master but may not sub-lease");

    uint32_t listed = 0;
    CHECK(count_lessees(master, &listed) == 1 && listed == cl.lessee_id,
          "lessor lists the lessee");
    close(lessee);
    CHECK(count_lessees(master, NULL) == 0, "closing the lessee file ends the lease");

    CHECK_RET(create_lease(master, objects, 3, 0, &cl), 0, "CREATE_LEASE again");
    lessee = (int)cl.fd;
    CHECK(fcntl(lessee, F_GETFD) == 0, "lessee descriptor without O_CLOEXEC");
    struct drm_mode_revoke_lease revoke = { .lessee_id = cl.lessee_id };
    CHECK_RET(ioctl(master, DRM_IOCTL_MODE_REVOKE_LEASE, &revoke), 0, "REVOKE_LEASE");
    gl.count_objects = 4;
    CHECK_RET(ioctl(lessee, DRM_IOCTL_MODE_GET_LEASE, &gl), 0,
              "revoked lessee GET_LEASE");
    CHECK(gl.count_objects == 0, "revoked lessee holds no objects");
    CHECK(count_lessees(master, NULL) == 0, "LIST_LESSEES skips a revoked lease");
    close(lessee);
    CHECK_ERR(ioctl(master, DRM_IOCTL_MODE_REVOKE_LEASE, &revoke), ENOENT,
              "revoking a closed lessee reports ENOENT");
    close(master);
    TEST_DONE();
}
