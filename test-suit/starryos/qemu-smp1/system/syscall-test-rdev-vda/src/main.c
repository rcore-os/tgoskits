/*
 * test-rdev-vda — root block device node /dev/vda.
 *
 * starry has no real block backend for the root mount, but tools that resolve
 * the root device by scanning /dev (notably busybox `rdev`, which stats "/",
 * takes its st_dev, then looks for a block node in /dev whose st_rdev matches)
 * need such a node to exist. The kernel exposes /dev/vda as a placeholder block
 * device whose rdev equals the root filesystem's st_dev. Real I/O is
 * unsupported (read/write return EIO) so it never masquerades as a working disk.
 *
 * This is the kernel-side regression for busybox_rdev printing `/dev/vda /`.
 */

#include "test_framework.h"

#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>

int main(void)
{
    TEST_START("rdev: /dev/vda root block device");

    struct stat root_st, vda_st;
    CHECK(stat("/", &root_st) == 0, "stat / (root mount)");

    int have_vda = (stat("/dev/vda", &vda_st) == 0);
    CHECK(have_vda, "stat /dev/vda (root block device node exists)");
    if (have_vda) {
        CHECK(S_ISBLK(vda_st.st_mode), "/dev/vda is a block device (S_ISBLK)");
        CHECK(vda_st.st_rdev == root_st.st_dev,
              "/dev/vda st_rdev == root filesystem st_dev (busybox rdev resolves \"/\" -> /dev/vda)");

        /* RootBlk returns EIO on real I/O — it is a resolver placeholder, not a
         * working disk; it must not silently succeed for dd/blkid/fsck. */
        int fd = open("/dev/vda", O_RDONLY);
        CHECK(fd >= 0, "open /dev/vda O_RDONLY");
        if (fd >= 0) {
            char buf[16];
            errno = 0;
            ssize_t n = read(fd, buf, sizeof buf);
            CHECK(n < 0 && errno == EIO,
                  "read /dev/vda returns EIO (placeholder, no fake disk I/O)");
            close(fd);
        }
    }

    TEST_DONE();
}
