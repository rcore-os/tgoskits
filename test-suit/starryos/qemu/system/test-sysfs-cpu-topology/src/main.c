#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <sys/utsname.h>
#include <limits.h>

/*
 * test-sysfs-cpu-topology: SMP (-smp 4) regression for the two ZR233 blockers in
 * the /sys per-CPU cache/topology tree (kernel/src/pseudofs/sysfs.rs), grounded
 * in Documentation/ABI/testing/sysfs-devices-system-cpu and
 * drivers/base/cacheinfo.c.
 *
 * Runs inside the -smp 4 kernel system-suite, so multiple CPUs are online and
 * the two bugs are runtime-observable (they are invisible at -smp 1 where only
 * cpu0 exists and owns everything):
 *
 *   BLOCKER 1 - per-CPU cache must be fixed at bring-up, not read from whatever
 *   PE runs the sysfs read. Discriminator here: every cpuN/cache/ directory must
 *   exist and carry a well-formed, self-consistent leaf set for that CPU. On the
 *   homogeneous QEMU -smp 4 models the leaves are identical across CPUs, so a
 *   "read the executing PE" bug would not corrupt values - but it would make
 *   cpuN/cache depend on scheduling; we assert every CPU's tree is present and
 *   consistent, which is the observable half at -smp 4 (full heterogeneous
 *   divergence needs big.LITTLE, not available under QEMU).
 *
 *   BLOCKER 2 - shared_cpu_map/shared_cpu_list of a shared cache must list ALL
 *   CPUs that share it, not just the owner. Discriminator: the old code reported
 *   owner-only for every leaf; the new code builds the mask from the arch-info
 *   rule (cache_leaves_are_shared(): L1 private, L2+ shared by all online CPUs).
 *   So a shared L2/L3 must list every online CPU (e.g. "0-3"), and the relation
 *   must be symmetric. The old owner-only code fails the "lists all sharers" and
 *   symmetry assertions; the new code passes.
 *
 *   BYTE ABI - a single-line sysfs attribute (node0/cpulist,
 *   cpu/{online,possible,present}) must terminate in exactly one '\n'. The
 *   pre-fix range helper embedded its own newline and the renderer appended a
 *   second, emitting `<range>\n\n`; the newline-stripping readers above hid it.
 *   check_single_line_attr() reads the raw bytes and rejects the doubled '\n'.
 */

#ifndef PATH_MAX
#define PATH_MAX 4096
#endif

#define MAX_CPUS 64
#define MAX_LEAVES 16

static int read_file(const char *path, char *buf, size_t cap) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    ssize_t n = read(fd, buf, cap - 1);
    close(fd);
    if (n < 0) {
        return -1;
    }
    buf[n] = '\0';
    /* strip trailing newline */
    while (n > 0 && (buf[n - 1] == '\n' || buf[n - 1] == '\r')) {
        buf[--n] = '\0';
    }
    return 0;
}

/*
 * Read a file preserving every byte, including the trailing newline(s) the
 * newline-stripping read_file() drops. Returns the byte count read (>= 0) or -1.
 * Used to assert the exact on-wire sysfs attribute bytes: a single-line attribute
 * must end in exactly one '\n', never a doubled '\n\n'.
 */
static ssize_t read_file_raw(const char *path, char *buf, size_t cap) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    ssize_t n = read(fd, buf, cap - 1);
    close(fd);
    if (n < 0) {
        return -1;
    }
    buf[n] = '\0';
    return n;
}

/*
 * Assert a single-line sysfs attribute ends with exactly one '\n' (the sysfs
 * byte ABI) and its body (sans that newline) equals `want`. Fails on the
 * pre-fix double newline `<body>\n\n`, which the newline-stripping reader hid.
 */
static void check_single_line_attr(const char *path, const char *want) {
    char raw[256] = "";
    char msg[320];
    ssize_t n = read_file_raw(path, raw, sizeof raw);
    snprintf(msg, sizeof msg, "%s is readable", path);
    CHECK(n >= 0, msg);
    if (n < 0) {
        return;
    }
    snprintf(msg, sizeof msg, "%s ends with exactly one newline (no trailing blank line)", path);
    CHECK(n >= 1 && raw[n - 1] == '\n' && (n < 2 || raw[n - 2] != '\n'), msg);
    /* Body without the single terminating newline must match exactly. */
    if (n >= 1 && raw[n - 1] == '\n') {
        raw[n - 1] = '\0';
    }
    snprintf(msg, sizeof msg, "%s body == \"%s\"", path, want);
    CHECK(strcmp(raw, want) == 0, msg);
}

static int dir_exists(const char *path) {
    struct stat st;
    return stat(path, &st) == 0 && S_ISDIR(st.st_mode);
}

/*
 * Parse a Linux cpumask *list* ("0", "0-3", "0,2-3", ...) into a bitset.
 * Returns the bitset; sets *count to the number of CPUs in it.
 */
static unsigned long long parse_cpulist(const char *s, int *count) {
    unsigned long long set = 0;
    const char *p = s;
    while (*p) {
        char *end = NULL;
        long a = strtol(p, &end, 10);
        if (end == p) {
            break;
        }
        long b = a;
        p = end;
        if (*p == '-') {
            p++;
            b = strtol(p, &end, 10);
            if (end == p) {
                break;
            }
            p = end;
        }
        for (long c = a; c <= b && c < MAX_CPUS; c++) {
            if (c >= 0) {
                set |= (1ULL << c);
            }
        }
        if (*p == ',') {
            p++;
        }
    }
    int n = 0;
    for (int i = 0; i < MAX_CPUS; i++) {
        if (set & (1ULL << i)) {
            n++;
        }
    }
    if (count) {
        *count = n;
    }
    return set;
}

enum arch { ARCH_X86_64, ARCH_AARCH64, ARCH_LOONGARCH64, ARCH_RISCV64, ARCH_OTHER };

static enum arch detect_arch(void) {
    struct utsname u;
    if (uname(&u) != 0) {
        return ARCH_OTHER;
    }
    printf("  uname machine: %s\n", u.machine);
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

/*
 * Parse a Linux cpumask *hex* ("1", "f", "1,00000000", ...) into a bitset.
 * Comma-separated 32-bit big-endian groups, most-significant group first, as
 * emitted by shared_cpu_map (%*pb). Only the low MAX_CPUS bits are kept.
 */
static unsigned long long parse_cpumask_hex(const char *s) {
    unsigned long long set = 0;
    const char *p = s;
    /* Count groups so we know each group's bit offset. */
    int groups = 1;
    for (const char *q = s; *q; q++) {
        if (*q == ',') {
            groups++;
        }
    }
    int gi = 0;
    while (*p) {
        char *end = NULL;
        unsigned long v = strtoul(p, &end, 16);
        if (end == p) {
            break;
        }
        int shift = (groups - 1 - gi) * 32;
        if (shift < 64) {
            set |= ((unsigned long long)v) << shift;
        }
        p = end;
        if (*p == ',') {
            p++;
        }
        gi++;
    }
    return set & ((MAX_CPUS >= 64) ? ~0ULL : ((1ULL << MAX_CPUS) - 1));
}

/* Per-leaf snapshot for symmetry cross-checks and stability re-reads. */
struct leaf {
    long level;
    char type[32];
    unsigned long long shared_set;
    int shared_count;
    int valid;
    /* Raw first-read strings, re-read later to prove reads are stable.
     * `type` above doubles as the first-read type string. */
    char raw_level[32];
    char raw_size[32];
    char raw_map[128];
    char raw_list[128];
    int has_size;
};

static struct leaf g_leaves[MAX_CPUS][MAX_LEAVES];
static int g_nleaves[MAX_CPUS];

/* How many CPUs are online, from /sys/devices/system/cpu/online. */
static int online_cpu_set(unsigned long long *set_out) {
    char buf[128] = "";
    int count = 0;
    if (read_file("/sys/devices/system/cpu/online", buf, sizeof buf) == 0) {
        unsigned long long set = parse_cpulist(buf, &count);
        printf("  cpu/online = %s (%d CPUs)\n", buf, count);
        if (set_out) {
            *set_out = set;
        }
    }
    return count;
}

int main(void) {
    TEST_START("sysfs-cpu-topology (SMP)");
    enum arch a = detect_arch();

    unsigned long long online_set = 0;
    int ncpu = online_cpu_set(&online_set);
    /* The kernel system-suite runs -smp 4, so we must see all 4 CPUs online. */
    CHECK(ncpu >= 2, "more than one CPU online (SMP run)");
    CHECK(ncpu == 4, "exactly 4 CPUs online (-smp 4 kernel suite)");

    /*
     * ---- sysfs single-line byte ABI: exactly one trailing newline ----
     * node0/cpulist and the cpu/{online,possible,present} lists once emitted
     * `<range>\n\n` (the range helper carried its own '\n' and the attribute
     * renderer appended another), an illegal blank line for a single-line
     * sysfs attribute. The existing checks read through newline-stripping
     * helpers and could not see it. Assert the raw bytes here.
     */
    char want_range[32];
    if (ncpu <= 1) {
        snprintf(want_range, sizeof want_range, "0");
    } else {
        snprintf(want_range, sizeof want_range, "0-%d", ncpu - 1);
    }
    check_single_line_attr("/sys/devices/system/node/node0/cpulist", want_range);
    check_single_line_attr("/sys/devices/system/cpu/online", want_range);
    check_single_line_attr("/sys/devices/system/cpu/possible", want_range);
    check_single_line_attr("/sys/devices/system/cpu/present", want_range);

    /* ---- BLOCKER 1: every online cpuN has its own present cache tree ---- */
    int arch_has_cache = (a == ARCH_X86_64 || a == ARCH_AARCH64 || a == ARCH_LOONGARCH64);
    int cpus_with_cache = 0;

    for (int cpu = 0; cpu < ncpu; cpu++) {
        char cdir[128];
        snprintf(cdir, sizeof cdir, "/sys/devices/system/cpu/cpu%d", cpu);
        char what[128];
        snprintf(what, sizeof what, "cpu%d/ directory present", cpu);
        CHECK(dir_exists(cdir), what);

        char cache[160];
        snprintf(cache, sizeof cache, "%s/cache", cdir);
        int has_cache = dir_exists(cache);

        if (arch_has_cache) {
            /*
             * riscv has no cache-geometry register source, so cache/ is
             * legitimately absent (Linux-consistent) - but on x86 under
             * QEMU/TCG leaf 4 may be unpopulated, so cache/ can be absent
             * there too. We therefore require cache/ per-cpu only when cpu0
             * has it, and then demand ALL cpus have it (the fixed-at-bring-up
             * invariant: no CPU may be missing its own tree).
             */
        }

        g_nleaves[cpu] = 0;
        if (!has_cache) {
            continue;
        }
        cpus_with_cache++;

        for (int i = 0; i < MAX_LEAVES; i++) {
            char base[224];
            snprintf(base, sizeof base, "%s/index%d", cache, i);
            if (!dir_exists(base)) {
                break;
            }
            char path[288], val[128];
            struct leaf *lf = &g_leaves[cpu][i];
            lf->valid = 1;

            snprintf(path, sizeof path, "%s/level", base);
            lf->raw_level[0] = '\0';
            lf->level = (read_file(path, lf->raw_level, sizeof lf->raw_level) == 0)
                            ? strtol(lf->raw_level, NULL, 10)
                            : -1;

            snprintf(path, sizeof path, "%s/type", base);
            lf->type[0] = '\0';
            read_file(path, lf->type, sizeof lf->type);

            snprintf(path, sizeof path, "%s/size", base);
            lf->raw_size[0] = '\0';
            lf->has_size = (read_file(path, lf->raw_size, sizeof lf->raw_size) == 0);

            snprintf(path, sizeof path, "%s/shared_cpu_map", base);
            lf->raw_map[0] = '\0';
            read_file(path, lf->raw_map, sizeof lf->raw_map);

            snprintf(path, sizeof path, "%s/shared_cpu_list", base);
            lf->raw_list[0] = '\0';
            lf->shared_set = 0;
            lf->shared_count = 0;
            if (read_file(path, lf->raw_list, sizeof lf->raw_list) == 0) {
                snprintf(val, sizeof val, "%s", lf->raw_list);
                lf->shared_set = parse_cpulist(val, &lf->shared_count);
            }
            g_nleaves[cpu] = i + 1;

            /* shared_cpu_map (hex) and shared_cpu_list must name the same set. */
            snprintf(path, sizeof path, "cpu%d/index%d shared_cpu_map hex == shared_cpu_list set",
                     cpu, i);
            CHECK(parse_cpumask_hex(lf->raw_map) == lf->shared_set, path);

            printf("  cpu%d/index%d: level=%ld type=%s shared_cpu_list=%s (n=%d)\n",
                   cpu, i, lf->level, lf->type, val, lf->shared_count);
        }

        snprintf(what, sizeof what, "cpu%d has >=1 cache leaf", cpu);
        CHECK(g_nleaves[cpu] >= 1, what);

        /* index0 must be an L1 leaf (ABI ordering). */
        snprintf(what, sizeof what, "cpu%d/cache/index0/level == 1", cpu);
        CHECK(g_nleaves[cpu] >= 1 && g_leaves[cpu][0].level == 1, what);
    }

    /*
     * Fixed-at-bring-up invariant: if any CPU exposes cache/, ALL online CPUs
     * must (a missing cpuN tree would mean the table was not populated for that
     * CPU). Homogeneous SMP => identical leaf counts across CPUs.
     */
    /*
     * aarch64 (CCSIDR/CLIDR) and loongarch64 (CPUCFG) expose cache geometry
     * through architectural registers that are always readable, so a wholly
     * absent cache/ tree there is a real regression, not a model gap, and must
     * fail rather than downgrade to a NOTE. x86_64's CPUID.4 deterministic-cache
     * leaf is optional (TCG may leave it unpopulated) and riscv64 has no
     * geometry source, so on those a fully absent tree stays a soft NOTE.
     */
    int cache_mandatory = (a == ARCH_AARCH64 || a == ARCH_LOONGARCH64);
    if (cpus_with_cache > 0) {
        CHECK(cpus_with_cache == ncpu, "every online CPU exposes its own cache/ tree");
        int n0 = g_nleaves[0];
        int consistent = 1;
        for (int cpu = 1; cpu < ncpu; cpu++) {
            if (g_nleaves[cpu] != n0) {
                consistent = 0;
            }
        }
        CHECK(consistent, "all CPUs report the same number of cache leaves (homogeneous SMP)");
    } else if (cache_mandatory) {
        CHECK(0, "cache/ tree present on every CPU (architectural cache regs are mandatory here)");
    } else if (arch_has_cache) {
        printf("  NOTE: cache/ absent on all CPUs (QEMU model did not populate cache regs); "
               "topology-only checks below still apply\n");
    }

    /*
     * ---- BLOCKER 1 (cross-read stability): cpuN/cache is fixed to cpuN ----
     *
     * The registers describing a cache leaf (CPUID.4 / CCSIDR / CPUCFG) read
     * only the executing PE, so /cpuN/cache must serve cpuN's pinned bring-up
     * sample regardless of which PE runs the read. The pre-fix code fell back to
     * a live read of the executing PE whenever cpuN's slot was unpopulated,
     * making the answer depend on scheduling. We re-read every leaf's key
     * attributes and require them byte-identical to the first read: if a read
     * ever returned "whatever PE happened to service it", two reads that land on
     * different PEs would disagree. Combined with the "every CPU exposes its own
     * tree" invariant above, this locks cpuN/cache to cpuN.
     */
    if (cpus_with_cache > 0) {
        int stable_all = 1;
        for (int cpu = 0; cpu < ncpu; cpu++) {
            for (int i = 0; i < g_nleaves[cpu]; i++) {
                struct leaf *lf = &g_leaves[cpu][i];
                if (!lf->valid) {
                    continue;
                }
                char base[224], path[288], v[128];
                snprintf(base, sizeof base,
                         "/sys/devices/system/cpu/cpu%d/cache/index%d", cpu, i);

                snprintf(path, sizeof path, "%s/level", base);
                if (read_file(path, v, sizeof v) != 0 || strcmp(v, lf->raw_level) != 0) {
                    stable_all = 0;
                }
                snprintf(path, sizeof path, "%s/type", base);
                if (read_file(path, v, sizeof v) != 0 || strcmp(v, lf->type) != 0) {
                    stable_all = 0;
                }
                if (lf->has_size) {
                    snprintf(path, sizeof path, "%s/size", base);
                    if (read_file(path, v, sizeof v) != 0 || strcmp(v, lf->raw_size) != 0) {
                        stable_all = 0;
                    }
                }
                snprintf(path, sizeof path, "%s/shared_cpu_map", base);
                if (read_file(path, v, sizeof v) != 0 || strcmp(v, lf->raw_map) != 0) {
                    stable_all = 0;
                }
                snprintf(path, sizeof path, "%s/shared_cpu_list", base);
                if (read_file(path, v, sizeof v) != 0 || strcmp(v, lf->raw_list) != 0) {
                    stable_all = 0;
                    printf("  UNSTABLE: cpu%d/index%d shared_cpu_list changed between reads "
                           "(first=%s second=%s)\n", cpu, i, lf->raw_list, v);
                }
            }
        }
        CHECK(stable_all, "every cpuN/cache attribute is identical across repeated reads "
                          "(fixed to cpuN, not the reading PE)");
    }

    /* ---- BLOCKER 2: shared_cpu_map lists ALL sharers, symmetric, L1 private ---- */
    int any_shared_multi = 0;    /* did we see a leaf shared by >1 CPU? */
    int owner_included_all = 1;
    int l1_private_all = 1;
    int symmetric_all = 1;
    int at_least_one_l1 = 0;

    for (int cpu = 0; cpu < ncpu; cpu++) {
        for (int i = 0; i < g_nleaves[cpu]; i++) {
            struct leaf *lf = &g_leaves[cpu][i];
            if (!lf->valid) {
                continue;
            }
            /* The owner CPU must always be in its own leaf's shared set. */
            if (!(lf->shared_set & (1ULL << cpu))) {
                owner_included_all = 0;
            }
            if (lf->shared_count > 1) {
                any_shared_multi = 1;
            }
            if (lf->level == 1) {
                at_least_one_l1 = 1;
                /* L1 is private: only its owner. */
                if (lf->shared_set != (1ULL << cpu)) {
                    l1_private_all = 0;
                }
            }
            /*
             * Symmetry: for every CPU j listed as sharing this leaf, cpu j must
             * have a leaf of the same level+type whose shared set includes cpu.
             * This is exactly what cache_shared_cpu_map_setup() guarantees
             * (cpumask_set_cpu on both this_leaf and sib_leaf). The old
             * owner-only code trivially fails this for any shared leaf.
             */
            for (int j = 0; j < ncpu; j++) {
                if (j == cpu || !(lf->shared_set & (1ULL << j))) {
                    continue;
                }
                int found = 0;
                for (int k = 0; k < g_nleaves[j]; k++) {
                    struct leaf *sl = &g_leaves[j][k];
                    if (sl->valid && sl->level == lf->level &&
                        strcmp(sl->type, lf->type) == 0 &&
                        (sl->shared_set & (1ULL << cpu))) {
                        found = 1;
                        break;
                    }
                }
                if (!found) {
                    symmetric_all = 0;
                    printf("  ASYMMETRY: cpu%d/index%d (L%ld %s) lists cpu%d, but cpu%d has no "
                           "matching leaf listing cpu%d\n",
                           cpu, i, lf->level, lf->type, j, j, cpu);
                }
            }
        }
    }

    if (cpus_with_cache > 0) {
        CHECK(owner_included_all, "every cache leaf's shared_cpu_list contains its owning CPU");
        CHECK(at_least_one_l1, "at least one L1 leaf enumerated");
        CHECK(l1_private_all, "every L1 leaf is private (shared_cpu_list == owner only)");
        CHECK(symmetric_all, "shared_cpu_map relation is symmetric across CPUs");

        /*
         * A cache shared by ALL online CPUs must exist under every model the
         * kernel suite uses, so its shared_cpu_list lists every online CPU
         * (e.g. "0-3"). This is the direct old-red/new-green discriminator for
         * BLOCKER 2: the pre-fix owner-only code could never produce a leaf
         * whose shared_cpu_list has >1 CPU.
         *
         *   - aarch64 (cortex-a53) / loongarch64 (la464): CCSIDR/CLIDR and
         *     CPUCFG carry no thread-sharing count, so the kernel applies the
         *     arch-info rule "L2+ shared by all online CPUs" -> every L2/L3 leaf
         *     lists all CPUs.
         *   - x86_64 (Haswell,+avx -smp 4): QEMU's default topology is 1 socket
         *     x 4 cores, and Haswell's L3 has die-level share scope, so CPUID
         *     leaf 4 reports L3 num_threads_sharing=3 (4 sharers) while L1/L2 are
         *     per-core. The kernel honours that count, so L3 is shared by all 4
         *     CPUs and L1/L2 stay private. (Verified against QEMU 10.2.1 source
         *     + empirical leaf-4 probe.)
         */
        int found_systemwide = 0;
        int l3_systemwide = 0;
        for (int cpu = 0; cpu < ncpu; cpu++) {
            for (int i = 0; i < g_nleaves[cpu]; i++) {
                struct leaf *lf = &g_leaves[cpu][i];
                if (!lf->valid) {
                    continue;
                }
                if (lf->shared_count == ncpu) {
                    found_systemwide = 1;
                    if (lf->level >= 3) {
                        l3_systemwide = 1;
                    }
                }
            }
        }

        CHECK(found_systemwide,
              "a shared cache lists ALL online CPUs (L2+/L3 arch-info rule; BLOCKER 2)");
        CHECK(any_shared_multi, "at least one cache leaf is genuinely cross-core-shared (>1 CPU)");

        if (a == ARCH_X86_64) {
            /* On x86 specifically it is the L3 that spans the socket. */
            CHECK(l3_systemwide,
                  "x86 L3 (die-scope) is shared by all online CPUs, L1/L2 per-core");
        }
    }

    /*
     * ---- topology: core_siblings == package_cpus (the package/socket domain) ----
     *
     * drivers/base/topology.c installs core_siblings{,_list} unconditionally in
     * bin_attrs[], backed by core_cpumask - the SAME mask package_cpus renders.
     * At -smp 4 the single-socket QEMU model puts every CPU in one package, so
     * each CPU's core_siblings must list all online CPUs and equal package_cpus.
     */
    int core_siblings_ok = 1;
    int core_siblings_systemwide = 1;
    for (int cpu = 0; cpu < ncpu; cpu++) {
        char base[160], csib[128] = "", pkg[128] = "", csibl[128] = "", pkgl[128] = "";
        snprintf(base, sizeof base, "/sys/devices/system/cpu/cpu%d/topology", cpu);

        char path[224];
        snprintf(path, sizeof path, "%s/core_siblings", base);
        int r1 = read_file(path, csib, sizeof csib);
        snprintf(path, sizeof path, "%s/package_cpus", base);
        int r2 = read_file(path, pkg, sizeof pkg);
        snprintf(path, sizeof path, "%s/core_siblings_list", base);
        int r3 = read_file(path, csibl, sizeof csibl);
        snprintf(path, sizeof path, "%s/package_cpus_list", base);
        int r4 = read_file(path, pkgl, sizeof pkgl);

        printf("  cpu%d: core_siblings=%s package_cpus=%s core_siblings_list=%s\n", cpu, csib, pkg,
               csibl);

        if (r1 != 0 || r2 != 0 || r3 != 0 || r4 != 0 || strcmp(csib, pkg) != 0 ||
            strcmp(csibl, pkgl) != 0) {
            core_siblings_ok = 0;
        }
        /* The whole machine is one socket, so the list must cover all CPUs. */
        int scount = 0;
        parse_cpulist(csibl, &scount);
        if (scount != ncpu) {
            core_siblings_systemwide = 0;
        }
    }
    CHECK(core_siblings_ok, "every cpu's core_siblings == package_cpus (same core_cpumask)");
    CHECK(core_siblings_systemwide,
          "core_siblings lists all online CPUs (single-socket package domain)");

    /*
     * ---- topology: the full per-CPU attribute contract ----
     *
     * Beyond core_siblings/package_cpus (checked above), drivers/base/topology.c
     * exposes, for every online CPU: the SMT/thread domain (thread_siblings ==
     * core_cpus), the per-CPU identifiers (core_id, physical_package_id) and -
     * behind Kconfig gates - the cluster domain (TOPOLOGY_CLUSTER_SYSFS: x86 /
     * arm64 / riscv, NOT loongarch) and the die domain (TOPOLOGY_DIE_SYSFS: x86
     * only). Every value is asserted against the exact single-socket / no-SMT /
     * no-sub-cluster geometry this kernel models, and the per-arch presence of
     * the cluster and die families is asserted BOTH ways: present-and-correct
     * where the arch gates them in, ENOENT where it gates them out (child_names /
     * lookup_child in kernel/src/pseudofs/sysfs.rs, which mirror the Linux gates).
     */
    int core_id_ok = 1;             /* core_id == this CPU's number */
    int pkg_id_zero_ok = 1;         /* physical_package_id == 0 (single socket) */
    int thread_self_ok = 1;         /* thread_siblings{,_list} == this CPU only */
    int core_cpus_eq_thread_ok = 1; /* core_cpus{,_list} == thread_siblings */
    int cluster_ok = 1;             /* cluster_* correct-where-gated-in / absent-where-out */
    int die_ok = 1;                 /* die_* correct-where-gated-in / absent-where-out */

    const int have_cluster = (a == ARCH_X86_64 || a == ARCH_AARCH64 || a == ARCH_RISCV64);
    const int have_die = (a == ARCH_X86_64);

    for (int cpu = 0; cpu < ncpu; cpu++) {
        char base[160], path[224], v[128];
        snprintf(base, sizeof base, "/sys/devices/system/cpu/cpu%d/topology", cpu);
        unsigned long long self_bit = (1ULL << cpu);

        /* core_id == this CPU's number: no-SMT one-thread-per-core, so the
         * per-core id coincides with the logical CPU number (sysfs.rs core_id
         * arm; Linux topology_core_id). */
        char want[32];
        snprintf(want, sizeof want, "%d", cpu);
        snprintf(path, sizeof path, "%s/core_id", base);
        if (read_file(path, v, sizeof v) != 0 || strcmp(v, want) != 0) {
            core_id_ok = 0;
            printf("  cpu%d core_id=%s want %s\n", cpu, v, want);
        }

        /* physical_package_id == 0: QEMU -smp 4 is a single socket. */
        snprintf(path, sizeof path, "%s/physical_package_id", base);
        if (read_file(path, v, sizeof v) != 0 || strcmp(v, "0") != 0) {
            pkg_id_zero_ok = 0;
            printf("  cpu%d physical_package_id=%s want 0\n", cpu, v);
        }

        /* thread_siblings{,_list}: no SMT -> exactly this CPU, in both the hex
         * mask and the list rendering. */
        unsigned long long ts_map = 0, ts_list = 0;
        int tsl_n = 0;
        snprintf(path, sizeof path, "%s/thread_siblings", base);
        if (read_file(path, v, sizeof v) == 0) {
            ts_map = parse_cpumask_hex(v);
        } else {
            thread_self_ok = 0;
        }
        snprintf(path, sizeof path, "%s/thread_siblings_list", base);
        if (read_file(path, v, sizeof v) == 0) {
            ts_list = parse_cpulist(v, &tsl_n);
        } else {
            thread_self_ok = 0;
        }
        if (ts_map != self_bit || ts_list != self_bit || tsl_n != 1) {
            thread_self_ok = 0;
            printf("  cpu%d thread_siblings map=%llx list_n=%d (want self only)\n", cpu, ts_map,
                   tsl_n);
        }

        /* core_cpus{,_list} == thread_siblings: Linux core_cpus is the SMT
         * sibling mask (topology_sibling_cpumask), the very mask thread_siblings
         * renders (sysfs.rs groups them in one arm). */
        unsigned long long cc_map = 0, cc_list = 0;
        int ccl_n = 0;
        snprintf(path, sizeof path, "%s/core_cpus", base);
        if (read_file(path, v, sizeof v) == 0) {
            cc_map = parse_cpumask_hex(v);
        } else {
            core_cpus_eq_thread_ok = 0;
        }
        snprintf(path, sizeof path, "%s/core_cpus_list", base);
        if (read_file(path, v, sizeof v) == 0) {
            cc_list = parse_cpulist(v, &ccl_n);
        } else {
            core_cpus_eq_thread_ok = 0;
        }
        if (cc_map != ts_map || cc_list != ts_list) {
            core_cpus_eq_thread_ok = 0;
            printf("  cpu%d core_cpus map=%llx != thread_siblings %llx\n", cpu, cc_map, ts_map);
        }

        /* cluster_*: present+correct on x86/arm64/riscv (cluster_id 0, cluster
         * mask == self, since no sub-package cluster is modelled - Linux's
         * default clear_cpu_topology() leaves cluster_sibling == self); ENOENT
         * on loongarch, which defines no topology_cluster_id/cpumask. */
        snprintf(path, sizeof path, "%s/cluster_id", base);
        int cid_present = (read_file(path, v, sizeof v) == 0);
        if (have_cluster) {
            if (!cid_present || strcmp(v, "0") != 0) {
                cluster_ok = 0;
                printf("  cpu%d cluster_id=%s (want 0)\n", cpu, cid_present ? v : "<absent>");
            }
            unsigned long long cl_map = 0, cl_list = 0;
            int cll_n = 0;
            snprintf(path, sizeof path, "%s/cluster_cpus", base);
            if (read_file(path, v, sizeof v) == 0) {
                cl_map = parse_cpumask_hex(v);
            } else {
                cluster_ok = 0;
            }
            snprintf(path, sizeof path, "%s/cluster_cpus_list", base);
            if (read_file(path, v, sizeof v) == 0) {
                cl_list = parse_cpulist(v, &cll_n);
            } else {
                cluster_ok = 0;
            }
            if (cl_map != self_bit || cl_list != self_bit) {
                cluster_ok = 0;
                printf("  cpu%d cluster mask=%llx list=%llx (want self)\n", cpu, cl_map, cl_list);
            }
        } else {
            /* loongarch: the whole cluster_* family must be absent (ENOENT). */
            if (cid_present) {
                cluster_ok = 0;
                printf("  cpu%d cluster_id present on arch that gates it out\n", cpu);
            }
            snprintf(path, sizeof path, "%s/cluster_cpus", base);
            if (read_file(path, v, sizeof v) == 0) {
                cluster_ok = 0;
            }
            snprintf(path, sizeof path, "%s/cluster_cpus_list", base);
            if (read_file(path, v, sizeof v) == 0) {
                cluster_ok = 0;
            }
        }

        /* die_*: present+correct on x86 only (die_id 0, die_cpus spans the whole
         * single die == every online CPU); ENOENT on arm64/riscv/loongarch,
         * none of which define topology_die_id/cpumask. */
        snprintf(path, sizeof path, "%s/die_id", base);
        int did_present = (read_file(path, v, sizeof v) == 0);
        if (have_die) {
            if (!did_present || strcmp(v, "0") != 0) {
                die_ok = 0;
                printf("  cpu%d die_id=%s (want 0)\n", cpu, did_present ? v : "<absent>");
            }
            unsigned long long d_map = 0, d_list = 0;
            int dl_n = 0;
            snprintf(path, sizeof path, "%s/die_cpus", base);
            if (read_file(path, v, sizeof v) == 0) {
                d_map = parse_cpumask_hex(v);
            } else {
                die_ok = 0;
            }
            snprintf(path, sizeof path, "%s/die_cpus_list", base);
            if (read_file(path, v, sizeof v) == 0) {
                d_list = parse_cpulist(v, &dl_n);
            } else {
                die_ok = 0;
            }
            if (d_map != online_set || d_list != online_set || dl_n != ncpu) {
                die_ok = 0;
                printf("  cpu%d die_cpus map=%llx list_n=%d (want all-online %llx/%d)\n", cpu, d_map,
                       dl_n, online_set, ncpu);
            }
        } else {
            if (did_present) {
                die_ok = 0;
                printf("  cpu%d die_id present on arch that gates it out\n", cpu);
            }
            snprintf(path, sizeof path, "%s/die_cpus", base);
            if (read_file(path, v, sizeof v) == 0) {
                die_ok = 0;
            }
            snprintf(path, sizeof path, "%s/die_cpus_list", base);
            if (read_file(path, v, sizeof v) == 0) {
                die_ok = 0;
            }
        }
    }

    CHECK(core_id_ok, "every cpu's core_id == its logical CPU number (no-SMT one-thread-per-core)");
    CHECK(pkg_id_zero_ok, "every cpu's physical_package_id == 0 (single socket)");
    CHECK(thread_self_ok, "thread_siblings{,_list} == the CPU itself (no SMT)");
    CHECK(core_cpus_eq_thread_ok, "core_cpus{,_list} == thread_siblings (SMT sibling mask)");
    if (have_cluster) {
        CHECK(cluster_ok,
              "cluster_id==0 and cluster_cpus{,_list}==self (TOPOLOGY_CLUSTER_SYSFS arch)");
    } else {
        CHECK(cluster_ok,
              "cluster_* absent/ENOENT on arch without TOPOLOGY_CLUSTER_SYSFS (loongarch)");
    }
    if (have_die) {
        CHECK(die_ok, "die_id==0 and die_cpus{,_list} span all online CPUs (x86 single die)");
    } else {
        CHECK(die_ok, "die_* absent/ENOENT on arch without TOPOLOGY_DIE_SYSFS (non-x86)");
    }

    TEST_DONE();
}
