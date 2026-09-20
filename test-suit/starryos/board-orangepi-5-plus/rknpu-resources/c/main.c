#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

/* Starry's resource policy, exercised with at most 16 MiB of live small GEMs.
 * Large aggregate byte limits are covered by the driver's component tests. */
enum { PAGE_BYTES = 4096, OWNER_OBJECTS = 1024, DEVICE_OWNERS = 4 };

struct mem_create {
    uint32_t handle, flags;
    uint64_t size, obj_addr, dma_addr, sram_size;
    int32_t iommu_domain_id;
    uint32_t core_mask;
};

struct mem_destroy {
    uint32_t handle, reserved;
    uint64_t obj_addr;
};

struct mem_map {
    uint32_t handle, reserved;
    uint64_t offset;
};

struct prime_handle {
    uint32_t handle, flags;
    int32_t fd;
};

#define MEM_CREATE _IOWR('d', 0x42, struct mem_create)
#define MEM_MAP _IOWR('d', 0x43, struct mem_map)
#define MEM_DESTROY _IOWR('d', 0x44, struct mem_destroy)
#define PRIME_EXPORT _IOWR('d', 0x2d, struct prime_handle)

static const char *phase;

static void require(int condition, const char *operation)
{
    if (!condition) {
        char message[256];
        int size = snprintf(message, sizeof(message),
            "\nSTARRY_RKNPU_RESOURCES_FAILED phase=%s operation=%s errno=%d\n",
            phase, operation, errno);
        if (size > 0 && (size_t)size < sizeof(message))
            syscall(SYS_write, STDOUT_FILENO, message, (size_t)size);
        else {
            static const char fallback[] = "\nSTARRY_RKNPU_RESOURCES_FAILED\n";
            syscall(SYS_write, STDOUT_FILENO, fallback, sizeof(fallback) - 1);
        }
        exit(1);
    }
}

static int open_card(void)
{
    int fd = syscall(SYS_openat, AT_FDCWD, "/dev/dri/card1", O_RDWR | O_CLOEXEC, 0);
    require(fd >= 0, "open");
    return fd;
}

static void close_fd(int fd)
{
    require(syscall(SYS_close, fd) == 0, "close");
}

static void destroy(int fd, uint32_t handle)
{
    struct mem_destroy request = {.handle = handle};
    require(syscall(SYS_ioctl, fd, MEM_DESTROY, &request) == 0, "destroy");
}

static uint32_t create_page(int fd)
{
    struct mem_create request = {.size = PAGE_BYTES};
    require(syscall(SYS_ioctl, fd, MEM_CREATE, &request) == 0, "create page");
    return request.handle;
}

static void expect_quota(int fd, uint64_t size)
{
    struct mem_create request = {.size = size};
    errno = 0;
    long result = syscall(SYS_ioctl, fd, MEM_CREATE, &request);
    int error = errno;
    /* Restore the old implementation's successful allocation before reporting
     * the regression, including the single 64 MiB + 1 request. */
    if (result == 0)
        destroy(fd, request.handle);
    errno = error;
    require(result == -1 && error == ENOMEM, "quota must return ENOMEM");
}

static void owner_lifetime(void)
{
    phase = "owner and backing lifetime";
    int owner = open_card();
    expect_quota(owner, 64ULL * 1024 * 1024 + 1);
    uint32_t mapped_handle = create_page(owner);
    uint32_t exported_handle = create_page(owner);
    for (unsigned i = 2; i < OWNER_OBJECTS; ++i)
        create_page(owner);
    expect_quota(owner, PAGE_BYTES);

    int independent = open_card();
    create_page(independent);
    struct mem_map map = {.handle = mapped_handle};
    require(syscall(SYS_ioctl, owner, MEM_MAP, &map) == 0, "map offset");
    volatile uint32_t *mapping = (void *)syscall(SYS_mmap, NULL, PAGE_BYTES,
        PROT_READ | PROT_WRITE, MAP_SHARED, owner, map.offset);
    require(mapping != MAP_FAILED, "mmap");
    *mapping = 0x73514a29;

    struct prime_handle exported = {.handle = exported_handle, .flags = O_CLOEXEC};
    require(syscall(SYS_ioctl, owner, PRIME_EXPORT, &exported) == 0, "PRIME export");
    destroy(owner, mapped_handle);
    destroy(owner, exported_handle);
    require(*mapping == 0x73514a29, "mapping survives handle destroy");
    expect_quota(owner, PAGE_BYTES);
    /* Each independent backing releases exactly one slot at its final drop. */
    close_fd(exported.fd);
    create_page(owner);
    expect_quota(owner, PAGE_BYTES);
    require(syscall(SYS_munmap, mapping, PAGE_BYTES) == 0, "munmap");
    create_page(owner);
    expect_quota(owner, PAGE_BYTES);
    close_fd(independent);
    close_fd(owner);
}

static void device_lifetime(void)
{
    phase = "device and duplicated file lifetime";
    int owners[DEVICE_OWNERS];
    for (unsigned i = 0; i < DEVICE_OWNERS; ++i) {
        owners[i] = open_card();
        for (unsigned j = 0; j < OWNER_OBJECTS; ++j)
            create_page(owners[i]);
    }
    int next = open_card();
    expect_quota(next, PAGE_BYTES);
    int duplicate = syscall(SYS_dup, owners[0]);
    require(duplicate >= 0, "dup");
    close_fd(owners[0]);
    expect_quota(next, PAGE_BYTES);
    close_fd(duplicate);
    for (unsigned i = 0; i < OWNER_OBJECTS; ++i)
        create_page(next);
    expect_quota(next, PAGE_BYTES);
    close_fd(next);
    for (unsigned i = 1; i < DEVICE_OWNERS; ++i)
        close_fd(owners[i]);
}

int main(void)
{
    owner_lifetime();
    device_lifetime();
    /* Publish the whole line in one write so kernel logs cannot separate the
     * success token from its newline. The leading newline also isolates it
     * from preceding console output. */
    static const char success[] = "\nSTARRY_RKNPU_RESOURCES_OK\n";
    return syscall(SYS_write, STDOUT_FILENO, success, sizeof(success) - 1)
        == (long)(sizeof(success) - 1) ? 0 : 1;
}
