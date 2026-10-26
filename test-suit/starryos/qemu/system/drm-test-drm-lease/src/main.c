/*
 * DRM lease: a lessee gets its own master file description that only sees the
 * leased CRTC, connector and plane. Revoking empties the lease and hides it
 * from LIST_LESSEES; closing the lessee file ends it. Legacy clients receive
 * implicit primary/cursor planes, while universal-plane clients name planes
 * explicitly. Query padding and rejected requests must not alter the lease.
 */
#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <stdint.h>
#include <sys/mman.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <unistd.h>

struct drm_mode_card_res {
    uint64_t fb_id_ptr; uint64_t crtc_id_ptr; uint64_t connector_id_ptr;
    uint64_t encoder_id_ptr;
    uint32_t count_fbs; uint32_t count_crtcs;
    uint32_t count_connectors; uint32_t count_encoders;
    uint32_t min_width, max_width, min_height, max_height;
};
struct drm_mode_mode_info {
    uint32_t clock;
    uint16_t hdisplay, hsync_start, hsync_end, htotal, hskew;
    uint16_t vdisplay, vsync_start, vsync_end, vtotal, vscan;
    uint32_t vrefresh, flags, kind; char name[32];
};
struct drm_mode_get_connector {
    uint64_t encoders_ptr, modes_ptr, props_ptr, prop_values_ptr;
    uint32_t count_modes, count_props, count_encoders, encoder_id, connector_id;
    uint32_t connector_type, connector_type_id, connection, mm_width, mm_height;
    uint32_t subpixel, pad;
};
struct drm_mode_crtc {
    uint64_t set_connectors_ptr;
    uint32_t count_connectors, crtc_id, fb_id, x, y, gamma_size, mode_valid;
    struct drm_mode_mode_info mode;
};
struct drm_mode_create_dumb {
    uint32_t height, width, bpp, flags, handle, pitch;
    uint64_t size;
};
struct drm_mode_fb_cmd {
    uint32_t fb_id, width, height, pitch, bpp, depth, handle;
};
struct drm_mode_destroy_dumb { uint32_t handle; };
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
#define DRM_IOCTL_MODE_GETCRTC           _IOWR('d', 0xA1, struct drm_mode_crtc)
#define DRM_IOCTL_MODE_SETCRTC           _IOWR('d', 0xA2, struct drm_mode_crtc)
#define DRM_IOCTL_MODE_GETCONNECTOR      _IOWR('d', 0xA7, struct drm_mode_get_connector)
#define DRM_IOCTL_MODE_ADDFB             _IOWR('d', 0xAE, struct drm_mode_fb_cmd)
#define DRM_IOCTL_MODE_RMFB              _IOWR('d', 0xAF, uint32_t)
#define DRM_IOCTL_MODE_CREATE_DUMB       _IOWR('d', 0xB2, struct drm_mode_create_dumb)
#define DRM_IOCTL_MODE_DESTROY_DUMB      _IOWR('d', 0xB4, struct drm_mode_destroy_dumb)
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

/* SETCRTC implicitly uses the primary plane. A lease listing only a CRTC
 * and connector must therefore authorize more than GET_LEASE enumeration. */
static void check_legacy_scanout(int fd, uint32_t crtc, uint32_t connector)
{
    struct drm_mode_mode_info mode = {0};
    struct drm_mode_get_connector conn = {
        .connector_id = connector, .count_modes = 1,
        .modes_ptr = (uint64_t)(uintptr_t)&mode,
    };
    CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_GETCONNECTOR, &conn), 0,
              "legacy lessee reads its connector mode");
    CHECK(conn.count_modes > 0 && mode.hdisplay > 0 && mode.vdisplay > 0,
          "legacy lessee has a scanout mode");
    struct drm_mode_create_dumb dumb = {
        .width = mode.hdisplay, .height = mode.vdisplay, .bpp = 32,
    };
    CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_CREATE_DUMB, &dumb), 0,
              "legacy lessee creates a scanout buffer");
    struct drm_mode_fb_cmd fb = {
        .width = dumb.width, .height = dumb.height, .pitch = dumb.pitch,
        .bpp = 32, .depth = 24, .handle = dumb.handle,
    };
    CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_ADDFB, &fb), 0,
              "legacy lessee creates its framebuffer");
    struct drm_mode_crtc enable = {
        .set_connectors_ptr = (uint64_t)(uintptr_t)&connector,
        .count_connectors = 1, .crtc_id = crtc, .fb_id = fb.fb_id,
        .mode_valid = 1, .mode = mode,
    };
    CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_SETCRTC, &enable), 0,
              "implicit primary-plane lease permits real scanout");
    struct drm_mode_crtc observed = { .crtc_id = crtc };
    CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_GETCRTC, &observed), 0,
              "read back leased CRTC after scanout");
    CHECK(observed.mode_valid && observed.fb_id == fb.fb_id,
          "leased CRTC actually presents the submitted framebuffer");
    struct drm_mode_crtc disable = { .crtc_id = crtc };
    CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_SETCRTC, &disable), 0,
              "disable leased scanout before releasing its framebuffer");
    CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_RMFB, &fb.fb_id), 0,
              "remove leased framebuffer");
    struct drm_mode_destroy_dumb destroy = { .handle = dumb.handle };
    CHECK_RET(syscall(SYS_ioctl, fd, DRM_IOCTL_MODE_DESTROY_DUMB, &destroy), 0,
              "release leased scanout buffer");
}

/* Adjacent mappings with different backing must not be treated as one Rust
 * allocation. GET_LEASE advances into the second mapping; LIST_LESSEES also
 * exercises a single (unaligned) output that straddles the mapping boundary. */
static void check_query_mappings(int master, int lessee, const uint32_t *objects,
                                  uint32_t lessee_id)
{
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    void *region = mmap(NULL, page * 2, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(region != MAP_FAILED, "reserve adjacent query mappings");
    if (region == MAP_FAILED)
        return;
    uintptr_t boundary = (uintptr_t)region + page;
    int backing = (int)syscall(SYS_memfd_create, "lease-query", 0);
    CHECK(backing >= 0, "create independent query mapping backing");
    if (backing < 0)
        goto out;
    int truncated = ftruncate(backing, (off_t)page);
    CHECK(truncated == 0, "size query mapping backing");
    if (truncated != 0)
        goto out;
    void *first = mmap(region, page, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    void *second = mmap((void *)boundary, page, PROT_READ | PROT_WRITE,
                        MAP_SHARED | MAP_FIXED, backing, 0);
    CHECK(first == region && second == (void *)boundary,
          "install adjacent private and file-backed query mappings");
    if (first != region || second != (void *)boundary)
        goto out;

    struct drm_mode_get_lease get = {
        .count_objects = 3, .objects_ptr = boundary - sizeof(uint32_t),
    };
    CHECK_RET(syscall(SYS_ioctl, lessee, DRM_IOCTL_MODE_GET_LEASE, &get), 0,
              "GET_LEASE writes across independent mappings");
    uint32_t ids[3];
    memcpy(ids, (void *)(uintptr_t)get.objects_ptr, sizeof(ids));
    CHECK(get.count_objects == 3 && contains(ids, 3, objects[0]) &&
          contains(ids, 3, objects[1]) && contains(ids, 3, objects[2]),
          "cross-mapping output contains the leased object IDs");
    struct drm_mode_list_lessees list = {
        .count_lessees = 1, .lessees_ptr = boundary - 2,
    };
    CHECK_RET(syscall(SYS_ioctl, master, DRM_IOCTL_MODE_LIST_LESSEES, &list), 0,
              "LIST_LESSEES writes an ID across the mapping boundary");
    uint32_t listed = 0;
    memcpy(&listed, (void *)(uintptr_t)list.lessees_ptr, sizeof(listed));
    CHECK(list.count_lessees == 1 && listed == lessee_id,
          "cross-mapping output contains the active lessee ID");

    int protected = mprotect(second, page, PROT_NONE);
    CHECK(protected == 0, "make the second query mapping inaccessible");
    if (protected == 0) {
        get.count_objects = 4;
        CHECK_ERR(syscall(SYS_ioctl, lessee, DRM_IOCTL_MODE_GET_LEASE, &get),
                  EFAULT, "GET_LEASE faults after a valid output prefix");
        CHECK(get.count_objects == 4, "partial output fault preserves object count");
        memcpy(&listed, (void *)(uintptr_t)get.objects_ptr, sizeof(listed));
        CHECK(contains(objects, 3, listed), "valid prefix was copied before EFAULT");
        list.count_lessees = 4;
        CHECK_ERR(syscall(SYS_ioctl, master, DRM_IOCTL_MODE_LIST_LESSEES, &list),
                  EFAULT, "LIST_LESSEES faults on a split output ID");
        CHECK(list.count_lessees == 4, "split output fault preserves lessee count");
        CHECK_RET(mprotect(second, page, PROT_READ | PROT_WRITE), 0,
                  "restore query mapping permissions before teardown");
    }
out:
    CHECK_RET(munmap(region, page), 0, "release private query mapping");
    CHECK_RET(munmap((void *)boundary, page), 0, "release file-backed query mapping");
    if (backing >= 0)
        CHECK_RET(close(backing), 0, "close query mapping backing");
}

int main(void)
{
    TEST_START("drm-lease");

    int master = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
    CHECK(master >= 0, "open /dev/dri/card0 as the lessor");
    if (master < 0) {
        TEST_DONE();
    }
    uint32_t crtcs[4] = {0}, connectors[4] = {0};
    struct drm_mode_card_res res = {
        .crtc_id_ptr = (uint64_t)(uintptr_t)crtcs,
        .connector_id_ptr = (uint64_t)(uintptr_t)connectors,
        .count_crtcs = 4,
        .count_connectors = 4,
    };
    CHECK_RET(ioctl(master, DRM_IOCTL_MODE_GETRESOURCES, &res), 0,
              "lessor GETRESOURCES");
    CHECK(res.count_crtcs >= 1 && res.count_connectors >= 1,
          "device exposes a CRTC and connector for legacy leasing");
    struct drm_mode_list_lessees invalid_list = { .pad = 1 };
    CHECK_ERR(syscall(SYS_ioctl, master, DRM_IOCTL_MODE_LIST_LESSEES,
                      &invalid_list), EINVAL, "LIST_LESSEES rejects nonzero pad");
    uint32_t legacy_objects[2] = { crtcs[0], connectors[0] };
    struct drm_mode_create_lease legacy;
    int legacy_result = create_lease(master, legacy_objects, 2, O_CLOEXEC, &legacy);
    CHECK(legacy_result == 0,
          "default client leases CRTC and connector without explicit planes");
    if (legacy_result == 0) {
        uint32_t legacy_ids[4] = {0};
        struct drm_mode_get_lease legacy_get = {
            .count_objects = 4, .objects_ptr = (uint64_t)(uintptr_t)legacy_ids,
        };
        CHECK_RET(syscall(SYS_ioctl, legacy.fd, DRM_IOCTL_MODE_GET_LEASE,
                          &legacy_get), 0, "GET_LEASE includes implicit objects");
        CHECK(legacy_get.count_objects >= 3 &&
              contains(legacy_ids, 4, crtcs[0]) &&
              contains(legacy_ids, 4, connectors[0]),
              "legacy lease includes display objects and implicit planes");
        struct drm_mode_create_lease busy;
        int busy_result = create_lease(master, legacy_objects, 2, 0, &busy);
        CHECK(busy_result == -1 && errno == EBUSY,
              "active legacy lease rejects a second lease with EBUSY");
        if (busy_result == 0)
            close((int)busy.fd);
        CHECK(count_lessees(master, NULL) == 1,
              "conflicting lease leaves the original lessee intact");
        check_legacy_scanout((int)legacy.fd, crtcs[0], connectors[0]);
        CHECK_RET(close((int)legacy.fd), 0, "close legacy lease");
    }
    CHECK(count_lessees(master, NULL) == 0, "legacy lease releases its objects");
    struct drm_set_client_cap cap = {
        .capability = DRM_CLIENT_CAP_UNIVERSAL_PLANES, .value = 1,
    };
    CHECK_RET(ioctl(master, DRM_IOCTL_SET_CLIENT_CAP, &cap), 0,
              "lessor enables universal planes so planes can be leased");
    struct drm_mode_create_lease missing_plane;
    int missing_result = create_lease(master, legacy_objects, 2, 0, &missing_plane);
    CHECK(missing_result == -1 && errno == EINVAL,
          "universal-plane lease without an explicit plane is EINVAL");
    if (missing_result == 0)
        close((int)missing_plane.fd);
    CHECK(count_lessees(master, NULL) == 0,
          "rejected incomplete lease leaves no published lessee");

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
    cap.value = 0;
    CHECK_RET(ioctl(master, DRM_IOCTL_SET_CLIENT_CAP, &cap), 0,
              "disable universal planes to exercise implicit-ID collision");
    /* The CRTC adds its primary plane before the same explicit ID is visited.
     * Failure must discard the partially populated set, not publish a lessee. */
    int duplicate_result = create_lease(master, objects, 3, 0, &cl);
    CHECK(duplicate_result == -1 && errno == ENOSPC,
          "duplicate explicit and implicit plane IDs reject the lease");
    if (duplicate_result == 0)
        close((int)cl.fd);
    CHECK(count_lessees(master, NULL) == 0,
          "failed implicit-plane insertion rolls back the unpublished lease");
    cap.value = 1;
    CHECK_RET(ioctl(master, DRM_IOCTL_SET_CLIENT_CAP, &cap), 0,
              "restore universal planes before retrying an explicit lease");
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
    struct drm_mode_get_lease invalid_get = { .pad = 1 };
    CHECK_ERR(syscall(SYS_ioctl, lessee, DRM_IOCTL_MODE_GET_LEASE, &invalid_get),
              EINVAL, "GET_LEASE rejects nonzero pad");
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
    struct drm_mode_list_lessees list_count = {0};
    CHECK_RET(syscall(SYS_ioctl, master, DRM_IOCTL_MODE_LIST_LESSEES,
                      &list_count), 0, "LIST_LESSEES supports count-only queries");
    CHECK(list_count.count_lessees == 1, "count-only query reports the active lease");
    struct drm_mode_get_lease object_count = {0};
    CHECK_RET(syscall(SYS_ioctl, lessee, DRM_IOCTL_MODE_GET_LEASE,
                      &object_count), 0, "GET_LEASE supports count-only queries");
    CHECK(object_count.count_objects == 3, "count-only query reports the leased objects");
    /* A zero-capacity query only counts. With capacity, both null and
     * inaccessible arrays must fail without rewriting the input count. */
    for (uintptr_t address = 0; address <= 1; address++) {
        struct drm_mode_list_lessees bad_list = {
            .count_lessees = 4, .lessees_ptr = address,
        };
        CHECK_ERR(syscall(SYS_ioctl, master, DRM_IOCTL_MODE_LIST_LESSEES,
                          &bad_list), EFAULT, "LIST_LESSEES rejects a bad array");
        CHECK(bad_list.count_lessees == 4,
              "failed LIST_LESSEES preserves the input count");
        struct drm_mode_get_lease bad_get = {
            .count_objects = 4, .objects_ptr = address,
        };
        CHECK_ERR(syscall(SYS_ioctl, lessee, DRM_IOCTL_MODE_GET_LEASE,
                          &bad_get), EFAULT, "GET_LEASE rejects a bad array");
        CHECK(bad_get.count_objects == 4,
              "failed GET_LEASE preserves the input count");
    }
    check_query_mappings(master, lessee, objects, cl.lessee_id);
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
    struct drm_mode_create_lease replacement;
    int replacement_result = create_lease(master, objects, 3, 0, &replacement);
    CHECK(replacement_result == 0,
          "revoked objects can be leased again before the old fd closes");
    if (replacement_result == 0)
        CHECK_RET(close((int)replacement.fd), 0, "close replacement lease");
    close(lessee);
    CHECK_ERR(ioctl(master, DRM_IOCTL_MODE_REVOKE_LEASE, &revoke), ENOENT,
              "revoking a closed lessee reports ENOENT");
    close(master);
    TEST_DONE();
}
