// xattr 系统调用测试 — StarryOS
//
// 背景: pip uninstall 跨文件系统移动文件 (ext4 → tmpfs) 时,
// shutil.copy2() → copystat() → _copyxattr() 会调用
// listxattr / getxattr / setxattr, 因此内核必须处理这些调用,
// 即使 rsext4 不支持扩展属性。
//
// 测试策略:
//   1. listxattr → 返回 0 (空列表)
//   2. getxattr → 返回 -1, errno=ENODATA
//   3. setxattr → EOPNOTSUPP (StarryOS) 或 0 (Linux ext4)
//   4. removexattr → EOPNOTSUPP (StarryOS) 或 0 (Linux ext4)
//   5. fd 变体: flistxattr / fgetxattr / fsetxattr / fremovexattr
//   6. 跨文件系统复制模拟 (pip uninstall 场景)
//   7. 一致性: list→get/set 模式 (防止只更新部分 stub)

#define _GNU_SOURCE
#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <sys/types.h>
#include <sys/xattr.h>
#include <unistd.h>

#define TEST_FILE "/root/test-xattr-file.txt"

// 探测文件系统是否支持扩展属性。
// Linux ext4: setxattr 返回 0; StarryOS rsext4: 返回 EOPNOTSUPP。
// TODO: xattr stub — rsext4 没有扩展属性, 未完全实现。
static int probe_xattr_support(const char *path) {
    int rc = setxattr(path, "user.probe", "x", 1, 0);
    if (rc == 0) {
        removexattr(path, "user.probe");
        return 1;  // 支持 xattr
    }
    return 0;  // 不支持 xattr (EOPNOTSUPP)
}

int main(void) {
    TEST_START("xattr syscalls");

    // 在 ext4 (/root) 上创建测试文件
    int fd = open(TEST_FILE, O_CREAT | O_WRONLY, 0644);
    CHECK(fd >= 0, "创建测试文件");
    if (fd < 0) {
        TEST_DONE();
    }
    write(fd, "xattr test\n", 11);
    close(fd);

    int xattr_supported = probe_xattr_support(TEST_FILE);

    // ============================================================
    // 1. listxattr — 路径方式
    // ============================================================
    // TODO: xattr stub — rsext4 没有扩展属性, 始终返回 0。
    CHECK_RET(listxattr(TEST_FILE, NULL, 0), 0,
              "listxattr(NULL, 0) 返回 0");

    {
        char buf[256];
        memset(buf, 0x42, sizeof(buf));
        // TODO: xattr stub — rsext4 没有扩展属性, 返回 0。
        ssize_t sz = listxattr(TEST_FILE, buf, sizeof(buf));
        CHECK_RET(sz, 0, "listxattr(buf, 256) 返回 0");
        // 验证缓冲区未被修改 (没有写入任何属性名)
        CHECK(buf[0] == 0x42, "listxattr 未修改缓冲区 (无属性)");
    }

    // ============================================================
    // 2. listxattr — fd 方式 (flistxattr)
    // ============================================================
    // TODO: xattr stub — rsext4 没有扩展属性, 始终返回 0。
    fd = open(TEST_FILE, O_RDONLY);
    CHECK(fd >= 0, "打开文件用于 flistxattr");
    if (fd >= 0) {
        CHECK_RET(flistxattr(fd, NULL, 0), 0,
                  "flistxattr(NULL, 0) 返回 0");
        close(fd);
    }

    // ============================================================
    // 3. getxattr — 路径方式
    // ============================================================
    // TODO: xattr stub — rsext4 没有扩展属性, 返回 ENODATA。
    {
        char buf[256];
        ssize_t sz = getxattr(TEST_FILE, "user.test", buf, sizeof(buf));
        CHECK_RET(sz, -1, "getxattr 返回 -1");
        CHECK_ERR(getxattr(TEST_FILE, "user.test", buf, sizeof(buf)),
                  ENODATA, "getxattr errno=ENODATA");
    }

    // ============================================================
    // 4. getxattr — fd 方式 (fgetxattr)
    // ============================================================
    // TODO: xattr stub — rsext4 没有扩展属性, 返回 ENODATA。
    fd = open(TEST_FILE, O_RDONLY);
    CHECK(fd >= 0, "打开文件用于 fgetxattr");
    if (fd >= 0) {
        char buf[256];
        CHECK_ERR(fgetxattr(fd, "user.test", buf, sizeof(buf)),
                  ENODATA, "fgetxattr errno=ENODATA");
        close(fd);
    }

    // ============================================================
    // 5. setxattr — 路径方式
    // ============================================================
    // TODO: xattr stub — rsext4 没有扩展属性, 返回 EOPNOTSUPP。
    if (xattr_supported) {
        CHECK_RET(setxattr(TEST_FILE, "user.test", "value", 5, 0), 0,
                  "setxattr 返回 0 (支持 xattr)");
    } else {
        CHECK_ERR(setxattr(TEST_FILE, "user.test", "value", 5, 0),
                  EOPNOTSUPP, "setxattr errno=EOPNOTSUPP");
    }

    // ============================================================
    // 6. setxattr — fd 方式 (fsetxattr)
    // ============================================================
    // TODO: xattr stub — rsext4 没有扩展属性, 返回 EOPNOTSUPP。
    fd = open(TEST_FILE, O_RDONLY);
    CHECK(fd >= 0, "打开文件用于 fsetxattr");
    if (fd >= 0) {
        if (xattr_supported) {
            CHECK_RET(fsetxattr(fd, "user.test", "value", 5, 0), 0,
                      "fsetxattr 返回 0 (支持 xattr)");
        } else {
            CHECK_ERR(fsetxattr(fd, "user.test", "value", 5, 0),
                      EOPNOTSUPP, "fsetxattr errno=EOPNOTSUPP");
        }
        close(fd);
    }

    // ============================================================
    // 7. removexattr — 路径方式
    // ============================================================
    // TODO: xattr stub — rsext4 没有扩展属性, 返回 EOPNOTSUPP。
    if (xattr_supported) {
        CHECK_RET(removexattr(TEST_FILE, "user.test"), 0,
                  "removexattr 返回 0 (支持 xattr)");
    } else {
        CHECK_ERR(removexattr(TEST_FILE, "user.test"),
                  EOPNOTSUPP, "removexattr errno=EOPNOTSUPP");
    }

    // ============================================================
    // 8. removexattr — fd 方式 (fremovexattr)
    // ============================================================
    // TODO: xattr stub — rsext4 没有扩展属性, 返回 EOPNOTSUPP。
    fd = open(TEST_FILE, O_RDONLY);
    CHECK(fd >= 0, "打开文件用于 fremovexattr");
    if (fd >= 0) {
        if (xattr_supported) {
            CHECK_RET(fremovexattr(fd, "user.test"), 0,
                      "fremovexattr 返回 0 (支持 xattr)");
        } else {
            CHECK_ERR(fremovexattr(fd, "user.test"),
                      EOPNOTSUPP, "fremovexattr errno=EOPNOTSUPP");
        }
        close(fd);
    }

    // ============================================================
    // 9. 跨文件系统复制模拟 (pip uninstall 场景)
    //
    // shutil._copyxattr() 的模式:
    //   names = listxattr(src)  → 返回 0 (空)
    //   for name in names:      → 循环体不执行
    //       getxattr(src, name)
    //       setxattr(dst, name, value)
    //
    // 安全不变量: listxattr 返回 0 → 循环体不执行 →
    //            setxattr 不被调用 → EOPNOTSUPP 不会被触发。
    // ============================================================
    // TODO: xattr stub — rsext4 没有扩展属性。
    {
        const char *dst = "/tmp/test-xattr-dst.txt";
        int dfd = open(dst, O_CREAT | O_WRONLY, 0644);
        if (dfd < 0) {
            // /tmp 未挂载或其他错误
            CHECK(0, "在 /tmp 上创建目标文件 (tmpfs 未挂载则跳过)");
        } else {
            close(dfd);

            // 步骤 1: listxattr(src) → 必须返回 0
            char names[256];
            ssize_t nsz = listxattr(TEST_FILE, names, sizeof(names));
            CHECK_RET(nsz, 0, "跨 fs: listxattr(src) 返回 0");

            // 步骤 2: xattr 复制循环 (nsz == 0, 循环体跳过)
            int copy_ok = 1;
            ssize_t off = 0;
            while (off < nsz) {
                const char *name = names + off;
                ssize_t nlen = strlen(name);
                if (nlen == 0) break;

                char val[4096];
                ssize_t vsz = getxattr(TEST_FILE, name, val, sizeof(val));
                if (vsz < 0) {
                    copy_ok = 0;
                    break;
                }
                if (setxattr(dst, name, val, vsz, 0) < 0) {
                    copy_ok = 0;
                    break;
                }
                off += nlen + 1;
            }
            CHECK(copy_ok, "跨 fs: xattr 复制循环成功 (循环体跳过)");

            // 步骤 3: 验证目标无属性
            ssize_t dsz = listxattr(dst, NULL, 0);
            CHECK_RET(dsz, 0, "跨 fs: dst listxattr == 0");

            unlink(dst);
        }
    }

    // ============================================================
    // 10. 一致性测试: list → get/set 模式
    //
    // 确保 xattr stub 保持一致。如果有人将 listxattr 改为返回实际
    // 属性名, 则必须同时更新:
    //   - getxattr 返回实际值 (不是 ENODATA)
    //   - setxattr 实际写入 (不是 EOPNOTSUPP)
    //   - removexattr 实际删除 (不是 EOPNOTSUPP)
    //
    // 当前 stub: listxattr 返回 0, 步骤 b/c 被跳过。
    // 若 listxattr 被更新但 getxattr/setxattr 未更新, 测试仍会通过
    // (步骤 b/c 执行但优雅失败)。CI 审查时应检查全部 12 个 stub。
    // ============================================================
    // TODO: xattr stub — rsext4 没有扩展属性。
    {
        char names[256];
        ssize_t nsz = listxattr(TEST_FILE, names, sizeof(names));
        CHECK_RET(nsz, 0, "一致性: listxattr 返回 0");

        // 若 listxattr 未来返回 > 0, 以下必须同时工作:
        if (nsz > 0) {
            // 遍历 null 结尾的属性名列表
            ssize_t off = 0;
            while (off < nsz) {
                const char *name = names + off;
                ssize_t nlen = strlen(name);
                if (nlen == 0) break;

                // getxattr 必须返回值, 而非 ENODATA
                char val[4096];
                ssize_t vsz = getxattr(TEST_FILE, name, val, sizeof(val));
                CHECK(vsz >= 0,
                      "一致性: getxattr 必须在 listxattr 返回属性名时成功");

                // setxattr 必须成功, 而非 EOPNOTSUPP
                int rc = setxattr(TEST_FILE, name, val, vsz, 0);
                CHECK(rc == 0,
                      "一致性: setxattr 必须在 listxattr 返回属性名时成功");

                off += nlen + 1;
            }
        }
    }

    unlink(TEST_FILE);

    TEST_DONE();
}
