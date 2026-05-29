#ifndef K230_SDK_COMPAT_H
#define K230_SDK_COMPAT_H

#include <stddef.h>
#include <stdint.h>

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
