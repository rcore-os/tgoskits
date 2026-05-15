#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

static int __pass = 0;
static int __fail = 0;

#define CHECK(cond, msg) do {                                           \
    if (cond) {                                                         \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);      \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s\n", __FILE__, __LINE__, msg);      \
        __fail++;                                                       \
    }                                                                   \
    fflush(stdout);                                                     \
} while(0)

static int create_temp_file(const char *path, size_t size) {
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return -1;
    if (ftruncate(fd, size) != 0) { close(fd); return -1; }
    return fd;
}

int main(void) {
    printf("================================================\n");
    printf("  TEST: msync edge cases\n");
    printf("  FILE: %s\n", __FILE__);
    printf("================================================\n");
    fflush(stdout);

    const char *tmpfile = "/tmp/msync_test.bin";
    size_t file_size = 8192;
    unlink(tmpfile);

    /* ---- T1: Unaligned addr -> EINVAL ---- */
    printf("\n--- T1: Unaligned addr returns EINVAL ---\n"); fflush(stdout);
    {
        int fd = create_temp_file(tmpfile, file_size);
        CHECK(fd >= 0, "create temp file");
        if (fd >= 0) {
            void *p = mmap(NULL, file_size, PROT_READ | PROT_WRITE,
                           MAP_SHARED, fd, 0);
            close(fd);
            CHECK(p != MAP_FAILED, "mmap MAP_SHARED");
            if (p != MAP_FAILED) {
                memset(p, 0xAA, file_size);

                errno = 0;
                int rc = msync((char *)p + 1, 4096, MS_SYNC);
                CHECK(rc == -1 && errno == EINVAL,
                      "msync(addr+1) returns EINVAL");

                errno = 0;
                rc = msync((char *)p + 100, 4096, MS_SYNC);
                CHECK(rc == -1 && errno == EINVAL,
                      "msync(addr+100) returns EINVAL");

                errno = 0;
                rc = msync(p, 4096, MS_SYNC);
                CHECK(rc == 0, "msync(aligned_addr) succeeds");

                munmap(p, file_size);
            }
        }
    }

    /* ---- T2: MS_SYNC | MS_ASYNC together -> EINVAL ---- */
    printf("\n--- T2: MS_SYNC|MS_ASYNC returns EINVAL ---\n"); fflush(stdout);
    {
        int fd = create_temp_file(tmpfile, file_size);
        CHECK(fd >= 0, "create temp file");
        if (fd >= 0) {
            void *p = mmap(NULL, file_size, PROT_READ | PROT_WRITE,
                           MAP_SHARED, fd, 0);
            close(fd);
            CHECK(p != MAP_FAILED, "mmap MAP_SHARED");
            if (p != MAP_FAILED) {
                memset(p, 0xBB, file_size);

                errno = 0;
                int rc = msync(p, 4096, MS_SYNC | MS_ASYNC);
                CHECK(rc == -1 && errno == EINVAL,
                      "msync(MS_SYNC|MS_ASYNC) returns EINVAL");

                errno = 0;
                rc = msync(p, 4096, MS_SYNC);
                CHECK(rc == 0, "msync(MS_SYNC) alone succeeds");

                errno = 0;
                rc = msync(p, 4096, MS_ASYNC);
                CHECK(rc == 0, "msync(MS_ASYNC) alone succeeds");

                munmap(p, file_size);
            }
        }
    }

    /* ---- T3: Invalid flags -> EINVAL ---- */
    printf("\n--- T3: Unknown flags return EINVAL ---\n"); fflush(stdout);
    {
        int fd = create_temp_file(tmpfile, file_size);
        CHECK(fd >= 0, "create temp file");
        if (fd >= 0) {
            void *p = mmap(NULL, file_size, PROT_READ | PROT_WRITE,
                           MAP_SHARED, fd, 0);
            close(fd);
            CHECK(p != MAP_FAILED, "mmap MAP_SHARED");
            if (p != MAP_FAILED) {
                errno = 0;
                int rc = msync(p, 4096, MS_SYNC | 0x100);
                CHECK(rc == -1 && errno == EINVAL,
                      "msync(unknown flag bit) returns EINVAL");

                munmap(p, file_size);
            }
        }
    }

    /* ---- T4: length=0 -> success ---- */
    printf("\n--- T4: length=0 succeeds ---\n"); fflush(stdout);
    {
        int fd = create_temp_file(tmpfile, file_size);
        CHECK(fd >= 0, "create temp file");
        if (fd >= 0) {
            void *p = mmap(NULL, file_size, PROT_READ | PROT_WRITE,
                           MAP_SHARED, fd, 0);
            close(fd);
            CHECK(p != MAP_FAILED, "mmap MAP_SHARED");
            if (p != MAP_FAILED) {
                errno = 0;
                int rc = msync(p, 0, MS_SYNC);
                CHECK(rc == 0, "msync(len=0) succeeds");

                munmap(p, file_size);
            }
        }
    }

    /* ---- T5: Unmapped range -> ENOMEM ---- */
    printf("\n--- T5: Unmapped range returns ENOMEM ---\n"); fflush(stdout);
    {
        int fd = create_temp_file(tmpfile, file_size);
        CHECK(fd >= 0, "create temp file");
        if (fd >= 0) {
            void *p = mmap(NULL, file_size, PROT_READ | PROT_WRITE,
                           MAP_SHARED, fd, 0);
            close(fd);
            CHECK(p != MAP_FAILED, "mmap MAP_SHARED");
            if (p != MAP_FAILED) {
                munmap(p, file_size);

                errno = 0;
                int rc = msync(p, file_size, MS_SYNC);
                CHECK(rc == -1 && (errno == ENOMEM || errno == EINVAL),
                      "msync(unmapped) returns ENOMEM/EINVAL");

            }
        }
    }

    /* ---- T6: Normal sync after write ---- */
    printf("\n--- T6: Normal sync after write ---\n"); fflush(stdout);
    {
        int fd = create_temp_file(tmpfile, file_size);
        CHECK(fd >= 0, "create temp file");
        if (fd >= 0) {
            void *p = mmap(NULL, file_size, PROT_READ | PROT_WRITE,
                           MAP_SHARED, fd, 0);
            close(fd);
            CHECK(p != MAP_FAILED, "mmap MAP_SHARED");
            if (p != MAP_FAILED) {
                memset(p, 0xCC, file_size);

                errno = 0;
                int rc = msync(p, file_size, MS_SYNC);
                CHECK(rc == 0, "msync(MS_SYNC) after write succeeds");

                munmap(p, file_size);
            }
        }
    }

    /* ---- T7: MAP_PRIVATE sync (should succeed silently) ---- */
    printf("\n--- T7: MAP_PRIVATE msync succeeds ---\n"); fflush(stdout);
    {
        int fd = create_temp_file(tmpfile, file_size);
        CHECK(fd >= 0, "create temp file");
        if (fd >= 0) {
            void *p = mmap(NULL, file_size, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE, fd, 0);
            close(fd);
            CHECK(p != MAP_FAILED, "mmap MAP_PRIVATE");
            if (p != MAP_FAILED) {
                memset(p, 0xDD, file_size);

                errno = 0;
                int rc = msync(p, file_size, MS_SYNC);
                CHECK(rc == 0, "msync(MAP_PRIVATE) succeeds");

                munmap(p, file_size);
            }
        }
    }

    /* ---- T8: Repeated write-msync cycle data persistence ---- */
    printf("\n--- T8: Repeated write-msync cycle data persistence ---\n"); fflush(stdout);
    {
        int fd = create_temp_file(tmpfile, file_size);
        CHECK(fd >= 0, "create temp file");
        if (fd >= 0) {
            void *p = mmap(NULL, file_size, PROT_READ | PROT_WRITE,
                           MAP_SHARED, fd, 0);
            close(fd);
            CHECK(p != MAP_FAILED, "mmap MAP_SHARED");
            if (p != MAP_FAILED) {
                memset(p, 0x11, file_size);
                errno = 0;
                int rc = msync(p, file_size, MS_SYNC);
                CHECK(rc == 0, "first msync succeeds");

                memset(p, 0x22, file_size);
                errno = 0;
                rc = msync(p, file_size, MS_SYNC);
                CHECK(rc == 0, "second msync succeeds");

                memset(p, 0x33, file_size);
                errno = 0;
                rc = msync(p, file_size, MS_SYNC);
                CHECK(rc == 0, "third msync succeeds");

                munmap(p, file_size);

                fd = open(tmpfile, O_RDONLY);
                CHECK(fd >= 0, "reopen file for verification");
                if (fd >= 0) {
                    unsigned char buf[4096];
                    ssize_t n = read(fd, buf, sizeof(buf));
                    CHECK(n == (ssize_t)sizeof(buf), "read back first page");

                    int all_match = 1;
                    for (int i = 0; i < (int)sizeof(buf); i++) {
                        if (buf[i] != 0x33) { all_match = 0; break; }
                    }
                    CHECK(all_match, "data matches third write (0x33)");

                    close(fd);
                }
            }
        }
    }

    /* ---- T9: Two MAP_SHARED mappings (different pages), partial msync ---- */
    printf("\n--- T9: Two MAP_SHARED mappings, partial msync ---\n"); fflush(stdout);
    {
        unlink(tmpfile);
        size_t big_size = 16384;
        int fd = create_temp_file(tmpfile, big_size);
        CHECK(fd >= 0, "create big temp file");
        if (fd >= 0) {
            void *p1 = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                            MAP_SHARED, fd, 0);
            void *p2 = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                            MAP_SHARED, fd, 4096);
            close(fd);
            CHECK(p1 != MAP_FAILED, "mmap first page");
            CHECK(p2 != MAP_FAILED, "mmap second page");
            if (p1 != MAP_FAILED && p2 != MAP_FAILED) {
                memset(p1, 0xAA, 4096);
                memset(p2, 0xBB, 4096);

                errno = 0;
                int rc = msync(p1, 4096, MS_SYNC);
                CHECK(rc == 0, "msync first page only");

                memset(p1, 0x11, 4096);
                memset(p2, 0x22, 4096);

                errno = 0;
                rc = msync(p1, 4096, MS_SYNC);
                CHECK(rc == 0, "msync first page again after rewrite");

                errno = 0;
                rc = msync(p2, 4096, MS_SYNC);
                CHECK(rc == 0, "msync second page");

                munmap(p1, 4096);
                munmap(p2, 4096);

                fd = open(tmpfile, O_RDONLY);
                CHECK(fd >= 0, "reopen for verification");
                if (fd >= 0) {
                    unsigned char buf1[4096], buf2[4096];
                    ssize_t n1 = read(fd, buf1, 4096);
                    ssize_t n2 = read(fd, buf2, 4096);
                    close(fd);

                    CHECK(n1 == 4096, "read page 1");
                    CHECK(n2 == 4096, "read page 2");

                    int page1_ok = 1, page2_ok = 1;
                    for (int i = 0; i < 4096; i++) {
                        if (buf1[i] != 0x11) page1_ok = 0;
                        if (buf2[i] != 0x22) page2_ok = 0;
                    }
                    CHECK(page1_ok, "page 1 data matches 0x11");
                    CHECK(page2_ok, "page 2 data matches 0x22");
                }
            }
        }
    }

    /* ---- T10: Same file page mapped twice (alias), write through both ---- */
    printf("\n--- T10: Same file page dual alias write+msync ---\n"); fflush(stdout);
    {
        unlink(tmpfile);
        int fd = create_temp_file(tmpfile, 4096);
        CHECK(fd >= 0, "create temp file");
        if (fd >= 0) {
            void *p1 = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                            MAP_SHARED, fd, 0);
            void *p2 = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                            MAP_SHARED, fd, 0);
            close(fd);
            CHECK(p1 != MAP_FAILED, "mmap alias 1");
            CHECK(p2 != MAP_FAILED, "mmap alias 2");
            if (p1 != MAP_FAILED && p2 != MAP_FAILED) {
                memset(p1, 0xAA, 4096);
                errno = 0;
                int rc = msync(p1, 4096, MS_SYNC);
                CHECK(rc == 0, "msync alias 1 after p1 write");

                memset(p2, 0xBB, 4096);
                errno = 0;
                rc = msync(p2, 4096, MS_SYNC);
                CHECK(rc == 0, "msync alias 2 after p2 write");

                memset(p1, 0x11, 4096);
                errno = 0;
                rc = msync(p1, 4096, MS_SYNC);
                CHECK(rc == 0, "msync alias 1 after p1 rewrite");

                memset(p2, 0x22, 4096);
                errno = 0;
                rc = msync(p2, 4096, MS_SYNC);
                CHECK(rc == 0, "msync alias 2 after p2 rewrite");

                munmap(p1, 4096);
                munmap(p2, 4096);

                fd = open(tmpfile, O_RDONLY);
                CHECK(fd >= 0, "reopen for verification");
                if (fd >= 0) {
                    unsigned char buf[4096];
                    ssize_t n = read(fd, buf, 4096);
                    close(fd);
                    CHECK(n == 4096, "read back page");

                    int all_0x22 = 1;
                    for (int i = 0; i < 4096; i++) {
                        if (buf[i] != 0x22) { all_0x22 = 0; break; }
                    }
                    CHECK(all_0x22, "data matches last alias write (0x22)");
                }
            }
        }
    }

    unlink(tmpfile);

    printf("------------------------------------------------\n");
    printf("  DONE: %d pass, %d fail\n", __pass, __fail);
    printf("================================================\n\n");
    fflush(stdout);

    return __fail > 0 ? 1 : 0;
}
