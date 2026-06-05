#include "k230_sdk_compat.h"

#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

extern "C" int __real_open(const char *pathname, int flags, ...);
extern "C" int __real_openat(int dirfd, const char *pathname, int flags, ...);
extern "C" int __real_close(int fd);
extern "C" void *__real_mmap(void *addr, size_t length, int prot, int flags,
                              int fd, off_t offset);
extern "C" int __real_munmap(void *addr, size_t length);
extern "C" int __real_ioctl(int fd, unsigned long request, ...);

namespace {

constexpr int kFakeFdBase = 23000;
constexpr int kFakeFdGnne = kFakeFdBase + 1;
constexpr int kFakeFdAi2d = kFakeFdBase + 2;
constexpr int kFakeFdMem = kFakeFdBase + 3;
constexpr size_t kPageSize = 4096;
constexpr size_t kMaxAllocations = 256;
constexpr size_t kMaxMappings = 256;

struct RuntimeWindow {
    uint64_t paddr;
    uint64_t size;
    uint64_t mmap_offset;
    const char *name;
};

constexpr RuntimeWindow kWindows[] = {
    {KPU_CFG_PADDR, KPU_CFG_SIZE, KPU_MMAP_CFG_OFFSET, "cfg"},
    {KPU_L2_PADDR, KPU_L2_SIZE, KPU_MMAP_L2_OFFSET, "l2"},
    {KPU_FAKE_OUTPUT_PADDR, KPU_FAKE_OUTPUT_SIZE, KPU_MMAP_FAKE_OUTPUT_OFFSET,
     "fake-output"},
    {KPU_RUNTIME_RDATA_PADDR, KPU_RUNTIME_RDATA_SIZE,
     KPU_MMAP_RUNTIME_RDATA_OFFSET, "rdata"},
    {KPU_RUNTIME_COMMAND_PADDR, KPU_RUNTIME_COMMAND_SIZE,
     KPU_MMAP_RUNTIME_COMMAND_OFFSET, "command"},
    {KPU_RUNTIME_DIRECT_IO_PADDR, KPU_RUNTIME_DIRECT_IO_SIZE,
     KPU_MMAP_RUNTIME_DIRECT_IO_OFFSET, "direct-io"},
    {KPU_RUNTIME_DDR_PADDR, KPU_RUNTIME_DDR_SIZE, KPU_MMAP_RUNTIME_DDR_OFFSET,
     "ddr"},
};

struct Allocation {
    uint64_t paddr;
    void *vaddr;
    size_t size;
    bool live;
};

struct Mapping {
    void *returned;
    void *base;
    size_t len;
    uint64_t paddr;
    bool live;
};

int g_kpu_fd = -1;
uint8_t *g_ddr = nullptr;
size_t g_ddr_bump = 0;
Allocation g_allocations[kMaxAllocations];
Mapping g_mappings[kMaxMappings];
unsigned g_run_count = 0;
unsigned g_mmz_alloc_count = 0;
bool g_identity_mapped = false;
bool g_runtime_rdata_mirrored = false;
uint64_t g_runtime_rdata_source = 0;

size_t align_up(size_t value, size_t align) {
    return (value + align - 1) & ~(align - 1);
}

int open_real_with_mode(const char *pathname, int flags, va_list ap) {
    if ((flags & O_CREAT) != 0) {
        mode_t mode = static_cast<mode_t>(va_arg(ap, int));
        return __real_open(pathname, flags, mode);
    }
    return __real_open(pathname, flags);
}

int openat_real_with_mode(int dirfd, const char *pathname, int flags, va_list ap) {
    if ((flags & O_CREAT) != 0) {
        mode_t mode = static_cast<mode_t>(va_arg(ap, int));
        return __real_openat(dirfd, pathname, flags, mode);
    }
    return __real_openat(dirfd, pathname, flags);
}

int fake_fd_for_path(const char *pathname) {
    if (!pathname) {
        return -1;
    }
    if (strcmp(pathname, "/dev/gnne_device") == 0) {
        return kFakeFdGnne;
    }
    if (strcmp(pathname, "/dev/ai_2d_device") == 0) {
        return kFakeFdAi2d;
    }
    if (strcmp(pathname, "/dev/mem") == 0) {
        return kFakeFdMem;
    }
    return -1;
}

bool is_fake_fd(int fd) {
    return fd == kFakeFdGnne || fd == kFakeFdAi2d || fd == kFakeFdMem;
}

int ensure_kpu_fd() {
    if (g_kpu_fd >= 0) {
        return g_kpu_fd;
    }
    g_kpu_fd = __real_open(KPU_DEVICE_PATH, O_RDWR | O_CLOEXEC);
    if (g_kpu_fd < 0) {
        g_kpu_fd = __real_open(KPU_DEVICE_ALIAS_PATH, O_RDWR | O_CLOEXEC);
    }
    if (g_kpu_fd < 0) {
        fprintf(stderr, "K230_SDK_COMPAT: cannot open %s: %s\n",
                KPU_DEVICE_PATH, strerror(errno));
    }
    return g_kpu_fd;
}

const RuntimeWindow *find_window_by_paddr(uint64_t paddr, size_t len) {
    for (const auto &window : kWindows) {
        if (paddr >= window.paddr &&
            paddr + len <= window.paddr + window.size) {
            return &window;
        }
    }
    return nullptr;
}

void remember_mapping(void *returned, void *base, size_t len, uint64_t paddr) {
    for (auto &mapping : g_mappings) {
        if (mapping.live && mapping.returned == returned) {
            mapping.base = base;
            mapping.len = len;
            mapping.paddr = paddr;
            return;
        }
    }
    for (auto &mapping : g_mappings) {
        if (!mapping.live) {
            mapping.returned = returned;
            mapping.base = base;
            mapping.len = len;
            mapping.paddr = paddr;
            mapping.live = true;
            return;
        }
    }
    fprintf(stderr, "K230_SDK_COMPAT: mapping table full\n");
}

Mapping *find_mapping_by_returned(void *returned) {
    for (auto &mapping : g_mappings) {
        if (mapping.live && mapping.returned == returned) {
            return &mapping;
        }
    }
    return nullptr;
}

bool translate_vaddr(const void *addr, size_t len, uint64_t *paddr) {
    const uintptr_t value = reinterpret_cast<uintptr_t>(addr);
    for (const auto &allocation : g_allocations) {
        if (!allocation.live) {
            continue;
        }
        const uintptr_t start = reinterpret_cast<uintptr_t>(allocation.vaddr);
        const uintptr_t end = start + allocation.size;
        if (value >= start && value + len <= end) {
            *paddr = allocation.paddr + (value - start);
            return true;
        }
    }
    for (const auto &mapping : g_mappings) {
        if (!mapping.live) {
            continue;
        }
        const uintptr_t start = reinterpret_cast<uintptr_t>(mapping.returned);
        const uintptr_t end = start + mapping.len;
        if (value >= start && value + len <= end) {
            *paddr = mapping.paddr + (value - start);
            return true;
        }
    }
    return false;
}

bool range_in(uint64_t start, size_t len, uint64_t base, uint64_t size) {
    return start >= base && start - base <= size && len <= size - (start - base);
}

bool runtime_ddr_alias(uint64_t paddr, size_t len, uint64_t *alias_paddr) {
    if (!range_in(paddr, len, KPU_RUNTIME_DDR_PADDR, KPU_RUNTIME_DDR_SIZE)) {
        return false;
    }
    uint64_t alias = KPU_RUNTIME_RDATA_PADDR + (paddr - KPU_RUNTIME_DDR_PADDR);
    if (!find_window_by_paddr(alias, len)) {
        return false;
    }
    *alias_paddr = alias;
    return true;
}

void *translate_paddr_to_vaddr(uint64_t paddr, size_t len) {
    for (const auto &allocation : g_allocations) {
        if (!allocation.live) {
            continue;
        }
        if (paddr >= allocation.paddr &&
            paddr + len <= allocation.paddr + allocation.size) {
            return static_cast<uint8_t *>(allocation.vaddr) +
                   (paddr - allocation.paddr);
        }
    }
    for (const auto &mapping : g_mappings) {
        if (!mapping.live) {
            continue;
        }
        if (paddr >= mapping.paddr && paddr + len <= mapping.paddr + mapping.len) {
            return static_cast<uint8_t *>(mapping.returned) +
                   (paddr - mapping.paddr);
        }
    }
    return nullptr;
}

void *map_window(uint64_t paddr, size_t len, int prot, int flags) {
    const RuntimeWindow *window = find_window_by_paddr(paddr, len);
    if (!window) {
        errno = EINVAL;
        return MAP_FAILED;
    }
    if (ensure_kpu_fd() < 0) {
        return MAP_FAILED;
    }
    void *base = __real_mmap(nullptr, window->size, prot, flags, g_kpu_fd,
                             static_cast<off_t>(window->mmap_offset));
    if (base == MAP_FAILED) {
        fprintf(stderr, "K230_SDK_COMPAT: mmap %s failed: %s\n", window->name,
                strerror(errno));
        return MAP_FAILED;
    }
    const size_t delta = static_cast<size_t>(paddr - window->paddr);
    void *returned = static_cast<uint8_t *>(base) + delta;
    remember_mapping(returned, base, window->size - delta, paddr);
    return returned;
}

int copy_to_runtime_window(uint64_t dst_paddr, const void *src, size_t len) {
    const auto *src_bytes = static_cast<const uint8_t *>(src);
    size_t copied = 0;
    while (copied < len) {
        const RuntimeWindow *window = find_window_by_paddr(dst_paddr + copied, 1);
        if (!window) {
            fprintf(stderr,
                    "K230_SDK_COMPAT: no runtime window for mirror dst=0x%llx\n",
                    static_cast<unsigned long long>(dst_paddr + copied));
            return -1;
        }
        size_t window_offset = static_cast<size_t>(dst_paddr + copied - window->paddr);
        size_t chunk = window->size - window_offset;
        if (chunk > len - copied) {
            chunk = len - copied;
        }
        void *dst = map_window(dst_paddr + copied, chunk, PROT_READ | PROT_WRITE,
                               MAP_SHARED);
        if (dst == MAP_FAILED) {
            return -1;
        }
        memcpy(dst, src_bytes + copied, chunk);
        copied += chunk;
    }
    return 0;
}

int map_identity_window(const RuntimeWindow &window) {
    void *addr = reinterpret_cast<void *>(static_cast<uintptr_t>(window.paddr));
    void *map = __real_mmap(addr, window.size, PROT_READ | PROT_WRITE,
                            MAP_SHARED | MAP_FIXED, g_kpu_fd,
                            static_cast<off_t>(window.mmap_offset));
    if (map == MAP_FAILED) {
        fprintf(stderr,
                "K230_SDK_COMPAT: identity mmap %s at 0x%llx failed: %s\n",
                window.name, static_cast<unsigned long long>(window.paddr),
                strerror(errno));
        return -1;
    }
    remember_mapping(map, map, window.size, window.paddr);
    if (window.paddr == KPU_RUNTIME_DDR_PADDR) {
        g_ddr = static_cast<uint8_t *>(map);
    }
    fprintf(stderr, "K230_SDK_COMPAT: identity mmap %s 0x%llx..0x%llx\n",
            window.name, static_cast<unsigned long long>(window.paddr),
            static_cast<unsigned long long>(window.paddr + window.size));
    return 0;
}

int ensure_identity_mappings() {
    if (g_identity_mapped) {
        return 0;
    }
    if (ensure_kpu_fd() < 0) {
        return -1;
    }
    for (const auto &window : kWindows) {
        if (window.paddr != KPU_L2_PADDR) {
            continue;
        }
        if (map_identity_window(window) != 0) {
            return -1;
        }
    }
    g_identity_mapped = true;
    return 0;
}

uint8_t *ensure_ddr_map() {
    if (g_ddr) {
        return g_ddr;
    }
    if (ensure_kpu_fd() < 0) {
        return nullptr;
    }
    void *map = __real_mmap(nullptr, KPU_RUNTIME_DDR_SIZE, PROT_READ | PROT_WRITE,
                            MAP_SHARED, g_kpu_fd, KPU_MMAP_RUNTIME_DDR_OFFSET);
    if (map == MAP_FAILED) {
        fprintf(stderr, "K230_SDK_COMPAT: mmap DDR failed: %s\n", strerror(errno));
        return nullptr;
    }
    g_ddr = static_cast<uint8_t *>(map);
    remember_mapping(g_ddr, g_ddr, KPU_RUNTIME_DDR_SIZE, KPU_RUNTIME_DDR_PADDR);
    return g_ddr;
}

int run_kpu_command(uint64_t command_paddr, size_t command_len) {
    if (ensure_kpu_fd() < 0) {
        return -1;
    }
    struct k230_kpu_command_range range = {
        .start_paddr = command_paddr,
        .end_paddr = command_paddr + command_len,
    };
    if (__real_ioctl(g_kpu_fd, KPU_IOC_RUN, &range) != 0) {
        fprintf(stderr, "K230_SDK_COMPAT: KPU_IOC_RUN failed: %s\n",
                strerror(errno));
        return -1;
    }
    if (__real_ioctl(g_kpu_fd, KPU_IOC_WAIT_DONE, 60000000UL) != 0) {
        fprintf(stderr, "K230_SDK_COMPAT: KPU_IOC_WAIT_DONE failed: %s\n",
                strerror(errno));
        return -1;
    }
    uint64_t status = 0;
    if (__real_ioctl(g_kpu_fd, KPU_IOC_GET_STATUS, &status) != 0) {
        fprintf(stderr, "K230_SDK_COMPAT: KPU_IOC_GET_STATUS failed: %s\n",
                strerror(errno));
        return -1;
    }
    __real_ioctl(g_kpu_fd, KPU_IOC_CLEAR, 0UL);
    ++g_run_count;
    fprintf(stderr,
            "K230_SDK_COMPAT: gnne_enable run=%u command=0x%llx..0x%llx "
            "status=0x%016llx\n",
            g_run_count, static_cast<unsigned long long>(command_paddr),
            static_cast<unsigned long long>(command_paddr + command_len),
            static_cast<unsigned long long>(status));
    return (status & KPU_DONE_STATUS) == KPU_DONE_STATUS ? 0 : -1;
}

uint32_t read_le32(const uint8_t *data) {
    return static_cast<uint32_t>(data[0]) |
           (static_cast<uint32_t>(data[1]) << 8) |
           (static_cast<uint32_t>(data[2]) << 16) |
           (static_cast<uint32_t>(data[3]) << 24);
}

void write_le32(uint8_t *data, uint32_t value) {
    data[0] = static_cast<uint8_t>(value);
    data[1] = static_cast<uint8_t>(value >> 8);
    data[2] = static_cast<uint8_t>(value >> 16);
    data[3] = static_cast<uint8_t>(value >> 24);
}

unsigned patch_runtime_arg_table(void) {
    constexpr size_t kArgTableBytes = 64;
    void *table = translate_paddr_to_vaddr(KPU_RUNTIME_ARG_TABLE_PADDR,
                                           kArgTableBytes);
    if (!table) {
        return 0;
    }
    auto *bytes = static_cast<uint8_t *>(table);
    unsigned patched = 0;
    for (size_t offset = 0; offset < kArgTableBytes; offset += sizeof(uint32_t)) {
        uint32_t word = read_le32(bytes + offset);
        if (word == static_cast<uint32_t>(KPU_RUNTIME_DDR_PADDR + 0x20)) {
            if (!g_runtime_rdata_mirrored || g_runtime_rdata_source != word) {
                constexpr size_t kRuntimeRdataMirrorBytes =
                    KPU_RUNTIME_DIRECT_IO_PADDR - KPU_RUNTIME_RDATA_BASE;
                void *src = translate_paddr_to_vaddr(word,
                                                     kRuntimeRdataMirrorBytes);
                if (!src ||
                    copy_to_runtime_window(KPU_RUNTIME_RDATA_BASE, src,
                                           kRuntimeRdataMirrorBytes) != 0) {
                    fprintf(stderr,
                            "K230_SDK_COMPAT: failed to mirror runtime rdata "
                            "src=0x%08x bytes=%zu\n",
                            word, kRuntimeRdataMirrorBytes);
                } else {
                    g_runtime_rdata_mirrored = true;
                    g_runtime_rdata_source = word;
                    fprintf(stderr,
                            "K230_SDK_COMPAT: mirrored runtime rdata "
                            "0x%08x -> 0x%llx bytes=%zu\n",
                            word,
                            static_cast<unsigned long long>(KPU_RUNTIME_RDATA_BASE),
                            kRuntimeRdataMirrorBytes);
                }
            }
            write_le32(bytes + offset, static_cast<uint32_t>(KPU_RUNTIME_RDATA_BASE));
            ++patched;
        }
    }
    return patched;
}

void log_runtime_arg_table(void) {
    void *table = translate_paddr_to_vaddr(KPU_RUNTIME_ARG_TABLE_PADDR, 16);
    if (!table) {
        return;
    }
    const auto *bytes = static_cast<const uint8_t *>(table);
    fprintf(stderr,
            "K230_SDK_COMPAT: arg_table words=0x%08x 0x%08x 0x%08x "
            "0x%08x\n",
            read_le32(bytes), read_le32(bytes + 4), read_le32(bytes + 8),
            read_le32(bytes + 12));
}

} // namespace

extern "C" int k230_compat_init(void) {
    return ensure_identity_mappings();
}

extern "C" void k230_compat_dump_stats(void) {
    fprintf(stderr, "K230_SDK_COMPAT: stats mmz_alloc=%u kpu_run=%u\n",
            g_mmz_alloc_count, g_run_count);
}

extern "C" int __wrap_open(const char *pathname, int flags, ...) {
    int fake_fd = fake_fd_for_path(pathname);
    if (fake_fd >= 0) {
        return fake_fd;
    }

    va_list ap;
    va_start(ap, flags);
    int fd = open_real_with_mode(pathname, flags, ap);
    va_end(ap);
    return fd;
}

extern "C" int __wrap_openat(int dirfd, const char *pathname, int flags, ...) {
    int fake_fd = fake_fd_for_path(pathname);
    if (fake_fd >= 0) {
        return fake_fd;
    }

    va_list ap;
    va_start(ap, flags);
    int fd = openat_real_with_mode(dirfd, pathname, flags, ap);
    va_end(ap);
    return fd;
}

extern "C" int __wrap_close(int fd) {
    if (is_fake_fd(fd)) {
        return 0;
    }
    return __real_close(fd);
}

extern "C" void *__wrap_mmap(void *addr, size_t length, int prot, int flags,
                              int fd, off_t offset) {
    if (fd == kFakeFdGnne || fd == kFakeFdAi2d) {
        (void)addr;
        (void)offset;
        return map_window(KPU_CFG_PADDR, length > KPU_CFG_SIZE ? KPU_CFG_SIZE : length,
                          prot, MAP_SHARED);
    }
    if (fd == kFakeFdMem) {
        return map_window(static_cast<uint64_t>(offset), length, prot,
                          flags & ~MAP_FIXED);
    }
    return __real_mmap(addr, length, prot, flags, fd, offset);
}

extern "C" int __wrap_munmap(void *addr, size_t length) {
    Mapping *mapping = find_mapping_by_returned(addr);
    if (mapping) {
        int ret = __real_munmap(mapping->base, mapping->len);
        mapping->live = false;
        return ret;
    }
    return __real_munmap(addr, length);
}

extern "C" int __wrap_ioctl(int fd, unsigned long request, ...) {
    if (is_fake_fd(fd)) {
        return 0;
    }
    va_list ap;
    va_start(ap, request);
    void *arg = va_arg(ap, void *);
    va_end(ap);
    return __real_ioctl(fd, request, arg);
}

extern "C" int __wrap_gnne_enable(uint64_t pc_start, uint64_t pc_end,
                                   uint64_t pc_breakpoint) {
    (void)pc_breakpoint;
    if (pc_end <= pc_start) {
        errno = EINVAL;
        return -1;
    }
    const size_t command_len = static_cast<size_t>(pc_end - pc_start);
    uint64_t command_paddr = 0;
    const void *command_data = nullptr;
    const bool pc_is_runtime_paddr =
        find_window_by_paddr(pc_start, command_len) != nullptr;
    bool command_paddr_is_known = pc_is_runtime_paddr;

    if (pc_is_runtime_paddr) {
        command_paddr = pc_start;
        command_data = translate_paddr_to_vaddr(pc_start, command_len);
    } else {
        command_data = reinterpret_cast<const void *>(pc_start);
        if (translate_vaddr(command_data, command_len, &command_paddr)) {
            command_paddr_is_known =
                find_window_by_paddr(command_paddr, command_len) != nullptr;
        }
    }

    if (!command_paddr_is_known && command_data &&
        command_len <= KPU_RUNTIME_COMMAND_SIZE) {
        void *command_window = map_window(KPU_RUNTIME_COMMAND_PADDR, command_len,
                                          PROT_READ | PROT_WRITE, MAP_SHARED);
        if (command_window == MAP_FAILED) {
            return -1;
        }
        memcpy(command_window, command_data, command_len);
        command_paddr = KPU_RUNTIME_COMMAND_PADDR;
    }

    const char *mode = command_paddr_is_known ? "native" : "copied";
    uint64_t alias_paddr = 0;
    if (command_data && runtime_ddr_alias(command_paddr, command_len,
                                          &alias_paddr)) {
        void *alias_window = map_window(alias_paddr, command_len,
                                        PROT_READ | PROT_WRITE, MAP_SHARED);
        if (alias_window == MAP_FAILED) {
            return -1;
        }
        memcpy(alias_window, command_data, command_len);
        command_paddr = alias_paddr;
        mode = "runtime-alias";
    }

    unsigned patched_arg_words = patch_runtime_arg_table();
    fprintf(stderr,
            "K230_SDK_COMPAT: gnne_enable raw=0x%llx..0x%llx len=%zu "
            "mode=%s submit=0x%llx arg_patch=%u\n",
            static_cast<unsigned long long>(pc_start),
            static_cast<unsigned long long>(pc_end), command_len,
            mode, static_cast<unsigned long long>(command_paddr),
            patched_arg_words);
    log_runtime_arg_table();

    if (command_paddr == 0) {
        fprintf(stderr,
                "K230_SDK_COMPAT: cannot translate gnne command %p..%p\n",
                reinterpret_cast<void *>(pc_start),
                reinterpret_cast<void *>(pc_end));
        errno = EFAULT;
        return -1;
    }
    return run_kpu_command(command_paddr, command_len);
}

extern "C" int kd_mpi_sys_mmz_alloc(uint64_t *phy_addr, void **virt_addr,
                                     const char *mmb, const char *zone,
                                     uint32_t len) {
    return kd_mpi_sys_mmz_alloc_cached(phy_addr, virt_addr, mmb, zone, len);
}

extern "C" int kd_mpi_sys_mmz_alloc_cached(uint64_t *phy_addr, void **virt_addr,
                                            const char *mmb, const char *zone,
                                            uint32_t len) {
    (void)mmb;
    (void)zone;
    if (!phy_addr || !virt_addr || len == 0) {
        return -1;
    }
    uint8_t *ddr = ensure_ddr_map();
    if (!ddr) {
        return -1;
    }
    const size_t offset = align_up(g_ddr_bump, kPageSize);
    const size_t size = align_up(len, kPageSize);
    if (offset + size > KPU_RUNTIME_DDR_SIZE) {
        return -1;
    }
    for (auto &allocation : g_allocations) {
        if (!allocation.live) {
            allocation.paddr = KPU_RUNTIME_DDR_PADDR + offset;
            allocation.vaddr = ddr + offset;
            allocation.size = size;
            allocation.live = true;
            *phy_addr = allocation.paddr;
            *virt_addr = allocation.vaddr;
            g_ddr_bump = offset + size;
            ++g_mmz_alloc_count;
            memset(*virt_addr, 0, size);
            return 0;
        }
    }
    return -1;
}

extern "C" int kd_mpi_sys_mmz_flush_cache(uint64_t phy_addr, void *virt_addr,
                                           uint32_t size) {
    (void)phy_addr;
    (void)virt_addr;
    (void)size;
    return 0;
}

extern "C" int kd_mpi_sys_mmz_free(uint64_t phy_addr, void *virt_addr) {
    for (auto &allocation : g_allocations) {
        if (allocation.live &&
            (allocation.paddr == phy_addr || allocation.vaddr == virt_addr)) {
            allocation.live = false;
            return 0;
        }
    }
    return 0;
}

extern "C" void *kd_mpi_sys_mmap(uint64_t phy_addr, uint32_t size) {
    return kd_mpi_sys_mmap_cached(phy_addr, size);
}

extern "C" void *kd_mpi_sys_mmap_cached(uint64_t phy_addr, uint32_t size) {
    return map_window(phy_addr, size, PROT_READ | PROT_WRITE, MAP_SHARED);
}

extern "C" int kd_mpi_sys_munmap(void *virt_addr, uint32_t size) {
    return __wrap_munmap(virt_addr, size);
}

extern "C" int kd_mpi_sys_get_virmem_info(const void *virt_addr,
                                           k_sys_virmem_info *mem_info) {
    if (!mem_info) {
        return -1;
    }
    uint64_t paddr = 0;
    if (!translate_vaddr(virt_addr, 1, &paddr)) {
        return -1;
    }
    mem_info->phy_addr = paddr;
    mem_info->cached = 1;
    return 0;
}
