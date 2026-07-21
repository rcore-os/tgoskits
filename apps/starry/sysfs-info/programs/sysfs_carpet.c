/*
 * sysfs_carpet - doc-grounded carpet test for StarryOS /sys CPU topology,
 * per-CPU cache geometry, and per-NUMA-node meminfo.
 *
 * Ground truth: Documentation/ABI/testing/sysfs-devices-system-cpu and
 * drivers/base/node.c:node_read_meminfo() (Linux). Each assertion prints the
 * real value it read so the harness log shows actual cache sizes, MemFree,
 * topology masks, etc. The final line is a self-counted "OK=n/n" plus a
 * TEST PASSED / TEST FAILED verdict the qemu success_regex keys on.
 *
 * Arch-aware: on x86_64/aarch64/loongarch64 the kernel enumerates real cache
 * leaves from architecture registers (x86_64: CPUID leaf 4, with a legacy leaf
 * 0x2 descriptor-table fallback; aarch64: CLIDR/CCSIDR; loongarch64: CPUCFG),
 * so cache/index0.. must be present and well-formed. On riscv64 there is no
 * cache-geometry register source (Linux uses the device tree only), so the
 * kernel deliberately omits cache/; the carpet asserts that absence instead of
 * failing.
 */

#include <ctype.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/utsname.h>
#include <unistd.h>

static int g_ok;
static int g_total;

static void check(int cond, const char *what) {
    g_total++;
    if (cond) {
        g_ok++;
        printf("  [ ok ] %s\n", what);
    } else {
        printf("  [FAIL] %s\n", what);
    }
}

/* Read a whole sysfs file into buf (NUL-terminated). Returns 0 on success. */
static int read_file(const char *path, char *buf, size_t cap) {
    FILE *f = fopen(path, "r");
    if (!f) {
        return -1;
    }
    size_t n = fread(buf, 1, cap - 1, f);
    fclose(f);
    buf[n] = '\0';
    /* strip a single trailing newline for tidy comparisons */
    if (n > 0 && buf[n - 1] == '\n') {
        buf[n - 1] = '\0';
    }
    return 0;
}

static int path_exists(const char *path) {
    struct stat st;
    return stat(path, &st) == 0;
}

static int dir_exists(const char *path) {
    struct stat st;
    return stat(path, &st) == 0 && S_ISDIR(st.st_mode);
}

/* Parse a leading unsigned long from a sysfs value string. -1 on failure. */
static long parse_ulong(const char *s) {
    char *end = NULL;
    long v = strtol(s, &end, 10);
    if (end == s) {
        return -1;
    }
    return v;
}

/*
 * Detect the running architecture from uname(). We do not rely on a compile
 * define because a single binary staged per-arch is simpler to reason about,
 * and uname is the ground truth the kernel actually reports.
 */
enum arch { ARCH_X86_64, ARCH_AARCH64, ARCH_LOONGARCH64, ARCH_RISCV64, ARCH_OTHER };

static enum arch detect_arch(void) {
    struct utsname u;
    if (uname(&u) != 0) {
        return ARCH_OTHER;
    }
    printf("uname machine: %s\n", u.machine);
    if (strstr(u.machine, "x86_64") || strstr(u.machine, "amd64")) {
        return ARCH_X86_64;
    }
    if (strstr(u.machine, "aarch64") || strstr(u.machine, "arm64")) {
        return ARCH_AARCH64;
    }
    if (strstr(u.machine, "loongarch")) {
        return ARCH_LOONGARCH64;
    }
    if (strstr(u.machine, "riscv")) {
        return ARCH_RISCV64;
    }
    return ARCH_OTHER;
}

static const char *CPU0 = "/sys/devices/system/cpu/cpu0";
static const char *NODE0 = "/sys/devices/system/node/node0";

/* ---- cache/index<N> geometry (ABI: sysfs-devices-system-cpu cache/index*) ---- */

static void test_cache_present(enum arch a) {
    char cache_dir[256];
    snprintf(cache_dir, sizeof cache_dir, "%s/cache", CPU0);

    printf("\n== cpu0/cache (arch enumerates real cache leaves) ==\n");

    /*
     * On x86_64/aarch64/loongarch64 the kernel enumerates the cache hierarchy
     * from architecture registers, so cache/ MUST be present. x86_64 reads
     * CPUID leaf 4 (deterministic cache parameters) and, when that is
     * unavailable, falls back to the legacy CPUID leaf 0x2 descriptor table -
     * exactly like arch/x86/kernel/cpu/cacheinfo.c (intel_cacheinfo_0x2). The
     * x86 QEMU model (Haswell) populates leaf 4, so index0.. carry full
     * geometry (size/line/sets/ways). aarch64/loongarch64 read CLIDR/CCSIDR and
     * CPUCFG, which are always readable.
     */
    check(dir_exists(cache_dir), "cpu0/cache/ directory present");

    char idx0[256];
    snprintf(idx0, sizeof idx0, "%s/cache/index0", CPU0);
    check(dir_exists(idx0), "cpu0/cache/index0/ present (at least L1 enumerated)");
    if (!dir_exists(idx0)) {
        return;
    }

    /* Walk every index<N> until absent, asserting each leaf's shape. Also
     * verify index0 is specifically an L1 (level==1) private cache. */
    int found_l1_private = 0;
    for (int i = 0;; i++) {
        char base[256];
        snprintf(base, sizeof base, "%s/cache/index%d", CPU0, i);
        if (!dir_exists(base)) {
            if (i == 0) {
                check(0, "cache/index0 exists");
            }
            printf("  (enumerated %d cache leaf/leaves)\n", i);
            break;
        }

        char path[320], val[128];

        snprintf(path, sizeof path, "%s/level", base);
        long level = -1;
        if (read_file(path, val, sizeof val) == 0) {
            level = parse_ulong(val);
        }
        printf("  index%d/level = %ld\n", i, level);

        snprintf(path, sizeof path, "%s/type", base);
        char ctype[64] = "";
        read_file(path, ctype, sizeof ctype);
        printf("  index%d/type = %s\n", i, ctype);
        int type_ok = (strcmp(ctype, "Data") == 0 || strcmp(ctype, "Instruction") == 0 ||
                       strcmp(ctype, "Unified") == 0);

        snprintf(path, sizeof path, "%s/size", base);
        char size_s[64] = "";
        read_file(path, size_s, sizeof size_s);
        printf("  index%d/size = %s\n", i, size_s);
        size_t slen = strlen(size_s);
        long size_num = parse_ulong(size_s);
        /* ABI: "total cache size in kB", size_show() prints "%uK" */
        int size_ok = slen > 1 && size_s[slen - 1] == 'K' && size_num > 0;

        snprintf(path, sizeof path, "%s/coherency_line_size", base);
        long line = -1;
        if (read_file(path, val, sizeof val) == 0) {
            line = parse_ulong(val);
        }
        printf("  index%d/coherency_line_size = %ld\n", i, line);

        snprintf(path, sizeof path, "%s/number_of_sets", base);
        long sets = -1;
        if (read_file(path, val, sizeof val) == 0) {
            sets = parse_ulong(val);
        }
        printf("  index%d/number_of_sets = %ld\n", i, sets);

        snprintf(path, sizeof path, "%s/ways_of_associativity", base);
        long ways = -1;
        if (read_file(path, val, sizeof val) == 0) {
            ways = parse_ulong(val);
        }
        printf("  index%d/ways_of_associativity = %ld\n", i, ways);

        /* physical_line_partition: lines per tag. Absent => treat as 1 (the
         * single-partition default the size identity assumes). */
        snprintf(path, sizeof path, "%s/physical_line_partition", base);
        long part = 1;
        if (read_file(path, val, sizeof val) == 0) {
            long p = parse_ulong(val);
            if (p > 0) {
                part = p;
            }
        }
        printf("  index%d/physical_line_partition = %ld\n", i, part);

        snprintf(path, sizeof path, "%s/shared_cpu_map", base);
        char scm[128] = "";
        read_file(path, scm, sizeof scm);
        printf("  index%d/shared_cpu_map = %s\n", i, scm);

        snprintf(path, sizeof path, "%s/shared_cpu_list", base);
        char scl[128] = "";
        read_file(path, scl, sizeof scl);
        printf("  index%d/shared_cpu_list = %s\n", i, scl);

        char what[96];
        snprintf(what, sizeof what, "index%d level>0", i);
        check(level > 0, what);
        snprintf(what, sizeof what, "index%d type in {Data,Instruction,Unified}", i);
        check(type_ok, what);
        snprintf(what, sizeof what, "index%d size ends 'K' and >0", i);
        check(size_ok, what);
        snprintf(what, sizeof what, "index%d coherency_line_size>0", i);
        check(line > 0, what);
        snprintf(what, sizeof what, "index%d number_of_sets>0", i);
        check(sets > 0, what);
        snprintf(what, sizeof what, "index%d ways_of_associativity>0", i);
        check(ways > 0, what);
        /* size == sets * line * ways * physical_line_partition is the geometry
         * identity for x86 leaf4 / arm CCSIDR / loongarch CPUCFG (size_show()
         * prints it as KiB). We assert "size == sets*line*ways*part/1024 K". */
        if (sets > 0 && line > 0 && ways > 0 && size_num > 0) {
            long computed_k = ((long)sets * line * ways * part) / 1024;
            snprintf(what, sizeof what, "index%d size(%ldK) == sets*line*ways*part/1024(%ldK)",
                     i, size_num, computed_k);
            check(size_num == computed_k, what);
        }

        /*
         * shared_cpu_map / shared_cpu_list, single-core (-smp 1):
         *
         * Only cpu0 is online, so EVERY leaf - L1 (private by rule) and L2+
         * (system-wide-shared by rule) alike - collapses to the single member
         * cpu0: shared_cpu_map == "1" (hex bit 0) and shared_cpu_list == "0".
         * The kernel builds these from the leaf's sharing scope following
         * cache_leaves_are_shared() (L1 private, L2+ shared by all online CPUs),
         * so at one online CPU both scopes render identically to bit 0. The
         * scope difference (an L2/L3 listing "0-3" while L1 stays "0") is only
         * observable with >1 online CPU; that multi-core case is covered by the
         * -smp 4 kernel system-suite test test-sysfs-cpu-topology. Here we assert
         * the smp1 collapse for every leaf.
         */
        snprintf(what, sizeof what, "index%d shared_cpu_list == \"0\" (only cpu0 online)", i);
        check(strcmp(scl, "0") == 0, what);
        snprintf(what, sizeof what, "index%d shared_cpu_map == \"1\" (only cpu0 online)", i);
        check(strcmp(scm, "1") == 0, what);

        /* At least one L1 leaf must be present and private-by-rule. */
        if (level == 1 && strcmp(scl, "0") == 0) {
            found_l1_private = 1;
        }

        if (i > 15) {
            break; /* guard */
        }
    }
    check(found_l1_private, "at least one L1 leaf is private to cpu0 (shared_cpu_list==0)");

    /* index0 must specifically be level 1 per the ABI ordering (L1 first). */
    char p[256], v[64];
    snprintf(p, sizeof p, "%s/cache/index0/level", CPU0);
    long l0 = -1;
    if (read_file(p, v, sizeof v) == 0) {
        l0 = parse_ulong(v);
    }
    check(l0 == 1, "cache/index0/level == 1");

    (void)a;
}

static void test_cache_absent_riscv(void) {
    char cache_dir[256];
    snprintf(cache_dir, sizeof cache_dir, "%s/cache", CPU0);
    printf("\n== cpu0/cache on riscv64 (expected ABSENT: no cache-geometry reg source) ==\n");
    int present = path_exists(cache_dir);
    printf("  cpu0/cache exists = %d\n", present);
    check(!present, "cpu0/cache/ absent on riscv64 (Linux-consistent, DT-only)");
}

/* ---- node0/meminfo (drivers/base/node.c:node_read_meminfo) ---- */

static void test_node_meminfo(void) {
    char path[256], buf[4096];
    printf("\n== node0/meminfo ==\n");

    snprintf(path, sizeof path, "%s/meminfo", NODE0);
    check(path_exists(path), "node0/meminfo present");
    if (read_file(path, buf, sizeof buf) != 0) {
        check(0, "node0/meminfo readable");
        return;
    }
    printf("--- node0/meminfo ---\n%s\n---------------------\n", buf);

    long total = -1, free = -1, used = -1;
    char *line = strtok(buf, "\n");
    while (line) {
        /* format: "Node 0 MemTotal:       <n> kB" */
        char *colon = strchr(line, ':');
        if (colon) {
            long v = parse_ulong(colon + 1);
            if (strstr(line, "MemTotal")) {
                total = v;
            } else if (strstr(line, "MemFree")) {
                free = v;
            } else if (strstr(line, "MemUsed")) {
                used = v;
            }
        }
        line = strtok(NULL, "\n");
    }
    printf("  MemTotal=%ld kB  MemFree=%ld kB  MemUsed=%ld kB\n", total, free, used);

    check(total > 0, "MemTotal > 0");
    check(free > 0, "MemFree > 0");
    /* The old fake emitted MemFree==MemTotal and MemUsed==0. Prove it's real. */
    check(free < total, "MemFree < MemTotal (not the old fake all-free gauge)");
    check(used == total - free, "MemUsed == MemTotal - MemFree");
    check(used > 0, "MemUsed > 0 (allocator has consumed some RAM)");
}

/* ---- cpu0/topology (drivers/base/topology.c) ---- */

static void test_topology(void) {
    char path[256], val[128];
    printf("\n== cpu0/topology ==\n");

    struct {
        const char *file;
    } files[] = {
        {"core_id"},
        {"physical_package_id"},
        {"core_cpus"},
        {"core_cpus_list"},
        {"package_cpus"},
        {"thread_siblings"},
        {"thread_siblings_list"},
    };

    for (size_t i = 0; i < sizeof files / sizeof files[0]; i++) {
        snprintf(path, sizeof path, "%s/topology/%s", CPU0, files[i].file);
        int ok = read_file(path, val, sizeof val) == 0;
        printf("  topology/%s = %s\n", files[i].file, ok ? val : "<missing>");
        char what[96];
        snprintf(what, sizeof what, "topology/%s present + non-empty", files[i].file);
        check(ok && val[0] != '\0', what);
    }

    /* core_id and physical_package_id must parse as integers. */
    snprintf(path, sizeof path, "%s/topology/core_id", CPU0);
    if (read_file(path, val, sizeof val) == 0) {
        check(parse_ulong(val) >= 0, "topology/core_id parses as >=0 int");
    } else {
        check(0, "topology/core_id readable");
    }
    snprintf(path, sizeof path, "%s/topology/physical_package_id", CPU0);
    if (read_file(path, val, sizeof val) == 0) {
        check(parse_ulong(val) >= 0, "topology/physical_package_id parses as >=0 int");
    } else {
        check(0, "topology/physical_package_id readable");
    }

    /* Single-core (-smp 1): core_cpus_list must be exactly "0". */
    snprintf(path, sizeof path, "%s/topology/core_cpus_list", CPU0);
    if (read_file(path, val, sizeof val) == 0) {
        check(strcmp(val, "0") == 0, "topology/core_cpus_list == \"0\" (single core)");
    } else {
        check(0, "topology/core_cpus_list readable");
    }
    snprintf(path, sizeof path, "%s/topology/thread_siblings_list", CPU0);
    if (read_file(path, val, sizeof val) == 0) {
        check(strcmp(val, "0") == 0, "topology/thread_siblings_list == \"0\" (no SMT)");
    } else {
        check(0, "topology/thread_siblings_list readable");
    }
}

int main(void) {
    printf("=== StarryOS sysfs CPU/cache/node carpet ===\n");
    enum arch a = detect_arch();

    /* cpu0 itself must exist on every arch. */
    check(dir_exists(CPU0), "/sys/devices/system/cpu/cpu0 present");

    if (a == ARCH_RISCV64) {
        test_cache_absent_riscv();
    } else if (a == ARCH_X86_64 || a == ARCH_AARCH64 || a == ARCH_LOONGARCH64) {
        test_cache_present(a);
    } else {
        printf("\n(unknown arch: skipping cache present/absent split)\n");
    }

    test_node_meminfo();
    test_topology();

    printf("\n=== SYSFS_CARPET OK=%d/%d ===\n", g_ok, g_total);
    if (g_ok == g_total && g_total > 0) {
        printf("SYSFS_CARPET TEST PASSED\n");
        return 0;
    }
    printf("SYSFS_CARPET TEST FAILED (%d not ok)\n", g_total - g_ok);
    return 1;
}
