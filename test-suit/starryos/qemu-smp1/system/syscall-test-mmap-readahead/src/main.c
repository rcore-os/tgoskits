/*
 * test-mmap-readahead — file-backed mmap demand-paging readahead correctness.
 *
 * StarryOS now fills a run of consecutive not-yet-mapped FILE-backed pages with
 * a SINGLE batched `read_at` (a 32-page readahead window) instead of one read
 * per page (feat: file-backed mmap readahead). This regression guards that the
 * batched fill produces byte-IDENTICAL contents to the per-page path:
 *   - a mapping LARGER than the 32-page window (so multiple batched runs + a
 *     boundary are exercised) reads back exactly the file's bytes, at every
 *     offset (an off-by-one in the per-page run offset would corrupt a page);
 *   - the partial last page past EOF is demand-zero (the readahead read is
 *     clamped to file size, the tail stays zero);
 *   - faulting first in the MIDDLE of the mapping (window starting mid-file)
 *     still yields correct bytes everywhere.
 *
 * The pattern is a strict hash of the absolute file offset, so any page that
 * received the wrong file-offset's data fails immediately.
 */

#include "test_framework.h"

#include <stdint.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>

#define READAHEAD_PAGES 32
#define EXTRA_PAGES 8           /* map > one readahead window */
#define PARTIAL_TAIL 1234       /* partial last page -> EOF zero-fill path */

static unsigned char pat(size_t off)
{
    /* strict function of the absolute offset: a wrong offset => wrong byte */
    return (unsigned char)(((uint32_t)off * 2654435761u) >> 24);
}

int main(void)
{
    TEST_START("mmap file-backed readahead (batched page-fault fill)");

    long psl = sysconf(_SC_PAGESIZE);
    size_t ps = psl > 0 ? (size_t)psl : 4096;
    size_t pages = READAHEAD_PAGES + EXTRA_PAGES;
    size_t filesz = pages * ps + PARTIAL_TAIL;
    size_t mapsz = ((filesz + ps - 1) / ps) * ps;
    const char *path = "/tmp/bb_readahead_test.bin";

    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "open(create) test file");
    if (fd < 0) { TEST_DONE(); }

    unsigned char *wbuf = malloc(ps);
    CHECK(wbuf != NULL, "malloc write buffer");
    int wok = (wbuf != NULL);
    for (size_t off = 0; wok && off < filesz; ) {
        size_t chunk = (filesz - off < ps) ? (filesz - off) : ps;
        for (size_t k = 0; k < chunk; k++) wbuf[k] = pat(off + k);
        if (write(fd, wbuf, chunk) != (ssize_t)chunk) { wok = 0; break; }
        off += chunk;
    }
    CHECK(wok, "write full offset-hash pattern to file");
    free(wbuf);
    fsync(fd);
    close(fd);

    /* ---- mapping 1: sequential (fault page 0 first) ---- */
    fd = open(path, O_RDONLY);
    CHECK(fd >= 0, "reopen O_RDONLY");
    unsigned char *m = mmap(NULL, mapsz, PROT_READ, MAP_PRIVATE, fd, 0);
    CHECK(m != MAP_FAILED, "mmap MAP_PRIVATE PROT_READ (> readahead window)");
    if (m != MAP_FAILED) {
        size_t bad = 0;
        for (size_t i = 0; i < filesz; i++) {
            if (m[i] != pat(i)) {
                if (bad < 3)
                    printf("    mismatch@%zu got=%u want=%u\n", i, m[i], pat(i));
                bad++;
            }
        }
        CHECK(bad == 0, "all in-file bytes match (batched-run offsets correct)");

        size_t zbad = 0;
        for (size_t i = filesz; i < mapsz; i++) if (m[i] != 0) zbad++;
        CHECK(zbad == 0, "bytes past EOF in last page are zero (readahead clamp)");

        munmap(m, mapsz);
    }
    close(fd);

    /* ---- mapping 2: fault MID-mapping first (page 35) ---- */
    fd = open(path, O_RDONLY);
    m = mmap(NULL, mapsz, PROT_READ, MAP_PRIVATE, fd, 0);
    CHECK(m != MAP_FAILED, "mmap #2 for mid-mapping-first fault");
    if (m != MAP_FAILED) {
        volatile unsigned char touch = m[(READAHEAD_PAGES + 3) * ps]; /* window starts mid-file */
        (void)touch;
        size_t bad = 0;
        for (size_t i = 0; i < filesz; i++) if (m[i] != pat(i)) bad++;
        CHECK(bad == 0, "mid-mapping-first fault: every byte still correct");
        munmap(m, mapsz);
    }
    close(fd);
    unlink(path);

    TEST_DONE();
}
