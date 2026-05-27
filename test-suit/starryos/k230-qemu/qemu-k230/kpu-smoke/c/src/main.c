#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#define KPU_IOC_GET_STATUS 0x4b00
#define KPU_IOC_CLEAR 0x4b01

#define KPU_MMAP_CFG_OFFSET 0
#define KPU_MMAP_L2_OFFSET 0x1000
#define KPU_STATUS_LO 0x130
#define KPU_STATUS_HI 0x134

static int fail_errno(const char *what)
{
    printf("KPU_SMOKE_FAIL: %s: %s\n", what, strerror(errno));
    return 1;
}

static int fail_msg(const char *what)
{
    printf("KPU_SMOKE_FAIL: %s\n", what);
    return 1;
}

static int check_device_node(const char *path)
{
    int fd = open(path, O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        return fail_errno(path);
    }
    close(fd);
    printf("KPU_SMOKE: opened %s\n", path);
    return 0;
}

static int check_pread_reg(int fd)
{
    uint32_t value = 0;
    ssize_t nread = pread(fd, &value, sizeof(value), 0);
    if (nread < 0) {
        return fail_errno("pread /dev/kpu");
    }
    if (nread != (ssize_t)sizeof(value)) {
        return fail_msg("pread /dev/kpu returned a short register value");
    }
    printf("KPU_SMOKE: reg0=0x%08x\n", value);
    return 0;
}

static int check_status_ioctl(int fd)
{
    uint64_t status = 0;
    if (ioctl(fd, KPU_IOC_GET_STATUS, &status) != 0) {
        return fail_errno("KPU_IOC_GET_STATUS");
    }
    printf("KPU_SMOKE: status=0x%016llx\n", (unsigned long long)status);

    if (ioctl(fd, KPU_IOC_CLEAR, 0) != 0) {
        return fail_errno("KPU_IOC_CLEAR");
    }
    printf("KPU_SMOKE: clear_done ok\n");
    return 0;
}

static int check_cfg_mmap(int fd)
{
    volatile uint32_t *cfg = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd,
                                  KPU_MMAP_CFG_OFFSET);
    if (cfg == MAP_FAILED) {
        return fail_errno("mmap KPU CFG");
    }

    uint64_t status = ((uint64_t)cfg[KPU_STATUS_HI / sizeof(uint32_t)] << 32) |
                      cfg[KPU_STATUS_LO / sizeof(uint32_t)];
    printf("KPU_SMOKE: mmap_status=0x%016llx\n", (unsigned long long)status);

    if (munmap((void *)cfg, 4096) != 0) {
        return fail_errno("munmap KPU CFG");
    }
    return 0;
}

static int check_l2_mmap(int fd)
{
    volatile uint32_t *l2 = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd,
                                 KPU_MMAP_L2_OFFSET);
    if (l2 == MAP_FAILED) {
        return fail_errno("mmap KPU L2");
    }

    const uint32_t marker = 0x4b505532;
    l2[0] = marker;
    if (l2[0] != marker) {
        munmap((void *)l2, 4096);
        return fail_msg("KPU L2 mmap readback mismatch");
    }
    printf("KPU_SMOKE: l2_mmap_rw=0x%08x\n", marker);

    if (munmap((void *)l2, 4096) != 0) {
        return fail_errno("munmap KPU L2");
    }
    return 0;
}

int main(void)
{
    if (check_device_node("/dev/kpu") != 0 || check_device_node("/dev/kpu0") != 0) {
        return 1;
    }

    int fd = open("/dev/kpu", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        return fail_errno("open /dev/kpu");
    }

    int failed = check_pread_reg(fd) != 0 || check_status_ioctl(fd) != 0 ||
                 check_cfg_mmap(fd) != 0 || check_l2_mmap(fd) != 0;
    close(fd);
    if (failed) {
        return 1;
    }

    printf("KPU_SMOKE_PASS\n");
    return 0;
}
