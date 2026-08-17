#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_setxattrat
#define SYS_setxattrat 463
#endif

#ifndef SYS_getxattrat
#define SYS_getxattrat 464
#endif

#ifndef AT_EMPTY_PATH
#define AT_EMPTY_PATH 0x1000
#endif

#define INVALID_AT_FLAG 0x80000000u
#define POSIX_ACL_XATTR_VERSION 2u
#define ACL_USER_OBJ 0x01u
#define ACL_USER 0x02u
#define ACL_GROUP_OBJ 0x04u
#define ACL_MASK 0x10u
#define ACL_OTHER 0x20u
#define ACL_UNDEFINED_ID UINT32_MAX

struct xattr_args {
    uint64_t value;
    uint32_t size;
    uint32_t flags;
};

struct posix_acl_xattr_entry {
    uint16_t tag;
    uint16_t permissions;
    uint32_t id;
};

struct posix_acl_xattr {
    uint32_t version;
    struct posix_acl_xattr_entry entries[5];
};

static int failures;

static void check(int condition, const char *stage)
{
    if (condition) {
        puts(stage);
        return;
    }

    puts(stage);
    failures++;
}

static long raw_setxattrat(
    int dirfd,
    const char *path,
    unsigned int at_flags,
    const char *name,
    const struct xattr_args *args,
    size_t args_size
)
{
    return syscall(SYS_setxattrat, dirfd, path, at_flags, name, args, args_size);
}

static long raw_getxattrat(
    int dirfd,
    const char *path,
    unsigned int at_flags,
    const char *name,
    const struct xattr_args *args,
    size_t args_size
)
{
    return syscall(SYS_getxattrat, dirfd, path, at_flags, name, args, args_size);
}

static void expect_errno(
    const char *stage,
    long result,
    int expected_errno
)
{
    check(result == -1 && errno == expected_errno, stage);
}

int main(void)
{
    static const char fixture_dir[] = "/tmp/starry-xattrat";
    static const char fixture_name[] = "file";
    static const char attr_name[] = "user.starry";
    static const char attr_value[] = "nixos";
    static const char acl_name[] = "system.posix_acl_access";
    static const char default_acl_name[] = "system.posix_acl_default";
    static const struct posix_acl_xattr acl = {
        .version = POSIX_ACL_XATTR_VERSION,
        .entries = {
            {ACL_USER_OBJ, 6, ACL_UNDEFINED_ID},
            {ACL_USER, 4, 123},
            {ACL_GROUP_OBJ, 4, ACL_UNDEFINED_ID},
            {ACL_MASK, 4, ACL_UNDEFINED_ID},
            {ACL_OTHER, 0, ACL_UNDEFINED_ID},
        },
    };
    char value_buf[sizeof(attr_value)] = {};
    struct xattr_args args = {
        .value = (uintptr_t)attr_value,
        .size = sizeof(attr_value) - 1,
        .flags = 0,
    };
    int dirfd = -1;
    int fd = -1;

    setvbuf(stdout, NULL, _IONBF, 0);
    puts("STARRY_SYSTEM_TEST_BEGIN: bugfix-xattrat-path-resolution");

    mkdir(fixture_dir, 0700);
    dirfd = openat(
        AT_FDCWD,
        fixture_dir,
        O_RDONLY | O_DIRECTORY | O_CLOEXEC,
        0
    );
    check(dirfd >= 0, "open fixture directory");
    if (dirfd < 0) {
        goto out;
    }

    fd = openat(dirfd, fixture_name, O_CREAT | O_RDWR | O_TRUNC | O_CLOEXEC, 0600);
    check(fd >= 0, "create fixture file");
    if (fd < 0) {
        goto out;
    }

    errno = 0;
    check(
        raw_setxattrat(
            dirfd,
            fixture_name,
            0,
            attr_name,
            &args,
            sizeof(args)
        ) == 0,
        "setxattrat resolves a path relative to dirfd"
    );

    args.value = (uintptr_t)value_buf;
    args.size = sizeof(value_buf);
    errno = 0;
    check(
        raw_getxattrat(
            dirfd,
            fixture_name,
            0,
            attr_name,
            &args,
            sizeof(args)
        ) == (long)(sizeof(attr_value) - 1) &&
            memcmp(value_buf, attr_value, sizeof(attr_value) - 1) == 0,
        "getxattrat returns the exact relative-path value"
    );

    args.value = 0;
    args.size = 0;
    errno = 0;
    check(
        raw_getxattrat(
            dirfd,
            fixture_name,
            0,
            attr_name,
            &args,
            sizeof(args)
        ) == (long)(sizeof(attr_value) - 1),
        "getxattrat supports a size query"
    );

    args.value = (uintptr_t)value_buf;
    args.size = sizeof(value_buf);
    errno = 0;
    check(
        raw_getxattrat(
            fd,
            "",
            AT_EMPTY_PATH,
            attr_name,
            &args,
            sizeof(args)
        ) == (long)(sizeof(attr_value) - 1),
        "getxattrat supports AT_EMPTY_PATH on an fd"
    );

    args.value = (uintptr_t)&acl;
    args.size = sizeof(acl);
    errno = 0;
    expect_errno(
        "setxattrat rejects unsupported POSIX access ACLs",
        raw_setxattrat(
            dirfd,
            fixture_name,
            0,
            acl_name,
            &args,
            sizeof(args)
        ),
        EOPNOTSUPP
    );

    errno = 0;
    expect_errno(
        "setxattrat rejects default ACLs on non-directories",
        raw_setxattrat(
            dirfd,
            fixture_name,
            0,
            default_acl_name,
            &args,
            sizeof(args)
        ),
        EOPNOTSUPP
    );

    errno = 0;
    expect_errno(
        "setxattrat rejects unsupported POSIX default ACLs on directories",
        raw_setxattrat(
            dirfd,
            ".",
            0,
            default_acl_name,
            &args,
            sizeof(args)
        ),
        EOPNOTSUPP
    );

    errno = 0;
    expect_errno(
        "setxattrat rejects unknown path flags with EINVAL",
        raw_setxattrat(
            dirfd,
            fixture_name,
            INVALID_AT_FLAG,
            attr_name,
            &args,
            sizeof(args)
        ),
        EINVAL
    );

    errno = 0;
    expect_errno(
        "getxattrat rejects an undersized xattr_args with EINVAL",
        raw_getxattrat(
            dirfd,
            fixture_name,
            0,
            attr_name,
            &args,
            sizeof(args) - 1
        ),
        EINVAL
    );

out:
    if (fd >= 0) {
        close(fd);
    }
    if (dirfd >= 0) {
        unlinkat(dirfd, fixture_name, 0);
        close(dirfd);
    }
    rmdir(fixture_dir);

    if (failures == 0) {
        puts("STARRY_SYSTEM_TEST_PASSED: bugfix-xattrat-path-resolution");
        return EXIT_SUCCESS;
    }

    puts("STARRY_SYSTEM_TEST_FAILED: bugfix-xattrat-path-resolution");
    return EXIT_FAILURE;
}
