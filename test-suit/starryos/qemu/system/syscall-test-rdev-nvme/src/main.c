/*
 * test-rdev-nvme — root block device node /dev/nvme0n1.
 *
 * starry has no real block backend for the root mount, but tools that resolve
 * the root device by scanning /dev (notably busybox `rdev`, which stats "/",
 * takes its st_dev, then looks for a block node in /dev whose st_rdev matches)
 * need such a node to exist. The kernel exposes /dev/nvme0n1 as a placeholder
 * block device whose rdev equals the root filesystem's st_dev. Real I/O is
 * unsupported (read/write return EIO) so it never masquerades as a working
 * disk.
 *
 * This is the kernel-side regression for busybox_rdev printing
 * `/dev/nvme0n1 /`.
 */

#include "test_framework.h"

#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>

int main(void)
{
    TEST_START("rdev: /dev/nvme0n1 root block device");

    struct stat root_st, nvme_st;
    CHECK(stat("/", &root_st) == 0, "stat / (root mount)");

    int have_nvme = (stat("/dev/nvme0n1", &nvme_st) == 0);
    CHECK(have_nvme, "stat /dev/nvme0n1 (root block device node exists)");
    if (have_nvme) {
        CHECK(S_ISBLK(nvme_st.st_mode), "/dev/nvme0n1 is a block device (S_ISBLK)");
        CHECK(nvme_st.st_rdev == root_st.st_dev,
              "/dev/nvme0n1 st_rdev == root filesystem st_dev (busybox rdev resolves \"/\" -> /dev/nvme0n1)");

        /* RootBlk returns EIO on real I/O — it is a resolver placeholder, not a
         * working disk; it must not silently succeed for dd/blkid/fsck. */
        int fd = open("/dev/nvme0n1", O_RDONLY);
        CHECK(fd >= 0, "open /dev/nvme0n1 O_RDONLY");
        if (fd >= 0) {
            char buf[16];
            errno = 0;
            ssize_t n = read(fd, buf, sizeof buf);
            CHECK(n < 0 && errno == EIO,
                  "read /dev/nvme0n1 returns EIO (placeholder, no fake disk I/O)");
            close(fd);
        }
    }

    TEST_DONE();
}
