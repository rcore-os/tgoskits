/*
 * test-proc-mem-monitor: validate process memory monitoring via /proc.
 *
 * Covers user-space scenarios used by psutil, top, and glances:
 *   - /proc/self/status  VmSize / VmRSS / VmData / VmStk
 *   - /proc/self/statm   seven-field layout and cross-field consistency
 *   - /proc/self/stat    vsize (bytes) and rss (pages) consistency
 *   - mmap growth        statm size increases after anonymous mapping
 *   - /proc/<child>      parent reads child memory stats after fork()
 *   - /proc/meminfo      PageTables field is parseable (allocator-backed)
 *
 * Long-term invariants (must hold in Plan1 and Plan2):
 *   - VmRSS > 0, VmSize > 0, VmRSS <= VmSize
 *   - statm resident > 0, statm size > 0, resident <= size
 *   - stat vsize == statm size * page_size; stat rss == statm resident
 *   - statm size (pages) == VmSize (kB) converted with sysconf(_SC_PAGESIZE)
 *
 * Do NOT assert VmRSS == VmSize or resident == size: Plan2 real RSS may be
 * strictly less than VSS. Equality today is a temporary Plan1 upper-bound
 * shortcut, not a permanent kernel/userspace contract.
 */
#define _GNU_SOURCE

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

enum { MMAP_PROBE_SIZE = 2 * 1024 * 1024 };

static void expect(int condition, const char *message)
{
    if (!condition) {
        fputs("FAIL: ", stderr);
        fputs(message, stderr);
        fputc('\n', stderr);
        abort();
    }
}

static long read_status_kb_from(const char *path, const char *key)
{
    FILE *fp = fopen(path, "r");
    expect(fp != NULL, path);

    char line[256];
    char prefix[64];
    int found = 0;
    long value = -1;

    snprintf(prefix, sizeof(prefix), "%s:\t", key);
    while (fgets(line, sizeof(line), fp) != NULL) {
        if (strncmp(line, prefix, strlen(prefix)) != 0) {
            continue;
        }
        expect(sscanf(line + strlen(prefix), "%ld kB", &value) == 1, key);
        found = 1;
        break;
    }

    fclose(fp);
    expect(found, "missing status key");
    return value;
}

static long read_status_pid_from(const char *path, const char *key)
{
    FILE *fp = fopen(path, "r");
    expect(fp != NULL, path);

    char line[256];
    char prefix[64];
    int found = 0;
    long value = -1;

    snprintf(prefix, sizeof(prefix), "%s:\t", key);
    while (fgets(line, sizeof(line), fp) != NULL) {
        if (strncmp(line, prefix, strlen(prefix)) != 0) {
            continue;
        }
        expect(sscanf(line + strlen(prefix), "%ld", &value) == 1, key);
        found = 1;
        break;
    }

    fclose(fp);
    expect(found, "missing status pid key");
    return value;
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
    FILE *fp = fopen(path, "r");
    expect(fp != NULL, path);

    unsigned long lib = 0;
    unsigned long dirty = 0;
    int n = fscanf(
        fp,
        "%lu %lu %lu %lu %lu %lu %lu",
        size,
        resident,
        shared,
        text,
        &lib,
        data,
        &dirty
    );
    fclose(fp);

    expect(n == 7, "statm must expose seven integer fields");
}

static unsigned long read_stat_field_after_comm_from(const char *path, unsigned token_index)
{
    FILE *fp = fopen(path, "r");
    expect(fp != NULL, path);

    char line[4096];
    expect(fgets(line, sizeof(line), fp) != NULL, "read proc stat");
    fclose(fp);

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
    FILE *fp = fopen("/proc/meminfo", "r");
    expect(fp != NULL, "open /proc/meminfo");

    char line[256];
    char prefix[64];
    int found = 0;
    long value = -1;

    snprintf(prefix, sizeof(prefix), "%s:", key);
    while (fgets(line, sizeof(line), fp) != NULL) {
        if (strncmp(line, prefix, strlen(prefix)) != 0) {
            continue;
        }
        expect(sscanf(line + strlen(prefix), "%ld kB", &value) == 1, key);
        found = 1;
        break;
    }

    fclose(fp);
    expect(found, "missing meminfo key");
    return value;
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
        if (child_map == MAP_FAILED) {
            _exit(1);
        }

        close(notify[0]);
        close(release[1]);

        pid_t self = getpid();
        if (write(notify[1], &self, sizeof(self)) != (ssize_t)sizeof(self)) {
            _exit(1);
        }
        close(notify[1]);

        char byte = 0;
        if (read(release[0], &byte, 1) != 1) {
            _exit(1);
        }
        close(release[0]);
        munmap(child_map, MMAP_PROBE_SIZE);
        _exit(0);
    }

    close(notify[1]);
    close(release[0]);

    pid_t reported_pid = -1;
    expect(read(notify[0], &reported_pid, sizeof(reported_pid)) == (ssize_t)sizeof(reported_pid),
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
    expect(write(release[1], &byte, 1) == 1, "release child from pipe sync");
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
    test_child_proc_memory(page_size, parent_statm_size);
    expect(read_meminfo_kb("PageTables") >= 0, "PageTables field is parseable");

    puts("TEST PASSED");
    return 0;
}
