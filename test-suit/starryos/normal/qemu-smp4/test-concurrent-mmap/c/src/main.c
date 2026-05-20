/*
 * test-concurrent-mmap.c — Stress test for concurrent mmap/munmap/msync
 * under SMP.
 *
 * Forks N_WORKERS child processes, each of which maps the same file with
 * MAP_SHARED, writes a unique pattern to its assigned page range, calls
 * msync, then unmaps. The parent waits for all children and verifies that
 * every page contains the correct data from the child that wrote it.
 *
 * This exercises:
 *  - Concurrent page fault handling for shared file mappings
 *  - Page cache consistency under concurrent dirty writes
 *  - msync writeback correctness under concurrent access
 *  - MAP_PRIVATE copy-on-write isolation across processes
 */
#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/mman.h>
#include <sys/wait.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <string.h>

#define N_WORKERS 4
#define PAGES_PER_WORKER 2
#define TOTAL_PAGES (N_WORKERS * PAGES_PER_WORKER)
#define FILE_SIZE ((size_t)TOTAL_PAGES * 4096)
#define TEST_FILE "/tmp/concurrent_mmap_test"

static void fill_pattern(unsigned char *buf, int worker_id, int page_in_worker)
{
    for (int i = 0; i < 4096; i++) {
        buf[i] = (unsigned char)((worker_id * 31 + page_in_worker * 17 + i) & 0xFF);
    }
}

static int check_pattern(const unsigned char *buf, int worker_id, int page_in_worker)
{
    for (int i = 0; i < 4096; i++) {
        unsigned char expected = (unsigned char)((worker_id * 31 + page_in_worker * 17 + i) & 0xFF);
        if (buf[i] != expected) return 0;
    }
    return 1;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("concurrent_mmap");

    /* ── T1: Create and initialise test file ─────────────────────── */
    {
        int fd = open(TEST_FILE, O_CREAT | O_RDWR | O_TRUNC, 0666);
        CHECK(fd >= 0, "create test file");

        if (fd >= 0) {
            CHECK(ftruncate(fd, (off_t)FILE_SIZE) == 0, "ftruncate to FILE_SIZE");
            close(fd);
        }
    }

    /* ── T2: Fork workers – concurrent MAP_SHARED writes ─────────── */
    {
        pid_t pids[N_WORKERS];
        int n_forked = 0;
        int fork_ok = 1;

        for (int w = 0; w < N_WORKERS; w++) {
            pids[w] = fork();
            if (pids[w] < 0) {
                fork_ok = 0;
                break;
            }
            if (pids[w] == 0) {
                int fd = open(TEST_FILE, O_RDWR);
                if (fd < 0) _exit(10 + w);

                unsigned char *map = mmap(NULL, FILE_SIZE, PROT_READ | PROT_WRITE,
                                          MAP_SHARED, fd, 0);
                if (map == MAP_FAILED) {
                    close(fd);
                    _exit(20 + w);
                }

                for (int p = 0; p < PAGES_PER_WORKER; p++) {
                    int page_idx = w * PAGES_PER_WORKER + p;
                    fill_pattern(map + (size_t)page_idx * 4096, w, p);
                }

                msync(map, FILE_SIZE, MS_SYNC);
                munmap(map, FILE_SIZE);
                close(fd);
                _exit(0);
            }
            n_forked++;
        }
        CHECK(fork_ok, "fork all workers");

        int all_clean = 1;
        for (int w = 0; w < n_forked; w++) {
            int status = 0;
            waitpid(pids[w], &status, 0);
            if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
                printf("  FAIL | %s:%d | worker %d exit status %d\n",
                       __FILE__, __LINE__, w, WEXITSTATUS(status));
                __fail++;
                all_clean = 0;
            }
        }
        CHECK(all_clean, "all workers exited cleanly");

        /* ── T3: Verify data integrity ────────────────────────────── */
        {
            int fd = open(TEST_FILE, O_RDONLY);
            CHECK(fd >= 0, "open for verify");

            if (fd >= 0) {
                unsigned char *map = mmap(NULL, FILE_SIZE, PROT_READ,
                                          MAP_PRIVATE, fd, 0);
                CHECK(map != MAP_FAILED, "mmap for verify");

                if (map != MAP_FAILED) {
                    int verify_ok = 1;
                    for (int w = 0; w < N_WORKERS; w++) {
                        for (int p = 0; p < PAGES_PER_WORKER; p++) {
                            int page_idx = w * PAGES_PER_WORKER + p;
                            if (!check_pattern(map + (size_t)page_idx * 4096, w, p)) {
                                printf("  FAIL | %s:%d | page %d (worker %d) mismatch\n",
                                       __FILE__, __LINE__, page_idx, w);
                                __fail++;
                                verify_ok = 0;
                            }
                        }
                    }
                    if (verify_ok) {
                        printf("  PASS | %s:%d | all pages verified\n",
                               __FILE__, __LINE__);
                        __pass++;
                    }
                    munmap(map, FILE_SIZE);
                }
                close(fd);
            }
        }
    }

    /* ── T4: MAP_PRIVATE copy-on-write isolation ─────────────────── */
    {
        int fd = open(TEST_FILE, O_RDWR);
        CHECK(fd >= 0, "open for MAP_PRIVATE test");

        if (fd >= 0) {
            unsigned char *shared = mmap(NULL, FILE_SIZE, PROT_READ,
                                         MAP_SHARED, fd, 0);
            CHECK(shared != MAP_FAILED, "mmap shared baseline");

            if (shared != MAP_FAILED) {
                pid_t pid = fork();
                CHECK(pid >= 0, "fork for MAP_PRIVATE");

                if (pid == 0) {
                    unsigned char *priv = mmap(NULL, FILE_SIZE, PROT_READ | PROT_WRITE,
                                               MAP_PRIVATE, fd, 0);
                    if (priv == MAP_FAILED) _exit(1);
                    memset(priv, 0xAA, FILE_SIZE);
                    munmap(priv, FILE_SIZE);
                    close(fd);
                    _exit(0);
                }

                int status = 0;
                waitpid(pid, &status, 0);
                CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                      "MAP_PRIVATE child exited cleanly");

                int unchanged = 1;
                for (int w = 0; w < N_WORKERS && unchanged; w++) {
                    for (int p = 0; p < PAGES_PER_WORKER && unchanged; p++) {
                        int page_idx = w * PAGES_PER_WORKER + p;
                        if (!check_pattern(shared + (size_t)page_idx * 4096, w, p)) {
                            unchanged = 0;
                        }
                    }
                }
                CHECK(unchanged, "MAP_PRIVATE: shared data unchanged after child write");

                munmap(shared, FILE_SIZE);
            }
            close(fd);
        }
    }

    /* Cleanup */
    unlink(TEST_FILE);

    TEST_DONE();
}
