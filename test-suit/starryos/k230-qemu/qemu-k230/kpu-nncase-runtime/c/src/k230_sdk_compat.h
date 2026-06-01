#ifndef K230_SDK_COMPAT_H
#define K230_SDK_COMPAT_H

#include <stddef.h>
#include <stdint.h>

#define KPU_DEVICE_PATH "/dev/kpu"
#define KPU_DEVICE_ALIAS_PATH "/dev/kpu0"

#define KPU_IOC_GET_STATUS 0x4b00u
#define KPU_IOC_CLEAR 0x4b01u
#define KPU_IOC_RUN 0x4b04u
#define KPU_IOC_WAIT_DONE 0x4b05u

#define KPU_MMAP_CFG_OFFSET 0x0ull
#define KPU_MMAP_L2_OFFSET 0x1000ull
#define KPU_MMAP_FAKE_OUTPUT_OFFSET 0x2000ull
#define KPU_MMAP_RUNTIME_RDATA_OFFSET 0x3000ull
#define KPU_MMAP_RUNTIME_COMMAND_OFFSET 0x4000ull
#define KPU_MMAP_RUNTIME_DIRECT_IO_OFFSET 0x5000ull
#define KPU_MMAP_RUNTIME_DDR_OFFSET 0x6000ull

#define KPU_CFG_PADDR 0x80400000ull
#define KPU_L2_PADDR 0x80000000ull
#define KPU_FAKE_OUTPUT_PADDR 0x10090000ull
#define KPU_RUNTIME_RDATA_PADDR 0x10000000ull
#define KPU_RUNTIME_COMMAND_PADDR 0x10190000ull
#define KPU_RUNTIME_DIRECT_IO_PADDR 0x10500000ull
#define KPU_RUNTIME_DDR_PADDR 0x3c000000ull

#define KPU_RUNTIME_RDATA_BASE 0x10000020ull
#define KPU_RUNTIME_ARG_TABLE_PADDR 0x80000000ull
#define KPU_RUNTIME_DIRECT_SOURCE_PADDR 0x10500020ull

#define KPU_CFG_SIZE 0x800u
#define KPU_L2_SIZE 0x200000u
#define KPU_FAKE_OUTPUT_SIZE 0x100000u
#define KPU_RUNTIME_RDATA_SIZE 0x90000u
#define KPU_RUNTIME_COMMAND_SIZE 0x370000u
#define KPU_RUNTIME_DIRECT_IO_SIZE 0xb00000u
#define KPU_RUNTIME_DDR_SIZE 0x4000000u

#define KPU_DONE_STATUS 0x0000000400000004ull

struct k230_kpu_command_range {
    uint64_t start_paddr;
    uint64_t end_paddr;
};

#ifdef __cplusplus
extern "C" {
#endif

int k230_compat_init(void);
void k230_compat_dump_stats(void);

int kd_mpi_sys_mmz_alloc(uint64_t *phy_addr, void **virt_addr, const char *mmb,
                         const char *zone, uint32_t len);
int kd_mpi_sys_mmz_alloc_cached(uint64_t *phy_addr, void **virt_addr,
                                const char *mmb, const char *zone,
                                uint32_t len);
int kd_mpi_sys_mmz_flush_cache(uint64_t phy_addr, void *virt_addr,
                               uint32_t size);
int kd_mpi_sys_mmz_free(uint64_t phy_addr, void *virt_addr);
void *kd_mpi_sys_mmap(uint64_t phy_addr, uint32_t size);
void *kd_mpi_sys_mmap_cached(uint64_t phy_addr, uint32_t size);
int kd_mpi_sys_munmap(void *virt_addr, uint32_t size);

typedef struct {
    uint64_t phy_addr;
    int cached;
} k_sys_virmem_info;

int kd_mpi_sys_get_virmem_info(const void *virt_addr,
                               k_sys_virmem_info *mem_info);

#ifdef __cplusplus
}
#endif

#endif
