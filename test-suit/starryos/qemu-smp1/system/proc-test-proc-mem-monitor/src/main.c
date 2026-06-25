/*
 * test-proc-mem-monitor: validate process memory monitoring via /proc.
 *
 * Covers user-space scenarios used by psutil, top, and glances:
 *   - /proc/self/status  VmSize / VmRSS / VmData / VmStk / RssAnon
 *   - /proc/self/statm   seven-field layout and cross-field consistency
 *   - /proc/self/stat    vsize (bytes) and rss (pages) consistency
 *   - mmap growth        statm size increases after anonymous mapping
 *   - lazy RSS           VSS grows without touching pages; RSS grows on fault
 *   - munmap             RSS does not increase after unmapping touched pages
 *   - fork + COW         clone_map + parent/child RSS invariants across COW write
 *   - MAP_PRIVATE file   RssFile on read fault; single-process write → Anon
 *   - MAP_PRIVATE RW     write-first fault counts as RssAnon
 *   - MAP_PRIVATE RW     read-then-write without mprotect → File then Anon
 *   - fork RW file       parent read, child write; layered fault/fork/COW checks
 *   - fork dirty file    child COW on already-Anon private file page
 *   - memfd MAP_SHARED   RssShmem on shared fault
 *   - /proc/<child>      parent reads child memory stats after fork()
 *   - /proc/meminfo      PageTables field is parseable (allocator-backed)
 *
 * Long-term invariants (Plan1 + Plan2):
 *   - VmRSS > 0, VmSize > 0, VmRSS <= VmSize
 *   - statm resident > 0, statm size > 0, resident <= size
 *   - stat vsize == statm size * page_size; stat rss == statm resident
 *   - statm size (pages) == VmSize (kB) converted with sysconf(_SC_PAGESIZE)
 *
 * Plan2: resident may be strictly less than VSS until pages are faulted in.
 */
#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef MFD_CLOEXEC
#define MFD_CLOEXEC 0x0001
#endif
#ifndef SYS_memfd_create
#define SYS_memfd_create 279
#endif

static int memfd_create_sys(const char *name, unsigned int flags)
{
    return (int)syscall(SYS_memfd_create, name, flags);
}

enum { MMAP_PROBE_SIZE = 2 * 1024 * 1024 };
enum { LAZY_MAP_PAGES = MMAP_PROBE_SIZE / 4096 };
enum { RSS_TOUCH_TOLERANCE = 16 };

static void expect(int condition, const char *message)
{
    if (!condition) {
        fputs("FAIL: ", stderr);
        fputs(message, stderr);
        fputc('\n', stderr);
        abort();
    }
}

static ssize_t io_read_all(int fd, void *buf, size_t len)
{
    unsigned char *cursor = buf;
    size_t left = len;

    while (left > 0) {
        ssize_t n = read(fd, cursor, left);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            return n;
        }
        if (n == 0) {
            return (ssize_t)(len - left);
        }
        cursor += (size_t)n;
        left -= (size_t)n;
    }
    return (ssize_t)len;
}

static ssize_t io_write_all(int fd, const void *buf, size_t len)
{
    const unsigned char *cursor = buf;
    size_t left = len;

    while (left > 0) {
        ssize_t n = write(fd, cursor, left);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            return n;
        }
        if (n == 0) {
            return (ssize_t)(len - left);
        }
        cursor += (size_t)n;
        left -= (size_t)n;
    }
    return (ssize_t)len;
}

static long read_status_kb_from(const char *path, const char *key)
{
    int fd = open(path, O_RDONLY);
    expect(fd >= 0, path);

    char buf[4096];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    expect(n > 0, path);
    buf[n] = '\0';

    char prefix[64];
    snprintf(prefix, sizeof(prefix), "%s:\t", key);

    char *save = NULL;
    char *line = strtok_r(buf, "\n", &save);
    while (line != NULL) {
        if (strncmp(line, prefix, strlen(prefix)) == 0) {
            long value = -1;
            expect(sscanf(line + strlen(prefix), "%ld kB", &value) == 1, key);
            return value;
        }
        line = strtok_r(NULL, "\n", &save);
    }

    expect(0, "missing status key");
    return -1;
}

static long read_self_status_kb(const char *key)
{
    return read_status_kb_from("/proc/self/status", key);
}

typedef struct {
    pid_t pid;
    long rss_file_kb;
    long rss_anon_kb;
} fork_rw_rss_sync_t;

static long rss_kb_delta(long after, long before)
{
    return after - before;
}

static int rss_kb_within_tolerance(long before, long after, long page_kb)
{
    long delta = rss_kb_delta(after, before);
    if (delta < 0) {
        delta = -delta;
    }
    return delta <= page_kb * RSS_TOUCH_TOLERANCE;
}

static int rss_kb_not_dropped_by_page(long before, long after, long page_kb)
{
    return after + page_kb > before;
}

static long read_status_pid_from(const char *path, const char *key)
{
    int fd = open(path, O_RDONLY);
    expect(fd >= 0, path);

    char buf[4096];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    expect(n > 0, path);
    buf[n] = '\0';

    char prefix[64];
    snprintf(prefix, sizeof(prefix), "%s:\t", key);

    char *save = NULL;
    char *line = strtok_r(buf, "\n", &save);
    while (line != NULL) {
        if (strncmp(line, prefix, strlen(prefix)) == 0) {
            long value = -1;
            expect(sscanf(line + strlen(prefix), "%ld", &value) == 1, key);
            return value;
        }
        line = strtok_r(NULL, "\n", &save);
    }

    expect(0, "missing status pid key");
    return -1;
}

static void read_statm_fields_from(
    const char *path,
    unsigned long *size,
    unsigned long *resident,
    unsigned long *shared,
    unsigned long *text,
    unsigned long *data
)
{
    int fd = open(path, O_RDONLY);
    expect(fd >= 0, path);

    char buf[512];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    expect(n > 0, path);
    buf[n] = '\0';

    unsigned long lib = 0;
    unsigned long dirty = 0;
    int fields = sscanf(
        buf,
        "%lu %lu %lu %lu %lu %lu %lu",
        size,
        resident,
        shared,
        text,
        &lib,
        data,
        &dirty
    );

    expect(fields == 7, "statm must expose seven integer fields");
}

static unsigned long read_stat_field_after_comm_from(const char *path, unsigned token_index)
{
    int fd = open(path, O_RDONLY);
    expect(fd >= 0, path);

    char line[4096];
    ssize_t n = read(fd, line, sizeof(line) - 1);
    close(fd);
    expect(n > 0, "read proc stat");
    line[n] = '\0';

    char *cursor = strchr(line, ')');
    expect(cursor != NULL, "stat comm field");
    cursor += 2;
    while (*cursor == ' ') {
        cursor++;
    }

    for (unsigned i = 0; i < token_index; i++) {
        cursor = strchr(cursor, ' ');
        expect(cursor != NULL, "stat field index out of range");
        do {
            cursor++;
        } while (*cursor == ' ');
    }

    unsigned long value = 0;
    expect(sscanf(cursor, "%lu", &value) == 1, "parse stat field");
    return value;
}

static long read_meminfo_kb(const char *key)
{
    int fd = open("/proc/meminfo", O_RDONLY);
    expect(fd >= 0, "open /proc/meminfo");

    char buf[4096];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    expect(n > 0, "/proc/meminfo");
    buf[n] = '\0';

    char prefix[64];
    snprintf(prefix, sizeof(prefix), "%s:", key);

    char *save = NULL;
    char *line = strtok_r(buf, "\n", &save);
    while (line != NULL) {
        if (strncmp(line, prefix, strlen(prefix)) == 0) {
            long value = -1;
            expect(sscanf(line + strlen(prefix), "%ld kB", &value) == 1, key);
            return value;
        }
        line = strtok_r(NULL, "\n", &save);
    }

    expect(0, "missing meminfo key");
    return -1;
}

static void test_lazy_and_touch_rss(long page_size)
{
    unsigned long size_before = 0;
    unsigned long resident_before = 0;
    unsigned long size_after_map = 0;
    unsigned long resident_after_map = 0;
    unsigned long resident_after_touch = 0;
    unsigned long resident_after_full = 0;
    unsigned long resident_after_unmap = 0;
    unsigned long shared = 0;
    unsigned long text = 0;
    unsigned long data = 0;
    void *map = MAP_FAILED;
    volatile char *pages = NULL;
    unsigned long i = 0;

    read_statm_fields_from(
        "/proc/self/statm",
        &size_before,
        &resident_before,
        &shared,
        &text,
        &data
    );

    map = mmap(NULL, MMAP_PROBE_SIZE, PROT_READ | PROT_WRITE,
               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    expect(map != MAP_FAILED, "lazy mmap anonymous region");
    pages = (volatile char *)map;

    read_statm_fields_from(
        "/proc/self/statm",
        &size_after_map,
        &resident_after_map,
        &shared,
        &text,
        &data
    );
    expect(size_after_map >= size_before + LAZY_MAP_PAGES,
           "statm size grows after lazy mmap");
    expect(resident_after_map + RSS_TOUCH_TOLERANCE >= resident_before,
           "lazy mmap must not drop RSS significantly");
    expect(resident_after_map <= resident_before + RSS_TOUCH_TOLERANCE,
           "lazy mmap must not fault entire mapping");

    pages[0] = 1;
    read_statm_fields_from(
        "/proc/self/statm",
        &size_after_map,
        &resident_after_touch,
        &shared,
        &text,
        &data
    );
    expect(resident_after_touch >= resident_after_map + 1,
           "touch one page increases RSS");

    for (i = 0; i < LAZY_MAP_PAGES; i++) {
        pages[i * (unsigned long)page_size] = (char)i;
    }
    read_statm_fields_from(
        "/proc/self/statm",
        &size_after_map,
        &resident_after_full,
        &shared,
        &text,
        &data
    );
    expect(resident_after_full + RSS_TOUCH_TOLERANCE >=
               resident_after_touch + LAZY_MAP_PAGES,
           "touching full mapping increases RSS by page count");

    munmap(map, MMAP_PROBE_SIZE);
    read_statm_fields_from(
        "/proc/self/statm",
        &size_after_map,
        &resident_after_unmap,
        &shared,
        &text,
        &data
    );
    expect(resident_after_unmap <= resident_after_full,
           "munmap must not increase RSS");
}

static void test_map_private_file_rss(long page_size)
{
    char path[] = "/tmp/proc-mem-monitor-XXXXXX";
    int fd = mkstemp(path);
    long page_kb = page_size / 1024;
    long rss_anon_before = 0;
    long rss_file_before = 0;
    long rss_anon_after_read = 0;
    long rss_file_after_read = 0;
    long rss_anon_after_write = 0;
    long rss_file_after_write = 0;
    unsigned long statm_shared_before = 0;
    unsigned long statm_shared_after_read = 0;
    unsigned long statm_shared_after_write = 0;
    unsigned long statm_size = 0;
    unsigned long statm_resident = 0;
    unsigned long statm_text = 0;
    unsigned long statm_data = 0;
    unsigned char buf[4096];
    void *map = MAP_FAILED;
    volatile char *page = NULL;

    expect(fd >= 0, "mkstemp for MAP_PRIVATE file probe");
    memset(buf, 0x5a, sizeof(buf));
    expect(write(fd, buf, sizeof(buf)) == (ssize_t)sizeof(buf), "write temp file page");
    expect(ftruncate(fd, (off_t)sizeof(buf)) == 0, "truncate temp file");

    rss_anon_before = read_self_status_kb("RssAnon");
    rss_file_before = read_self_status_kb("RssFile");
    read_statm_fields_from(
        "/proc/self/statm",
        &statm_size,
        &statm_resident,
        &statm_shared_before,
        &statm_text,
        &statm_data
    );

    map = mmap(NULL, (size_t)page_size, PROT_READ, MAP_PRIVATE, fd, 0);
    expect(map != MAP_FAILED, "mmap MAP_PRIVATE file read-only");
    page = (volatile char *)map;
    expect(page[0] == (char)0x5a, "fault file-backed page");

    rss_anon_after_read = read_self_status_kb("RssAnon");
    rss_file_after_read = read_self_status_kb("RssFile");
    read_statm_fields_from(
        "/proc/self/statm",
        &statm_size,
        &statm_resident,
        &statm_shared_after_read,
        &statm_text,
        &statm_data
    );
    expect(rss_file_after_read >= rss_file_before + page_kb,
           "file fault increases RssFile");
    expect(rss_anon_after_read <= rss_anon_before + page_kb * RSS_TOUCH_TOLERANCE,
           "read fault does not significantly increase RssAnon");
    expect(statm_shared_after_read >= statm_shared_before + 1,
           "statm shared grows after file fault");

    expect(mprotect((void *)map, (size_t)page_size, PROT_READ | PROT_WRITE) == 0,
           "mprotect file mapping writable");
    page[0] = 0x42;
    rss_anon_after_write = read_self_status_kb("RssAnon");
    rss_file_after_write = read_self_status_kb("RssFile");
    read_statm_fields_from(
        "/proc/self/statm",
        &statm_size,
        &statm_resident,
        &statm_shared_after_write,
        &statm_text,
        &statm_data
    );
    expect(rss_anon_after_write >= rss_anon_after_read + page_kb,
           "single-process file COW write increases RssAnon");
    expect(rss_file_after_write + page_kb <=
               rss_file_after_read + page_kb * RSS_TOUCH_TOLERANCE,
           "single-process file COW write transfers RssFile to RssAnon");
    expect(statm_shared_after_write + 1 <=
               statm_shared_after_read + RSS_TOUCH_TOLERANCE,
           "statm shared drops after File to Anon transfer");

    munmap(map, (size_t)page_size);
    close(fd);
    unlink(path);
}

static void test_map_private_file_write_first_fault(long page_size)
{
    char path[] = "/tmp/proc-mem-monitor-wf-XXXXXX";
    int fd = mkstemp(path);
    long page_kb = page_size / 1024;
    long rss_anon_before = 0;
    long rss_file_before = 0;
    long rss_anon_after = 0;
    long rss_file_after = 0;
    unsigned char buf[4096];
    void *map = MAP_FAILED;
    volatile char *page = NULL;

    expect(fd >= 0, "mkstemp for write-first fault probe");
    memset(buf, 0x5a, sizeof(buf));
    expect(write(fd, buf, sizeof(buf)) == (ssize_t)sizeof(buf), "write temp file");
    expect(ftruncate(fd, (off_t)sizeof(buf)) == 0, "truncate temp file");

    rss_anon_before = read_self_status_kb("RssAnon");
    rss_file_before = read_self_status_kb("RssFile");

    map = mmap(NULL, (size_t)page_size, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    expect(map != MAP_FAILED, "mmap MAP_PRIVATE file read-write");
    page = (volatile char *)map;
    page[0] = 0x42;

    rss_anon_after = read_self_status_kb("RssAnon");
    rss_file_after = read_self_status_kb("RssFile");
    expect(rss_anon_after >= rss_anon_before + page_kb,
           "write-first fault counts as RssAnon");
    expect(rss_file_after <= rss_file_before + page_kb * RSS_TOUCH_TOLERANCE,
           "write-first fault does not significantly increase RssFile");

    munmap(map, (size_t)page_size);
    close(fd);
    unlink(path);
}

static void test_map_private_file_read_then_write(long page_size)
{
    char path[] = "/tmp/proc-mem-monitor-rwrt-XXXXXX";
    int fd = mkstemp(path);
    long page_kb = page_size / 1024;
    long rss_anon_before = 0;
    long rss_file_before = 0;
    long rss_anon_after_read = 0;
    long rss_file_after_read = 0;
    long rss_anon_after_write = 0;
    long rss_file_after_write = 0;
    unsigned long statm_shared_before = 0;
    unsigned long statm_shared_after_read = 0;
    unsigned long statm_shared_after_write = 0;
    unsigned long statm_size = 0;
    unsigned long statm_resident = 0;
    unsigned long statm_text = 0;
    unsigned long statm_data = 0;
    unsigned char buf[4096];
    void *map = MAP_FAILED;
    volatile char *page = NULL;

    expect(fd >= 0, "mkstemp for RW read-then-write probe");
    memset(buf, 0x5a, sizeof(buf));
    expect(write(fd, buf, sizeof(buf)) == (ssize_t)sizeof(buf), "write temp file page");
    expect(ftruncate(fd, (off_t)sizeof(buf)) == 0, "truncate temp file");

    rss_anon_before = read_self_status_kb("RssAnon");
    rss_file_before = read_self_status_kb("RssFile");
    read_statm_fields_from(
        "/proc/self/statm",
        &statm_size,
        &statm_resident,
        &statm_shared_before,
        &statm_text,
        &statm_data
    );

    map = mmap(NULL, (size_t)page_size, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    expect(map != MAP_FAILED, "mmap MAP_PRIVATE file read-write without mprotect");
    page = (volatile char *)map;
    expect(page[0] == (char)0x5a, "read fault on RW MAP_PRIVATE file mapping");

    rss_anon_after_read = read_self_status_kb("RssAnon");
    rss_file_after_read = read_self_status_kb("RssFile");
    read_statm_fields_from(
        "/proc/self/statm",
        &statm_size,
        &statm_resident,
        &statm_shared_after_read,
        &statm_text,
        &statm_data
    );
    expect(rss_file_after_read >= rss_file_before + page_kb,
           "read fault increases RssFile on RW mmap");
    expect(rss_anon_after_read <= rss_anon_before + page_kb * RSS_TOUCH_TOLERANCE,
           "read fault does not significantly increase RssAnon on RW mmap");
    expect(statm_shared_after_read >= statm_shared_before + 1,
           "statm shared grows after read fault on RW mmap");

    page[0] = 0x42;
    rss_anon_after_write = read_self_status_kb("RssAnon");
    rss_file_after_write = read_self_status_kb("RssFile");
    read_statm_fields_from(
        "/proc/self/statm",
        &statm_size,
        &statm_resident,
        &statm_shared_after_write,
        &statm_text,
        &statm_data
    );
    expect(rss_anon_after_write >= rss_anon_after_read + page_kb,
           "RW mmap read-then-write increases RssAnon");
    expect(rss_file_after_write + page_kb <= rss_file_after_read,
           "RW mmap read-then-write transfers RssFile to RssAnon");
    expect(statm_shared_after_write + 1 <= statm_shared_after_read,
           "statm shared drops after RW mmap File to Anon transfer");

    munmap(map, (size_t)page_size);
    close(fd);
    unlink(path);
}

static void test_fork_rw_file_read_child_write(long page_size)
{
    char path[] = "/tmp/proc-mem-monitor-frw-XXXXXX";
    int fd = mkstemp(path);
    long page_kb = page_size / 1024;
    long parent_file_before_read = 0;
    long parent_anon_before_read = 0;
    long parent_file_after_read = 0;
    long parent_anon_after_read = 0;
    long parent_file_after_fork = 0;
    long parent_anon_after_fork = 0;
    long parent_file_after_child = 0;
    long parent_anon_after_child = 0;
    long child_rss_file = 0;
    long child_rss_anon = 0;
    fork_rw_rss_sync_t child_before_write = {0};
    fork_rw_rss_sync_t child_after_write = {0};
    int notify[2];
    int release[2];
    unsigned char buf[4096];
    void *map = MAP_FAILED;
    volatile char *page = NULL;
    pid_t child = 0;
    char child_status[64];
    char byte = 0;
    int status = 0;

    expect(fd >= 0, "mkstemp for fork RW file read-child-write probe");
    memset(buf, 0x5a, sizeof(buf));
    expect(write(fd, buf, sizeof(buf)) == (ssize_t)sizeof(buf), "write temp file");
    expect(ftruncate(fd, (off_t)sizeof(buf)) == 0, "truncate temp file");

    map = mmap(NULL, (size_t)page_size, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    expect(map != MAP_FAILED, "mmap MAP_PRIVATE file read-write before fork");
    page = (volatile char *)map;

    parent_file_before_read = read_self_status_kb("RssFile");
    parent_anon_before_read = read_self_status_kb("RssAnon");
    expect(page[0] == (char)0x5a, "parent read fault on RW MAP_PRIVATE file");
    parent_file_after_read = read_self_status_kb("RssFile");
    parent_anon_after_read = read_self_status_kb("RssAnon");

    expect(parent_file_after_read >= parent_file_before_read + page_kb,
           "layer A: read fault increases RssFile by one page");
    expect(parent_anon_after_read <=
               parent_anon_before_read + page_kb * RSS_TOUCH_TOLERANCE,
           "layer A: read fault does not significantly increase RssAnon");

    expect(pipe(notify) == 0, "notify pipe for fork RW file sync");
    expect(pipe(release) == 0, "release pipe for fork RW file sync");

    child = fork();
    expect(child >= 0, "fork for RW file read-child-write probe");

    if (child == 0) {
        close(notify[0]);
        close(release[1]);
        child_before_write.pid = getpid();
        child_before_write.rss_file_kb = read_self_status_kb("RssFile");
        child_before_write.rss_anon_kb = read_self_status_kb("RssAnon");
        if (io_write_all(notify[1], &child_before_write, sizeof(child_before_write)) !=
            (ssize_t)sizeof(child_before_write)) {
            _exit(1);
        }
        if (io_read_all(release[0], &byte, 1) != 1) {
            _exit(1);
        }
        page[0] = 0x43;
        child_after_write.pid = getpid();
        child_after_write.rss_file_kb = read_self_status_kb("RssFile");
        child_after_write.rss_anon_kb = read_self_status_kb("RssAnon");
        if (io_write_all(notify[1], &child_after_write, sizeof(child_after_write)) !=
            (ssize_t)sizeof(child_after_write)) {
            _exit(1);
        }
        close(notify[1]);
        close(release[0]);
        close(fd);
        _exit(0);
    }

    parent_file_after_fork = read_self_status_kb("RssFile");
    parent_anon_after_fork = read_self_status_kb("RssAnon");
    close(notify[1]);
    close(release[0]);

    expect(rss_kb_not_dropped_by_page(parent_file_after_read, parent_file_after_fork,
                                      page_kb),
           "layer B: fork must not drop parent RssFile by a page");
    expect(parent_file_after_fork <=
               parent_file_after_read + page_kb * RSS_TOUCH_TOLERANCE,
           "layer B: fork keeps parent RssFile stable within tolerance");
    expect(rss_kb_not_dropped_by_page(parent_anon_after_read, parent_anon_after_fork,
                                      page_kb),
           "layer B: fork must not drop parent RssAnon by a page");
    expect(parent_anon_after_fork <=
               parent_anon_after_read + page_kb * RSS_TOUCH_TOLERANCE,
           "layer B: fork keeps parent RssAnon stable within tolerance");

    expect(io_read_all(notify[0], &child_before_write, sizeof(child_before_write)) ==
               (ssize_t)sizeof(child_before_write),
           "read child RSS before RW file COW write");
    expect(child_before_write.pid == child, "child pid matches fork return");

    expect(rss_kb_not_dropped_by_page(parent_file_after_read,
                                      child_before_write.rss_file_kb, page_kb),
           "layer C: child at fork must not underflow parent RssFile baseline");
    expect(child_before_write.rss_file_kb <=
               parent_file_after_read + page_kb * RSS_TOUCH_TOLERANCE,
           "layer C: child RssFile at fork matches parent within tolerance");
    expect(rss_kb_within_tolerance(parent_anon_after_read,
                                   child_before_write.rss_anon_kb, page_kb),
           "layer C: child RssAnon at fork matches parent within tolerance");

    expect(io_write_all(release[1], "D", 1) == 1, "release fork RW file child for COW write");
    expect(io_read_all(notify[0], &child_after_write, sizeof(child_after_write)) ==
               (ssize_t)sizeof(child_after_write),
           "read child RSS after RW file COW write");
    close(notify[0]);
    close(release[1]);
    expect(child_after_write.pid == child, "child pid matches after COW write");

    snprintf(child_status, sizeof(child_status), "/proc/%d/status", child);
    child_rss_file = read_status_kb_from(child_status, "RssFile");
    child_rss_anon = read_status_kb_from(child_status, "RssAnon");
    expect(child_rss_file == child_after_write.rss_file_kb,
           "child self and /proc RssFile agree after COW write");
    expect(child_rss_anon == child_after_write.rss_anon_kb,
           "child self and /proc RssAnon agree after COW write");

    expect(child_after_write.rss_anon_kb >=
               child_before_write.rss_anon_kb + page_kb,
           "layer D: child COW write increases RssAnon by one page");
    expect(child_after_write.rss_file_kb + page_kb <=
               child_before_write.rss_file_kb,
           "layer D: child COW write transfers RssFile to RssAnon");

    parent_file_after_child = read_self_status_kb("RssFile");
    parent_anon_after_child = read_self_status_kb("RssAnon");
    expect(rss_kb_not_dropped_by_page(parent_file_after_fork, parent_file_after_child,
                                      page_kb),
           "parent RssFile must not drop by a page after child COW write");
    expect(parent_file_after_child <=
               parent_file_after_fork + page_kb * RSS_TOUCH_TOLERANCE,
           "parent RssFile stable within tolerance after child COW write");
    expect(rss_kb_not_dropped_by_page(parent_anon_after_fork, parent_anon_after_child,
                                      page_kb),
           "parent RssAnon must not drop by a page after child COW write");
    expect(parent_anon_after_child <=
               parent_anon_after_fork + page_kb * RSS_TOUCH_TOLERANCE,
           "parent RssAnon stable within tolerance after child COW write");

    expect(waitpid(child, &status, 0) == child, "waitpid fork RW file child");
    expect(WIFEXITED(status) && WEXITSTATUS(status) == 0,
           "fork RW file child exits cleanly");

    munmap(map, (size_t)page_size);
    close(fd);
    unlink(path);
}

static void test_fork_dirty_private_file_cow(long page_size)
{
    char path[] = "/tmp/proc-mem-monitor-fdf-XXXXXX";
    int fd = mkstemp(path);
    long page_kb = page_size / 1024;
    long parent_rss_file_after_write = 0;
    long child_rss_file = 0;
    long child_rss_anon = 0;
    long child_vm_rss = 0;
    int notify[2];
    int release[2];
    unsigned char buf[4096];
    void *map = MAP_FAILED;
    volatile char *page = NULL;
    pid_t child = 0;
    pid_t reported_pid = -1;
    char child_status[64];
    char byte = 0;
    int status = 0;

    expect(fd >= 0, "mkstemp for fork dirty file probe");
    memset(buf, 0x5a, sizeof(buf));
    expect(write(fd, buf, sizeof(buf)) == (ssize_t)sizeof(buf), "write temp file");
    expect(ftruncate(fd, (off_t)sizeof(buf)) == 0, "truncate temp file");

    map = mmap(NULL, (size_t)page_size, PROT_READ, MAP_PRIVATE, fd, 0);
    expect(map != MAP_FAILED, "mmap MAP_PRIVATE file before fork");
    page = (volatile char *)map;
    expect(page[0] == (char)0x5a, "read fault file-backed page");

    expect(mprotect((void *)map, (size_t)page_size, PROT_READ | PROT_WRITE) == 0,
           "mprotect dirty file page writable");
    page[0] = 0x42;
    parent_rss_file_after_write = read_self_status_kb("RssFile");

    expect(pipe(notify) == 0, "notify pipe for fork dirty file sync");
    expect(pipe(release) == 0, "release pipe for fork dirty file sync");

    child = fork();
    expect(child >= 0, "fork after dirty MAP_PRIVATE file page");

    if (child == 0) {
        close(notify[0]);
        close(release[1]);
        page[0] = 0x43;
        reported_pid = getpid();
        if (io_write_all(notify[1], &reported_pid, sizeof(reported_pid)) !=
            (ssize_t)sizeof(reported_pid)) {
            _exit(1);
        }
        close(notify[1]);
        if (io_read_all(release[0], &byte, 1) != 1) {
            _exit(1);
        }
        close(release[0]);
        munmap(map, (size_t)page_size);
        close(fd);
        unlink(path);
        _exit(0);
    }

    close(notify[1]);
    close(release[0]);
    expect(io_read_all(notify[0], &reported_pid, sizeof(reported_pid)) ==
               (ssize_t)sizeof(reported_pid),
           "read child pid after dirty file COW write");
    close(notify[0]);
    expect(reported_pid == child, "child pid matches fork return");

    snprintf(child_status, sizeof(child_status), "/proc/%d/status", child);
    child_rss_file = read_status_kb_from(child_status, "RssFile");
    child_rss_anon = read_status_kb_from(child_status, "RssAnon");
    child_vm_rss = read_status_kb_from(child_status, "VmRSS");
    expect(child_rss_file <=
               parent_rss_file_after_write + page_kb * RSS_TOUCH_TOLERANCE,
           "child RssFile must not underflow after dirty file COW");
    expect(child_rss_anon > 0, "child RssAnon positive after dirty file COW");
    expect(child_vm_rss > 0, "child VmRSS positive after dirty file COW");

    expect(io_write_all(release[1], "D", 1) == 1, "release fork dirty file child");
    close(release[1]);
    expect(waitpid(child, &status, 0) == child, "waitpid fork dirty file child");
    expect(WIFEXITED(status) && WEXITSTATUS(status) == 0,
           "fork dirty file child exits cleanly");

    munmap(map, (size_t)page_size);
    close(fd);
    unlink(path);
}

static void test_fork_mprotect_only_sibling_unmap(long page_size)
{
    char path[] = "/tmp/proc-mem-monitor-mp-XXXXXX";
    int fd = mkstemp(path);
    long page_kb = page_size / 1024;
    long parent_rss_file_after_fault = 0;
    long child_rss_file_at_fork = 0;
    long child_rss_file_after_mprotect = 0;
    long parent_rss_file_after_child_unmap = 0;
    int notify[2];
    int release[2];
    unsigned char buf[4096];
    void *map = MAP_FAILED;
    volatile char *page = NULL;
    pid_t child = 0;
    pid_t reported_pid = -1;
    char child_status[64];
    char byte = 0;
    int status = 0;

    expect(fd >= 0, "mkstemp for fork mprotect-only probe");
    memset(buf, 0x5a, sizeof(buf));
    expect(write(fd, buf, sizeof(buf)) == (ssize_t)sizeof(buf), "write temp file");
    expect(ftruncate(fd, (off_t)sizeof(buf)) == 0, "truncate temp file");

    map = mmap(NULL, (size_t)page_size, PROT_READ, MAP_PRIVATE, fd, 0);
    expect(map != MAP_FAILED, "mmap MAP_PRIVATE file before fork");
    page = (volatile char *)map;
    expect(page[0] == (char)0x5a, "read fault file-backed page");
    parent_rss_file_after_fault = read_self_status_kb("RssFile");

    expect(pipe(notify) == 0, "notify pipe for mprotect-only sync");
    expect(pipe(release) == 0, "release pipe for mprotect-only sync");

    child = fork();
    expect(child >= 0, "fork for mprotect-only sibling probe");

    if (child == 0) {
        close(notify[0]);
        close(release[1]);
        snprintf(child_status, sizeof(child_status), "/proc/%d/status", getpid());
        child_rss_file_at_fork = read_status_kb_from(child_status, "RssFile");
        expect(mprotect((void *)map, (size_t)page_size, PROT_READ | PROT_WRITE) == 0,
               "child mprotect writable without write");
        child_rss_file_after_mprotect = read_status_kb_from(child_status, "RssFile");
        expect(child_rss_file_after_mprotect + page_kb * RSS_TOUCH_TOLERANCE >=
                   child_rss_file_at_fork,
               "child mprotect-only must not drop RssFile");
        expect(child_rss_file_after_mprotect <=
                   child_rss_file_at_fork + page_kb * RSS_TOUCH_TOLERANCE,
               "child mprotect-only must not reclassify to RssAnon");
        reported_pid = getpid();
        if (io_write_all(notify[1], &reported_pid, sizeof(reported_pid)) !=
            (ssize_t)sizeof(reported_pid)) {
            _exit(1);
        }
        close(notify[1]);
        if (io_read_all(release[0], &byte, 1) != 1) {
            _exit(1);
        }
        close(release[0]);
        munmap(map, (size_t)page_size);
        close(fd);
        unlink(path);
        _exit(0);
    }

    close(notify[1]);
    close(release[0]);
    expect(io_read_all(notify[0], &reported_pid, sizeof(reported_pid)) ==
               (ssize_t)sizeof(reported_pid),
           "read child pid after mprotect-only");
    close(notify[0]);
    expect(reported_pid == child, "child pid matches fork return");

    munmap(map, (size_t)page_size);
    parent_rss_file_after_child_unmap = read_self_status_kb("RssFile");
    expect(parent_rss_file_after_child_unmap + page_kb <=
               parent_rss_file_after_fault + page_kb * RSS_TOUCH_TOLERANCE,
           "parent unmap decreases RssFile after mprotect-only sibling");

    expect(io_write_all(release[1], "D", 1) == 1, "release mprotect-only child");
    close(release[1]);
    expect(waitpid(child, &status, 0) == child, "waitpid mprotect-only child");
    expect(WIFEXITED(status) && WEXITSTATUS(status) == 0,
           "mprotect-only child exits cleanly");

    close(fd);
    unlink(path);
}

static void test_fork_reclassify_writer_sibling_unmap(long page_size)
{
    char path[] = "/tmp/proc-mem-monitor-rw-XXXXXX";
    int fd = mkstemp(path);
    long page_kb = page_size / 1024;
    long parent_rss_file_after_fault = 0;
    long parent_rss_anon_after_fault = 0;
    long child_rss_file = 0;
    long child_rss_anon = 0;
    long parent_rss_file_after_unmap = 0;
    int notify[2];
    int release[2];
    unsigned char buf[4096];
    void *map = MAP_FAILED;
    volatile char *page = NULL;
    pid_t child = 0;
    pid_t reported_pid = -1;
    char child_status[64];
    char byte = 0;
    int status = 0;

    expect(fd >= 0, "mkstemp for fork reclassify writer probe");
    memset(buf, 0x5a, sizeof(buf));
    expect(write(fd, buf, sizeof(buf)) == (ssize_t)sizeof(buf), "write temp file");
    expect(ftruncate(fd, (off_t)sizeof(buf)) == 0, "truncate temp file");

    map = mmap(NULL, (size_t)page_size, PROT_READ, MAP_PRIVATE, fd, 0);
    expect(map != MAP_FAILED, "mmap MAP_PRIVATE file before fork");
    page = (volatile char *)map;
    expect(page[0] == (char)0x5a, "read fault file-backed page");
    parent_rss_file_after_fault = read_self_status_kb("RssFile");
    parent_rss_anon_after_fault = read_self_status_kb("RssAnon");
    expect(pipe(notify) == 0, "notify pipe for reclassify writer sync");
    expect(pipe(release) == 0, "release pipe for reclassify writer sync");

    child = fork();
    expect(child >= 0, "fork for reclassify writer sibling probe");

    if (child == 0) {
        close(notify[0]);
        close(release[1]);
        expect(mprotect((void *)map, (size_t)page_size, PROT_READ | PROT_WRITE) == 0,
               "child mprotect writable");
        page[0] = 0x43;
        reported_pid = getpid();
        if (io_write_all(notify[1], &reported_pid, sizeof(reported_pid)) !=
            (ssize_t)sizeof(reported_pid)) {
            _exit(1);
        }
        close(notify[1]);
        if (io_read_all(release[0], &byte, 1) != 1) {
            _exit(1);
        }
        close(release[0]);
        close(fd);
        _exit(0);
    }

    close(notify[1]);
    close(release[0]);
    expect(io_read_all(notify[0], &reported_pid, sizeof(reported_pid)) ==
               (ssize_t)sizeof(reported_pid),
           "read child pid after reclassify write");
    close(notify[0]);
    expect(reported_pid == child, "child pid matches fork return");

    snprintf(child_status, sizeof(child_status), "/proc/%d/status", child);
    child_rss_file = read_status_kb_from(child_status, "RssFile");
    child_rss_anon = read_status_kb_from(child_status, "RssAnon");
    expect(child_rss_anon >= parent_rss_anon_after_fault + page_kb,
           "child COW write increases RssAnon");
    expect(child_rss_file + page_kb <=
               parent_rss_file_after_fault + page_kb * RSS_TOUCH_TOLERANCE,
           "child RssFile unchanged or lower after own COW write");

    munmap(map, (size_t)page_size);
    parent_rss_file_after_unmap = read_self_status_kb("RssFile");
    expect(parent_rss_file_after_unmap + page_kb <=
               parent_rss_file_after_fault + page_kb * RSS_TOUCH_TOLERANCE,
           "parent unmap still decrements RssFile (page stayed File in parent)");

    expect(io_write_all(release[1], "D", 1) == 1, "release reclassify writer child");
    close(release[1]);
    expect(waitpid(child, &status, 0) == child, "waitpid reclassify writer child");
    expect(WIFEXITED(status) && WEXITSTATUS(status) == 0,
           "reclassify writer child exits cleanly");

    close(fd);
    unlink(path);
}

static void test_mremap_move_private_file_rss(long page_size)
{
    char path[] = "/tmp/proc-mem-monitor-mr-XXXXXX";
    int fd = mkstemp(path);
    long page_kb = page_size / 1024;
    long rss_file_before = 0;
    long rss_file_after = 0;
    long rss_anon_before = 0;
    long rss_anon_after = 0;
    unsigned char buf[4096];
    void *map = MAP_FAILED;
    void *moved = MAP_FAILED;
    volatile char *page = NULL;

    expect(fd >= 0, "mkstemp for mremap move probe");
    memset(buf, 0x5a, sizeof(buf));
    expect(write(fd, buf, sizeof(buf)) == (ssize_t)sizeof(buf), "write temp file");
    expect(ftruncate(fd, (off_t)sizeof(buf)) == 0, "truncate temp file");

    rss_file_before = read_self_status_kb("RssFile");
    rss_anon_before = read_self_status_kb("RssAnon");

    map = mmap(NULL, (size_t)page_size, PROT_READ, MAP_PRIVATE, fd, 0);
    expect(map != MAP_FAILED, "mmap MAP_PRIVATE file for mremap");
    page = (volatile char *)map;
    expect(page[0] == (char)0x5a, "read fault before mremap");

    moved = mremap(map, (size_t)page_size, (size_t)page_size, MREMAP_MAYMOVE);
    expect(moved != MAP_FAILED, "mremap move private file mapping");
    page = (volatile char *)moved;
    expect(page[0] == (char)0x5a, "mremap preserves file page content");

    rss_file_after = read_self_status_kb("RssFile");
    rss_anon_after = read_self_status_kb("RssAnon");
    expect(rss_file_after + page_kb * RSS_TOUCH_TOLERANCE >= rss_file_before + page_kb,
           "mremap move preserves RssFile charge");
    expect(rss_anon_after <= rss_anon_before + page_kb * RSS_TOUCH_TOLERANCE,
           "mremap move does not inflate RssAnon");

    munmap(moved, (size_t)page_size);
    close(fd);
    unlink(path);
}

static void test_mprotect_idempotent(long page_size)
{
    char path[] = "/tmp/proc-mem-monitor-id-XXXXXX";
    int fd = mkstemp(path);
    long page_kb = page_size / 1024;
    long rss_file_before = 0;
    long rss_file_after = 0;
    long rss_anon_before = 0;
    long rss_anon_after = 0;
    unsigned char buf[4096];
    void *map = MAP_FAILED;
    volatile char *page = NULL;
    int i = 0;

    expect(fd >= 0, "mkstemp for mprotect idempotent probe");
    memset(buf, 0x5a, sizeof(buf));
    expect(write(fd, buf, sizeof(buf)) == (ssize_t)sizeof(buf), "write temp file");
    expect(ftruncate(fd, (off_t)sizeof(buf)) == 0, "truncate temp file");

    map = mmap(NULL, (size_t)page_size, PROT_READ, MAP_PRIVATE, fd, 0);
    expect(map != MAP_FAILED, "mmap MAP_PRIVATE file for mprotect idempotent");
    page = (volatile char *)map;
    expect(page[0] == (char)0x5a, "read fault before repeated mprotect");

    rss_file_before = read_self_status_kb("RssFile");
    rss_anon_before = read_self_status_kb("RssAnon");

    for (i = 0; i < 5; i++) {
        expect(mprotect((void *)map, (size_t)page_size, PROT_READ | PROT_WRITE) == 0,
               "repeated mprotect writable");
        expect(mprotect((void *)map, (size_t)page_size, PROT_READ) == 0,
               "repeated mprotect read-only");
    }

    rss_file_after = read_self_status_kb("RssFile");
    rss_anon_after = read_self_status_kb("RssAnon");
    expect(rss_file_after + page_kb * RSS_TOUCH_TOLERANCE >= rss_file_before,
           "repeated mprotect without write keeps RssFile");
    expect(rss_file_after <= rss_file_before + page_kb * RSS_TOUCH_TOLERANCE,
           "repeated mprotect without write does not inflate RssFile");
    expect(rss_anon_after <= rss_anon_before + page_kb * RSS_TOUCH_TOLERANCE,
           "repeated mprotect without write does not inflate RssAnon");

    munmap(map, (size_t)page_size);
    close(fd);
    unlink(path);
}

static void test_shmem_rss_classification(long page_size)
{
    long page_kb = page_size / 1024;
    long shmem_before = 0;
    long shmem_after = 0;
    int fd = 0;
    void *map = MAP_FAILED;
    volatile char *page = NULL;

    fd = memfd_create_sys("rss-test", MFD_CLOEXEC);
    expect(fd >= 0, "memfd_create for RssShmem probe");
    expect(ftruncate(fd, (off_t)page_size) == 0, "memfd ftruncate");

    shmem_before = read_self_status_kb("RssShmem");
    map = mmap(NULL, (size_t)page_size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    expect(map != MAP_FAILED, "mmap MAP_SHARED memfd");
    page = (volatile char *)map;
    page[0] = 1;
    shmem_after = read_self_status_kb("RssShmem");
    expect(shmem_after >= shmem_before + page_kb,
           "shared mmap fault increases RssShmem");

    munmap(map, (size_t)page_size);
    close(fd);
}

static void test_fork_cow_rss(long page_size)
{
    int fork_ready[2];
    int resume[2];
    int notify[2];
    int release[2];
    unsigned long parent_r0 = 0;
    unsigned long parent_r1 = 0;
    unsigned long parent_r_after = 0;
    unsigned long child_r_at_fork = 0;
    unsigned long child_r_after_write = 0;
    unsigned long shared = 0;
    unsigned long text = 0;
    unsigned long data = 0;
    unsigned long statm_size = 0;
    void *shared_map = MAP_FAILED;
    volatile char *page = NULL;
    pid_t child = 0;
    pid_t reported_pid = -1;
    char child_statm[64];
    char byte = 0;
    int status = 0;

    read_statm_fields_from(
        "/proc/self/statm",
        &statm_size,
        &parent_r0,
        &shared,
        &text,
        &data
    );

    shared_map = mmap(NULL, (size_t)page_size, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    expect(shared_map != MAP_FAILED, "mmap shared page before fork");
    page = (volatile char *)shared_map;
    page[0] = 0x11;

    read_statm_fields_from(
        "/proc/self/statm",
        &statm_size,
        &parent_r1,
        &shared,
        &text,
        &data
    );
    expect(parent_r1 >= parent_r0 + 1, "parent fault increases RSS by one page");

    expect(pipe(fork_ready) == 0, "fork_ready pipe for COW sync");
    expect(pipe(resume) == 0, "resume pipe for COW sync");
    expect(pipe(notify) == 0, "notify pipe for COW sync");
    expect(pipe(release) == 0, "release pipe for COW sync");

    child = fork();
    expect(child >= 0, "fork for COW RSS probe");

    if (child == 0) {
        close(fork_ready[0]);
        close(resume[1]);
        close(notify[0]);
        close(release[1]);

        if (io_write_all(fork_ready[1], "R", 1) != 1) {
            _exit(1);
        }
        close(fork_ready[1]);
        if (io_read_all(resume[0], &byte, 1) != 1) {
            _exit(1);
        }
        close(resume[0]);

        page[0] = 0x22;
        reported_pid = getpid();
        if (io_write_all(notify[1], &reported_pid, sizeof(reported_pid)) !=
            (ssize_t)sizeof(reported_pid)) {
            _exit(1);
        }
        close(notify[1]);
        if (io_read_all(release[0], &byte, 1) != 1) {
            _exit(1);
        }
        close(release[0]);
        munmap(shared_map, (size_t)page_size);
        _exit(0);
    }

    close(fork_ready[1]);
    close(resume[0]);
    close(notify[1]);
    close(release[0]);

    expect(io_read_all(fork_ready[0], &byte, 1) == 1, "child ready at fork boundary");
    close(fork_ready[0]);

    snprintf(child_statm, sizeof(child_statm), "/proc/%d/statm", child);
    read_statm_fields_from(
        child_statm,
        &statm_size,
        &child_r_at_fork,
        &shared,
        &text,
        &data
    );
    expect(child_r_at_fork + RSS_TOUCH_TOLERANCE >= parent_r1,
           "child RSS after fork includes cloned shared page");
    expect(child_r_at_fork <= statm_size, "child resident <= size at fork");

    expect(io_write_all(resume[1], "G", 1) == 1, "resume child for COW write");
    close(resume[1]);

    expect(io_read_all(notify[0], &reported_pid, sizeof(reported_pid)) ==
               (ssize_t)sizeof(reported_pid),
           "read child pid after COW write");
    close(notify[0]);
    expect(reported_pid == child, "child pid matches fork return");

    read_statm_fields_from(
        child_statm,
        &statm_size,
        &child_r_after_write,
        &shared,
        &text,
        &data
    );
    read_statm_fields_from(
        "/proc/self/statm",
        &statm_size,
        &parent_r_after,
        &shared,
        &text,
        &data
    );
    expect(parent_r_after + RSS_TOUCH_TOLERANCE >= parent_r1,
           "parent RSS unchanged after child COW write");
    expect(child_r_after_write + RSS_TOUCH_TOLERANCE >= parent_r1,
           "child RSS still covers cloned page after COW write");
    expect(child_r_after_write <= child_r_at_fork + RSS_TOUCH_TOLERANCE,
           "anon COW write does not increase child RSS total");

    byte = 1;
    expect(io_write_all(release[1], &byte, 1) == 1, "release child");
    close(release[1]);
    expect(waitpid(child, &status, 0) == child, "waitpid child");
    expect(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child exits cleanly");

    munmap(shared_map, (size_t)page_size);
    (void)page_size;
}

static void test_self_proc_memory(long page_size)
{
    long vm_size_kb = 0;
    long vm_rss_kb = 0;
    long vm_data_kb = 0;
    long vm_stk_kb = 0;
    unsigned long statm_size = 0;
    unsigned long statm_resident = 0;
    unsigned long statm_shared = 0;
    unsigned long statm_text = 0;
    unsigned long statm_data = 0;
    unsigned long statm_size_after = 0;
    unsigned long vsize = 0;
    unsigned long rss = 0;
    void *probe = MAP_FAILED;

    vm_size_kb = read_status_kb_from("/proc/self/status", "VmSize");
    vm_rss_kb = read_status_kb_from("/proc/self/status", "VmRSS");
    vm_data_kb = read_status_kb_from("/proc/self/status", "VmData");
    vm_stk_kb = read_status_kb_from("/proc/self/status", "VmStk");
    expect(vm_size_kb > 0, "VmSize must be positive");
    expect(vm_rss_kb > 0, "VmRSS must be positive");
    expect(vm_data_kb >= 0, "VmData must be non-negative");
    expect(vm_stk_kb > 0, "VmStk must be positive");
    expect(vm_rss_kb <= vm_size_kb, "VmRSS must not exceed VmSize");

    read_statm_fields_from(
        "/proc/self/statm",
        &statm_size,
        &statm_resident,
        &statm_shared,
        &statm_text,
        &statm_data
    );
    expect(statm_size > 0, "statm size must be positive");
    expect(statm_resident > 0, "statm resident must be positive");
    expect(statm_resident <= statm_size, "statm resident must not exceed size");
    expect(statm_size == (unsigned long)vm_size_kb * 1024UL / (unsigned long)page_size,
           "statm size matches VmSize");

    vsize = read_stat_field_after_comm_from("/proc/self/stat", 20);
    rss = read_stat_field_after_comm_from("/proc/self/stat", 21);
    read_statm_fields_from(
        "/proc/self/statm",
        &statm_size,
        &statm_resident,
        &statm_shared,
        &statm_text,
        &statm_data
    );
    expect(vsize > 0, "stat vsize must be positive");
    expect(vsize == statm_size * (unsigned long)page_size, "stat vsize matches statm size");
    expect(rss == statm_resident, "stat rss pages match statm resident");

    probe = mmap(NULL, MMAP_PROBE_SIZE, PROT_READ | PROT_WRITE,
                 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    expect(probe != MAP_FAILED, "mmap probe mapping");

    read_statm_fields_from(
        "/proc/self/statm",
        &statm_size_after,
        &statm_resident,
        &statm_shared,
        &statm_text,
        &statm_data
    );
    expect(statm_size_after >= statm_size + MMAP_PROBE_SIZE / (unsigned long)page_size,
           "statm size grows after mmap");

    munmap(probe, MMAP_PROBE_SIZE);
}

static void test_child_proc_memory(long page_size, unsigned long parent_statm_size)
{
    int notify[2];
    int release[2];
    expect(pipe(notify) == 0, "notify pipe for fork sync");
    expect(pipe(release) == 0, "release pipe for fork sync");

    pid_t child = fork();
    expect(child >= 0, "fork child for /proc/<pid> probe");

    if (child == 0) {
        void *child_map = mmap(NULL, MMAP_PROBE_SIZE, PROT_READ | PROT_WRITE,
                               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        volatile char *page = NULL;
        if (child_map == MAP_FAILED) {
            _exit(1);
        }
        page = (volatile char *)child_map;
        page[0] = 1;

        close(notify[0]);
        close(release[1]);

        pid_t self = getpid();
        if (io_write_all(notify[1], &self, sizeof(self)) != (ssize_t)sizeof(self)) {
            _exit(1);
        }
        close(notify[1]);

        char byte = 0;
        if (io_read_all(release[0], &byte, 1) != 1) {
            _exit(1);
        }
        close(release[0]);
        munmap(child_map, MMAP_PROBE_SIZE);
        _exit(0);
    }

    close(notify[1]);
    close(release[0]);

    pid_t reported_pid = -1;
    expect(io_read_all(notify[0], &reported_pid, sizeof(reported_pid)) == (ssize_t)sizeof(reported_pid),
           "read child pid from pipe");
    close(notify[0]);
    expect(reported_pid == child, "child reports its own pid");

    char child_status[64];
    char child_statm[64];
    snprintf(child_status, sizeof(child_status), "/proc/%d/status", child);
    snprintf(child_statm, sizeof(child_statm), "/proc/%d/statm", child);

    expect(read_status_pid_from(child_status, "Pid") == (long)child,
           "child status Pid matches fork return");
    expect(read_status_pid_from(child_status, "Tgid") == (long)child,
           "child status Tgid matches fork return");
    expect(read_status_kb_from(child_status, "VmSize") > 0,
           "child VmSize must be positive");
    expect(read_status_kb_from(child_status, "VmRSS") > 0,
           "child VmRSS must be positive");

    unsigned long child_size = 0;
    unsigned long child_resident = 0;
    unsigned long child_shared = 0;
    unsigned long child_text = 0;
    unsigned long child_data = 0;
    read_statm_fields_from(
        child_statm,
        &child_size,
        &child_resident,
        &child_shared,
        &child_text,
        &child_data
    );
    expect(child_size > parent_statm_size, "child statm size exceeds parent baseline");
    expect(child_resident > 0, "child statm resident must be positive");
    expect(child_resident <= child_size, "child statm resident must not exceed size");
    expect(child_size == (unsigned long)read_status_kb_from(child_status, "VmSize")
                              * 1024UL / (unsigned long)page_size,
           "child statm size matches VmSize");

    char byte = 1;
    expect(io_write_all(release[1], &byte, 1) == 1, "release child from pipe sync");
    close(release[1]);

    int status = 0;
    expect(waitpid(child, &status, 0) == child, "waitpid reaps child");
    expect(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child exits cleanly");
}

int main(void)
{
    long page_size = 0;
    unsigned long parent_statm_size = 0;
    unsigned long parent_statm_resident = 0;
    unsigned long parent_statm_shared = 0;
    unsigned long parent_statm_text = 0;
    unsigned long parent_statm_data = 0;

    page_size = sysconf(_SC_PAGESIZE);
    expect(page_size > 0, "sysconf(_SC_PAGESIZE) must be positive");

    read_statm_fields_from(
        "/proc/self/statm",
        &parent_statm_size,
        &parent_statm_resident,
        &parent_statm_shared,
        &parent_statm_text,
        &parent_statm_data
    );

    test_self_proc_memory(page_size);
    test_lazy_and_touch_rss(page_size);
    test_map_private_file_rss(page_size);
    test_map_private_file_write_first_fault(page_size);
    test_map_private_file_read_then_write(page_size);
    test_fork_rw_file_read_child_write(page_size);
    test_fork_mprotect_only_sibling_unmap(page_size);
    test_fork_reclassify_writer_sibling_unmap(page_size);
    test_mremap_move_private_file_rss(page_size);
    test_mprotect_idempotent(page_size);
    test_fork_dirty_private_file_cow(page_size);
    test_shmem_rss_classification(page_size);
    test_fork_cow_rss(page_size);
    test_child_proc_memory(page_size, parent_statm_size);
    expect(read_meminfo_kb("PageTables") >= 0, "PageTables field is parseable");

    puts("TEST PASSED");
    return 0;
}
