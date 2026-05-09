/*
 * test-rename-content
 *
 * Exercises the axfs-ng-vfs rename DirEntry preservation path.
 *
 * Bug: DirNode::rename called forget_entry on the source DirEntry after the
 * underlying rename succeeded, releasing the only strong reference to the page
 * cache backing the file. A subsequent lookup by the destination name allocated
 * a fresh, zero-filled page cache, making reads of the renamed file return all
 * zeros.
 *
 * This replicates PostgreSQL's durable_rename pattern:
 *   open src, write known payload, fsync, close, rename src -> dst,
 *   open dst, read back -> content must equal what was written.
 */

#include "test_framework.h"
#include <fcntl.h>
#include <unistd.h>
#include <string.h>

#define SRC_PATH "/tmp/test-b20-src.tmp"
#define DST_PATH "/tmp/test-b20-dst.dat"

static const char PAYLOAD[] = "durable_rename_content_check_ABCDEFGH1234567890";
#define PAYLOAD_LEN ((int)(sizeof(PAYLOAD) - 1))

int main(void)
{
    TEST_START("rename preserves file content (durable_rename pattern)");

    unlink(SRC_PATH);
    unlink(DST_PATH);

    int wfd = open(SRC_PATH, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    CHECK(wfd >= 0, "open src for write");
    if (wfd < 0)
        goto done;

    CHECK_RET((int)write(wfd, PAYLOAD, PAYLOAD_LEN), PAYLOAD_LEN,
              "write payload to src");
    CHECK_RET(fsync(wfd), 0, "fsync src");
    close(wfd);

    CHECK_RET(rename(SRC_PATH, DST_PATH), 0, "rename src to dst");

    int rfd = open(DST_PATH, O_RDONLY);
    CHECK(rfd >= 0, "open dst for read");
    if (rfd < 0)
        goto done;

    char buf[256] = {0};
    int n = (int)read(rfd, buf, sizeof(buf));
    close(rfd);

    CHECK(n == PAYLOAD_LEN, "read returns correct byte count");
    CHECK(memcmp(buf, PAYLOAD, PAYLOAD_LEN) == 0,
          "dst content matches written payload");

    unlink(DST_PATH);

done:
    TEST_DONE();
}
