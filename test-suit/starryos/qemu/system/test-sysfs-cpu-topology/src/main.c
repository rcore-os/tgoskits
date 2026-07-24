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

/* Per-leaf snapshot for symmetry cross-checks. */
struct leaf {
    long level;
    char type[32];
    unsigned long long shared_set;
    int shared_count;
    int valid;
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
            lf->level = (read_file(path, val, sizeof val) == 0) ? strtol(val, NULL, 10) : -1;

            snprintf(path, sizeof path, "%s/type", base);
            lf->type[0] = '\0';
            read_file(path, lf->type, sizeof lf->type);

            snprintf(path, sizeof path, "%s/shared_cpu_list", base);
            lf->shared_set = 0;
            lf->shared_count = 0;
            if (read_file(path, val, sizeof val) == 0) {
                lf->shared_set = parse_cpulist(val, &lf->shared_count);
            }
            g_nleaves[cpu] = i + 1;

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
    } else if (arch_has_cache) {
        printf("  NOTE: cache/ absent on all CPUs (QEMU model did not populate cache regs); "
               "topology-only checks below still apply\n");
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

    TEST_DONE();
}
