#define _GNU_SOURCE
#include "test_framework.h"

#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/vfs.h>
#include <unistd.h>

#ifndef EXT4_SUPER_MAGIC
#define EXT4_SUPER_MAGIC 0xEF53
#endif

#ifndef EBADR
#define EBADR 53
#endif

#define FIEMAP_FLAG_SYNC 0x00000001U
#define FIEMAP_FLAG_XATTR 0x00000002U
#define FIEMAP_EXTENT_LAST 0x00000001U
#define FIEMAP_EXTENT_UNWRITTEN 0x00000800U

struct test_fiemap_extent {
    uint64_t fe_logical;
    uint64_t fe_physical;
    uint64_t fe_length;
    uint64_t fe_reserved64[2];
    uint32_t fe_flags;
    uint32_t fe_reserved[3];
};

struct test_fiemap {
    uint64_t fm_start;
    uint64_t fm_length;
    uint32_t fm_flags;
    uint32_t fm_mapped_extents;
    uint32_t fm_extent_count;
    uint32_t fm_reserved;
};

#define FS_IOC_FIEMAP _IOWR('f', 11, struct test_fiemap)

_Static_assert(sizeof(struct test_fiemap) == 32, "fiemap header ABI size");
_Static_assert(sizeof(struct test_fiemap_extent) == 56,
               "fiemap extent ABI size");

struct fiemap_buffer {
    struct test_fiemap header;
    struct test_fiemap_extent extents[8];
};

static void reset_fiemap(struct fiemap_buffer *map, uint64_t start,
                         uint64_t length, uint32_t flags, uint32_t count)
{
    memset(map, 0, sizeof(*map));
    map->header.fm_start = start;
    map->header.fm_length = length;
    map->header.fm_flags = flags;
    map->header.fm_extent_count = count;
}

int main(void)
{
    TEST_START("FS_IOC_FIEMAP");

    struct statfs rootfs;
    CHECK_RET(statfs("/root", &rootfs), 0, "statfs /root 应成功");
    CHECK((unsigned long)rootfs.f_type == EXT4_SUPER_MAGIC,
          "FIEMAP 持久化测例必须运行在 ext4 上");

    char path[] = "/root/test-fiemap-XXXXXX";
    int fd = mkstemp(path);
    CHECK(fd >= 0, "mkstemp 应成功");
    unsigned char block[4096];
    memset(block, 0x11, sizeof(block));
    CHECK_RET(write(fd, block, sizeof(block)), (ssize_t)sizeof(block),
              "写入 logical block 0");
    CHECK_RET(lseek(fd, 8192, SEEK_SET), 8192, "跳过一个 sparse block");
    memset(block, 0x22, sizeof(block));
    CHECK_RET(write(fd, block, sizeof(block)), (ssize_t)sizeof(block),
              "写入 logical block 2");
    CHECK_RET(fallocate(fd, 0, 16384, 4096), 0,
              "创建 logical block 4 unwritten extent");

    struct fiemap_buffer map;
    reset_fiemap(&map, 0, UINT64_MAX, FIEMAP_FLAG_SYNC, 0);
    CHECK_RET(ioctl(fd, FS_IOC_FIEMAP, &map.header), 0,
              "extent_count=0 计数模式成功");
    CHECK(map.header.fm_mapped_extents == 3,
          "计数模式报告两个 initialized 和一个 unwritten extent");

    reset_fiemap(&map, 0, UINT64_MAX, FIEMAP_FLAG_SYNC, 8);
    CHECK_RET(ioctl(fd, FS_IOC_FIEMAP, &map.header), 0,
              "完整 FIEMAP 查询成功");
    CHECK(map.header.fm_mapped_extents == 3, "完整查询返回三个 extent");
    CHECK(map.extents[0].fe_logical == 0, "第一个 extent logical offset 为 0");
    CHECK(map.extents[1].fe_logical == 8192,
          "sparse hole 不生成 extent，第二个 mapping 从 8192 开始");
    CHECK(map.extents[2].fe_logical == 16384,
          "第三个 unwritten extent 从 16384 开始");
    CHECK(map.extents[0].fe_physical != 0 && map.extents[1].fe_physical != 0,
          "initialized extent 返回真实 device byte offset");
    CHECK((map.extents[2].fe_flags & FIEMAP_EXTENT_UNWRITTEN) != 0,
          "预分配 extent 带 UNWRITTEN flag");
    CHECK((map.extents[2].fe_flags & FIEMAP_EXTENT_LAST) != 0,
          "最终 extent 带 LAST flag");

    reset_fiemap(&map, 0, UINT64_MAX, FIEMAP_FLAG_SYNC, 2);
    CHECK_RET(ioctl(fd, FS_IOC_FIEMAP, &map.header), 0,
              "有界 FIEMAP 查询成功");
    CHECK(map.header.fm_mapped_extents == 2, "extent buffer 满时只返回两项");
    CHECK((map.extents[1].fe_flags & FIEMAP_EXTENT_LAST) == 0,
          "截断结果不能伪造 LAST flag");

    reset_fiemap(&map, 2048, 8192, FIEMAP_FLAG_SYNC, 8);
    CHECK_RET(ioctl(fd, FS_IOC_FIEMAP, &map.header), 0,
              "非块对齐查询成功");
    CHECK(map.header.fm_mapped_extents == 2, "裁剪范围仍跳过 sparse hole");
    CHECK(map.extents[0].fe_logical == 0 && map.extents[0].fe_length == 4096,
          "FIEMAP 保留与查询范围相交的完整首个 extent");
    CHECK(map.extents[1].fe_logical == 8192 && map.extents[1].fe_length == 4096,
          "FIEMAP 保留与查询范围相交的完整末个 extent");
    CHECK((map.extents[1].fe_flags & FIEMAP_EXTENT_LAST) != 0,
          "查询范围内最后一个 mapping 带 LAST");

    reset_fiemap(&map, 0, 0, 0, 1);
    CHECK_ERR(ioctl(fd, FS_IOC_FIEMAP, &map.header), EINVAL,
              "zero-length FIEMAP 返回 EINVAL");
    reset_fiemap(&map, 0, 0, FIEMAP_FLAG_XATTR, 1);
    CHECK_ERR(ioctl(fd, FS_IOC_FIEMAP, &map.header), EINVAL,
              "范围校验优先于 XATTR mapping 检查");
    reset_fiemap(&map, 0, UINT64_MAX, FIEMAP_FLAG_XATTR, 8);
    CHECK_RET(ioctl(fd, FS_IOC_FIEMAP, &map.header), 0,
              "无磁盘 xattr 的 FIEMAP XATTR 查询成功");
    CHECK(map.header.fm_mapped_extents == 0,
          "无磁盘 xattr 的 FIEMAP XATTR 返回空 mapping");

    reset_fiemap(&map, 0, UINT64_MAX, 0x80000000U, 1);
    CHECK_ERR(ioctl(fd, FS_IOC_FIEMAP, &map.header), EBADR,
              "unknown FIEMAP flag 返回 EBADR");
    CHECK(map.header.fm_flags == 0x80000000U,
          "失败回写只保留 incompatible flags");

    reset_fiemap(&map, 0, UINT64_MAX, 0, UINT_MAX);
    CHECK_ERR(ioctl(fd, FS_IOC_FIEMAP, &map.header), EINVAL,
              "过大的 extent_count 返回 EINVAL");
    CHECK_ERR(ioctl(fd, FS_IOC_FIEMAP, (void *)1), EFAULT,
              "无效用户指针返回 EFAULT");

    int dirfd = open("/root", O_RDONLY | O_DIRECTORY);
    CHECK(dirfd >= 0, "打开 ext4 目录成功");
    reset_fiemap(&map, 0, UINT64_MAX, FIEMAP_FLAG_SYNC, 8);
    CHECK_RET(ioctl(dirfd, FS_IOC_FIEMAP, &map.header), 0,
              "ext4 目录复用 FIEMAP inode operation");
    CHECK(map.header.fm_mapped_extents > 0, "ext4 目录返回 data mapping");
    if (map.header.fm_mapped_extents > 0) {
        CHECK((map.extents[map.header.fm_mapped_extents - 1].fe_flags &
               FIEMAP_EXTENT_LAST) != 0,
              "完整目录 FIEMAP 的最后一项带 LAST");
    }

    close(dirfd);
    close(fd);
    unlink(path);
    TEST_DONE();
}
