#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#define KPU_DEVICE_PATH "/dev/kpu"
#define KPU_DEVICE_ALIAS_PATH "/dev/kpu0"

#define KPU_IOC_GET_STATUS 0x4b00u
#define KPU_IOC_CLEAR 0x4b01u
#define KPU_IOC_PROGRAM_COMMAND 0x4b02u
#define KPU_IOC_START 0x4b03u
#define KPU_IOC_RUN 0x4b04u
#define KPU_IOC_WAIT_DONE 0x4b05u
#define KPU_IOC_GET_INFO 0x4b06u
#define KPU_IOC_GET_IRQ_COUNT 0x4b07u

#define KPU_MMAP_CFG_OFFSET 0x0ull
#define KPU_MMAP_L2_OFFSET 0x1000ull
#define KPU_MMAP_FAKE_OUTPUT_OFFSET 0x2000ull
#define KPU_MMAP_RUNTIME_RDATA_OFFSET 0x3000ull
#define KPU_MMAP_RUNTIME_COMMAND_OFFSET 0x4000ull
#define KPU_MMAP_RUNTIME_DIRECT_IO_OFFSET 0x5000ull
#define KPU_MMAP_RUNTIME_DDR_OFFSET 0x6000ull

#define KPU_CFG_PADDR 0x80400000ull
#define KPU_L2_PADDR 0x80000000ull
#define KPU_FAKE_OUTPUT_PADDR 0x10090000ull
#define KPU_RUNTIME_RDATA_PADDR 0x10000000ull
#define KPU_RUNTIME_COMMAND_PADDR 0x10190000ull
#define KPU_RUNTIME_DIRECT_IO_PADDR 0x10500000ull
#define KPU_RUNTIME_DDR_PADDR 0x3c000000ull

#define KPU_RUNTIME_RDATA_BASE 0x10000020ull
#define KPU_RUNTIME_FUNCTION_COMMAND_PADDR 0x1032b020ull
#define KPU_RUNTIME_ARG_TABLE_PADDR 0x80000000ull
#define KPU_RUNTIME_DIRECT_SOURCE_PADDR 0x10500020ull
#define KPU_RUNTIME_DIRECT_OUTPUT_PADDR 0x10501020ull

#define KPU_CFG_SIZE 0x800u
#define KPU_L2_SIZE 0x200000u
#define KPU_FAKE_OUTPUT_SIZE 0x100000u
#define KPU_RUNTIME_RDATA_SIZE 0x90000u
#define KPU_RUNTIME_COMMAND_SIZE 0x370000u
#define KPU_RUNTIME_DIRECT_IO_SIZE 0xb00000u
#define KPU_RUNTIME_DDR_SIZE 0x4000000u

#define KPU_COMMAND_START 0x100u
#define KPU_COMMAND_END 0x104u
#define KPU_COMMAND_HI 0x108u
#define KPU_STATUS_LO 0x130u
#define KPU_STATUS_HI 0x134u
#define KPU_DONE_STATUS 0x0000000400000004ull

#define KPU_IRQ_NONE 0xffffffffu
#define KPU_INFO_F_FDT 0x1u
#define KPU_INFO_F_IRQ_WAIT 0x2u
#define KPU_INFO_F_FAKE_OUTPUT 0x4u
#define KPU_INFO_F_RUNTIME_SCRATCH 0x8u

struct k230_kpu_command_range {
    uint64_t start_paddr;
    uint64_t end_paddr;
};

struct k230_kpu_info {
    uint64_t cfg_paddr;
    uint64_t cfg_size;
    uint64_t l2_paddr;
    uint64_t l2_size;
    uint32_t irq;
    uint32_t flags;
};

_Static_assert(sizeof(struct k230_kpu_command_range) == 16, "unexpected KPU command range size");
_Static_assert(offsetof(struct k230_kpu_command_range, start_paddr) == 0,
               "unexpected KPU command range start offset");
_Static_assert(offsetof(struct k230_kpu_command_range, end_paddr) == 8,
               "unexpected KPU command range end offset");
_Static_assert(sizeof(struct k230_kpu_info) == 40, "unexpected KPU info size");
_Static_assert(offsetof(struct k230_kpu_info, cfg_paddr) == 0,
               "unexpected KPU info cfg_paddr offset");
_Static_assert(offsetof(struct k230_kpu_info, cfg_size) == 8,
               "unexpected KPU info cfg_size offset");
_Static_assert(offsetof(struct k230_kpu_info, l2_paddr) == 16,
               "unexpected KPU info l2_paddr offset");
_Static_assert(offsetof(struct k230_kpu_info, l2_size) == 24,
               "unexpected KPU info l2_size offset");
_Static_assert(offsetof(struct k230_kpu_info, irq) == 32, "unexpected KPU info irq offset");
_Static_assert(offsetof(struct k230_kpu_info, flags) == 36, "unexpected KPU info flags offset");

#define GNNE_FIELD(value, shift) ((uint32_t)(value) << (shift))
#define GNNE_LUI(rd, imm) (0x02u | GNNE_FIELD(rd, 7) | GNNE_FIELD(imm, 12))
#define GNNE_LW(rd, rs, offset) \
    (0x06u | GNNE_FIELD(rd, 7) | GNNE_FIELD(rs, 12) | GNNE_FIELD((offset) & 0xfff, 20))
#define GNNE_ADDI(rd, rs, imm) \
    (0x0eu | GNNE_FIELD(rd, 7) | GNNE_FIELD(rs, 12) | GNNE_FIELD((imm) & 0xfff, 20))
#define GNNE_MMU_CONF(rstart, rdepth, id) \
    (0x44u | GNNE_FIELD(rstart, 7) | GNNE_FIELD(rdepth, 12) | GNNE_FIELD(id, 17))
#define GNNE_SS_PACK_SHAPE(rn, rc, rh, rw, rss)                      \
    (0x40u | GNNE_FIELD(rn, 7) | GNNE_FIELD(rc, 12) |                \
     GNNE_FIELD(rh, 17) | GNNE_FIELD(rw, 22) | GNNE_FIELD(rss, 27))
#define GNNE_SS_PACK_STRIDE(rn, rc, rh, rss) \
    (0x42u | GNNE_FIELD(rn, 7) | GNNE_FIELD(rc, 12) | GNNE_FIELD(rh, 17) | GNNE_FIELD(rss, 27))
#define GNNE_L2_LOAD_CONF(rstride_d, rstride_s, l2_dt, ddr_dt)        \
    (0x46u | GNNE_FIELD(rstride_d, 7) | GNNE_FIELD(rstride_s, 10) |   \
     GNNE_FIELD(l2_dt, 13) | GNNE_FIELD(ddr_dt, 15))
#define GNNE_L2_STORE_CONF(rstride_d, rstride_s, l2_dt, ddr_dt)       \
    (0x4au | GNNE_FIELD(rstride_d, 7) | GNNE_FIELD(rstride_s, 10) |   \
     GNNE_FIELD(l2_dt, 13) | GNNE_FIELD(ddr_dt, 15))
#define GNNE_L2_LOAD(raddr_d, raddr_s, rshape) \
    (0x4cu | GNNE_FIELD(raddr_d, 7) | GNNE_FIELD(raddr_s, 12) | GNNE_FIELD(rshape, 17))
#define GNNE_L2_STORE(raddr_d, raddr_s, rshape) \
    (0x4eu | GNNE_FIELD(raddr_d, 7) | GNNE_FIELD(raddr_s, 12) | GNNE_FIELD(rshape, 17))

#define KPU_RUNTIME_IMAGE_FILE_PATH "/usr/share/k230-kpu-smoke/runtime-direct-io.krun"
#define KPU_RUNTIME_BLOB_IMAGE_FILE_PATH "/usr/share/k230-kpu-smoke/runtime-direct-io-file.krun"
#define KPU_YOLOV8N_CAPTURE_FILE_PATH "/usr/share/k230-kpu-smoke/captures/yolov8n-last-command.krun"
#define KPU_YOLOV8N_DELTA_CAPTURE_FILE_PATH \
    "/usr/share/k230-kpu-smoke/captures/yolov8n-full-sequence-delta.krun"
#define KPU_YOLOV8N_FULL_CAPTURE_FILE_PATH "/usr/share/k230-kpu-smoke/captures/yolov8n-full-sequence.krun"
#define KPU_REAL_KMODEL_FILE_PATH "/usr/share/k230-kpu-smoke/models/yolov8n_320.kmodel"
#define KPU_RUNTIME_IMAGE_MAX_NAME 64
#define KPU_RUNTIME_IMAGE_MAX_COMMANDS 65536
#define KPU_RUNTIME_IMAGE_MAX_RUNS 256
#define KPU_RUNTIME_IMAGE_MAX_SECTIONS 65536
#define KPU_RUNTIME_IMAGE_MAX_CHECKS 1024
#define KPU_RUNTIME_IMAGE_MAX_PAYLOAD (16 * 1024 * 1024)
#define KPU_RUNTIME_IMAGE_MAX_LINE 8192
#define KPU_RUNTIME_IMAGE_MAX_TOKENS 1024

enum kpu_runtime_window_id {
    KPU_RUNTIME_WINDOW_L2,
    KPU_RUNTIME_WINDOW_RDATA,
    KPU_RUNTIME_WINDOW_FAKE_OUTPUT,
    KPU_RUNTIME_WINDOW_COMMAND,
    KPU_RUNTIME_WINDOW_DIRECT_IO,
    KPU_RUNTIME_WINDOW_DDR,
    KPU_RUNTIME_WINDOW_COUNT,
};

enum kpu_runtime_section_kind {
    KPU_RUNTIME_SECTION_COPY,
    KPU_RUNTIME_SECTION_COPY_FILE,
    KPU_RUNTIME_SECTION_FILL,
};

enum kpu_runtime_check_kind {
    KPU_RUNTIME_CHECK_BYTES,
    KPU_RUNTIME_CHECK_FNV1A64,
};

struct kpu_runtime_window {
    enum kpu_runtime_window_id id;
    const char *name;
    uint64_t paddr;
    size_t size;
    off_t mmap_offset;
};

struct kpu_runtime_section {
    enum kpu_runtime_window_id window;
    size_t offset;
    enum kpu_runtime_section_kind kind;
    const uint8_t *data;
    const char *file_path;
    size_t file_offset;
    uint8_t fill;
    size_t len;
};

struct kpu_runtime_check {
    enum kpu_runtime_check_kind kind;
    enum kpu_runtime_window_id window;
    size_t offset;
    const uint8_t *expected;
    size_t expected_len;
    uint64_t expected_hash;
    uint8_t tail;
    size_t total_len;
    const char *what;
};

struct kpu_runtime_command_run {
    uint64_t command_paddr;
    const uint32_t *commands;
    size_t command_count;
    const char *command_file_path;
    size_t command_file_offset;
    size_t command_file_len;
    const struct kpu_runtime_section *sections;
    size_t section_count;
    const struct kpu_runtime_check *checks;
    size_t check_count;
};

struct kpu_runtime_image {
    const char *name;
    uint64_t command_paddr;
    const uint32_t *commands;
    size_t command_count;
    const char *command_file_path;
    size_t command_file_offset;
    size_t command_file_len;
    const struct kpu_runtime_section *sections;
    size_t section_count;
    const struct kpu_runtime_check *checks;
    size_t check_count;
    const struct kpu_runtime_command_run *runs;
    size_t run_count;
};

struct kpu_runtime_loaded_image {
    char name[KPU_RUNTIME_IMAGE_MAX_NAME];
    uint32_t *commands;
    char *command_file_path;
    struct kpu_runtime_command_run *runs;
    struct kpu_runtime_section *sections;
    struct kpu_runtime_section *run_sections;
    struct kpu_runtime_check *checks;
    struct kpu_runtime_check *run_checks;
    uint8_t *payload;
    size_t payload_len;
    size_t payload_cap;
    size_t run_check_count;
    size_t run_section_count;
    size_t current_run;
    struct kpu_runtime_image image;
};

static const struct kpu_runtime_window KPU_RUNTIME_WINDOWS[] = {
    {
        .id = KPU_RUNTIME_WINDOW_L2,
        .name = "KPU L2",
        .paddr = KPU_L2_PADDR,
        .size = KPU_L2_SIZE,
        .mmap_offset = KPU_MMAP_L2_OFFSET,
    },
    {
        .id = KPU_RUNTIME_WINDOW_RDATA,
        .name = "KPU runtime RDATA",
        .paddr = KPU_RUNTIME_RDATA_PADDR,
        .size = KPU_RUNTIME_RDATA_SIZE,
        .mmap_offset = KPU_MMAP_RUNTIME_RDATA_OFFSET,
    },
    {
        .id = KPU_RUNTIME_WINDOW_FAKE_OUTPUT,
        .name = "KPU fake output",
        .paddr = KPU_FAKE_OUTPUT_PADDR,
        .size = KPU_FAKE_OUTPUT_SIZE,
        .mmap_offset = KPU_MMAP_FAKE_OUTPUT_OFFSET,
    },
    {
        .id = KPU_RUNTIME_WINDOW_COMMAND,
        .name = "KPU runtime command",
        .paddr = KPU_RUNTIME_COMMAND_PADDR,
        .size = KPU_RUNTIME_COMMAND_SIZE,
        .mmap_offset = KPU_MMAP_RUNTIME_COMMAND_OFFSET,
    },
    {
        .id = KPU_RUNTIME_WINDOW_DIRECT_IO,
        .name = "KPU runtime direct I/O",
        .paddr = KPU_RUNTIME_DIRECT_IO_PADDR,
        .size = KPU_RUNTIME_DIRECT_IO_SIZE,
        .mmap_offset = KPU_MMAP_RUNTIME_DIRECT_IO_OFFSET,
    },
    {
        .id = KPU_RUNTIME_WINDOW_DDR,
        .name = "KPU runtime DDR mirror",
        .paddr = KPU_RUNTIME_DDR_PADDR,
        .size = KPU_RUNTIME_DDR_SIZE,
        .mmap_offset = KPU_MMAP_RUNTIME_DDR_OFFSET,
    },
};

static int fail_errno(const char *what)
{
    printf("KPU_SMOKE_FAIL: %s: %s\n", what, strerror(errno));
    return 1;
}

static int fail_msg(const char *what)
{
    printf("KPU_SMOKE_FAIL: %s\n", what);
    return 1;
}

static uint64_t fnv1a64_update(uint64_t hash, uint8_t value)
{
    return (hash ^ value) * 1099511628211ull;
}

static uint64_t fnv1a64_mmio(volatile const uint8_t *data, size_t len)
{
    uint64_t hash = 0xcbf29ce484222325ull;
    for (size_t i = 0; i < len; i++) {
        hash = fnv1a64_update(hash, data[i]);
    }
    return hash;
}

static void write_u32_buf_le(uint8_t *base, size_t offset, uint32_t value)
{
    uint8_t *bytes = base + offset;
    bytes[0] = (uint8_t)value;
    bytes[1] = (uint8_t)(value >> 8);
    bytes[2] = (uint8_t)(value >> 16);
    bytes[3] = (uint8_t)(value >> 24);
}

static int parse_u64_token(const char *text, uint64_t *value, const char *what)
{
    char *end = NULL;
    errno = 0;
    unsigned long long parsed = strtoull(text, &end, 0);
    if (errno != 0 || end == text || *end != '\0') {
        return fail_msg(what);
    }
    *value = (uint64_t)parsed;
    return 0;
}

static int parse_size_token(const char *text, size_t *value, const char *what)
{
    uint64_t parsed = 0;
    if (parse_u64_token(text, &parsed, what) != 0) {
        return 1;
    }
    if (parsed > SIZE_MAX) {
        return fail_msg(what);
    }
    *value = (size_t)parsed;
    return 0;
}

static int parse_u32_token(const char *text, uint32_t *value, const char *what)
{
    uint64_t parsed = 0;
    if (parse_u64_token(text, &parsed, what) != 0) {
        return 1;
    }
    if (parsed > UINT32_MAX) {
        return fail_msg(what);
    }
    *value = (uint32_t)parsed;
    return 0;
}

static int parse_u8_token(const char *text, uint8_t *value, const char *what)
{
    uint64_t parsed = 0;
    if (parse_u64_token(text, &parsed, what) != 0) {
        return 1;
    }
    if (parsed > UINT8_MAX) {
        return fail_msg(what);
    }
    *value = (uint8_t)parsed;
    return 0;
}

static void copy_to_mmio(volatile uint8_t *dst, const void *src, size_t len)
{
    const uint8_t *bytes = src;
    for (size_t i = 0; i < len; i++) {
        dst[i] = bytes[i];
    }
}

static void memset_mmio(volatile uint8_t *dst, uint8_t value, size_t len)
{
    for (size_t i = 0; i < len; i++) {
        dst[i] = value;
    }
}

static int copy_file_to_mmio(volatile uint8_t *dst, const char *path, size_t file_offset,
                             size_t len)
{
    FILE *file = fopen(path, "rb");
    if (file == NULL) {
        return fail_errno(path);
    }

    int failed = 0;
    if (fseeko(file, (off_t)file_offset, SEEK_SET) != 0) {
        failed = fail_errno("seek runtime image blob");
    }

    size_t copied = 0;
    while (failed == 0 && copied < len) {
        uint8_t buf[4096];
        size_t want = len - copied;
        if (want > sizeof(buf)) {
            want = sizeof(buf);
        }

        size_t nread = fread(buf, 1, want, file);
        if (nread != want) {
            failed = ferror(file) != 0 ? fail_errno(path) :
                                         fail_msg("runtime image blob read was short");
            break;
        }
        copy_to_mmio(dst + copied, buf, nread);
        copied += nread;
    }

    if (fclose(file) != 0 && failed == 0) {
        failed = fail_errno(path);
    }
    return failed;
}

static int expect_bytes(volatile uint8_t *data, const uint8_t *expected, size_t expected_len,
                        uint8_t tail, size_t total_len, const char *what)
{
    for (size_t i = 0; i < total_len; i++) {
        uint8_t want = i < expected_len ? expected[i] : tail;
        if (data[i] != want) {
            return fail_msg(what);
        }
    }
    return 0;
}

static int run_command_and_wait(int fd, uint64_t start_paddr, size_t command_size,
                                int expect_irq, const char *what, uint64_t *status_out,
                                uint64_t *irq_before_out, uint64_t *irq_after_out);

static const struct kpu_runtime_window *runtime_window_desc(enum kpu_runtime_window_id id)
{
    for (size_t i = 0; i < sizeof(KPU_RUNTIME_WINDOWS) / sizeof(KPU_RUNTIME_WINDOWS[0]); i++) {
        if (KPU_RUNTIME_WINDOWS[i].id == id) {
            return &KPU_RUNTIME_WINDOWS[i];
        }
    }
    return NULL;
}

static int runtime_window_from_name(const char *name, enum kpu_runtime_window_id *window)
{
    if (strcmp(name, "l2") == 0) {
        *window = KPU_RUNTIME_WINDOW_L2;
    } else if (strcmp(name, "rdata") == 0) {
        *window = KPU_RUNTIME_WINDOW_RDATA;
    } else if (strcmp(name, "fake_output") == 0) {
        *window = KPU_RUNTIME_WINDOW_FAKE_OUTPUT;
    } else if (strcmp(name, "command") == 0) {
        *window = KPU_RUNTIME_WINDOW_COMMAND;
    } else if (strcmp(name, "direct_io") == 0) {
        *window = KPU_RUNTIME_WINDOW_DIRECT_IO;
    } else if (strcmp(name, "ddr") == 0) {
        *window = KPU_RUNTIME_WINDOW_DDR;
    } else {
        return fail_msg("runtime image references an unknown named window");
    }
    return 0;
}

static int runtime_range_to_offset(enum kpu_runtime_window_id window, uint64_t paddr,
                                   size_t len, size_t *offset)
{
    const struct kpu_runtime_window *desc = runtime_window_desc(window);
    if (desc == NULL) {
        return fail_msg("runtime image references an unknown window");
    }
    if (paddr < desc->paddr || len > desc->size || paddr - desc->paddr > desc->size - len) {
        return fail_msg("runtime image command range is outside its mmap window");
    }
    *offset = (size_t)(paddr - desc->paddr);
    return 0;
}

static int runtime_section_fits(const struct kpu_runtime_section *section)
{
    const struct kpu_runtime_window *desc = runtime_window_desc(section->window);
    if (desc == NULL) {
        return fail_msg("runtime image section references an unknown window");
    }
    if (section->len > desc->size || section->offset > desc->size - section->len) {
        return fail_msg("runtime image section does not fit in its mmap window");
    }
    if (section->kind == KPU_RUNTIME_SECTION_COPY) {
        if (section->data == NULL) {
            return fail_msg("runtime image copy section has no data");
        }
    } else if (section->kind == KPU_RUNTIME_SECTION_COPY_FILE) {
        if (section->file_path == NULL) {
            return fail_msg("runtime image file copy section has no path");
        }
    } else if (section->kind != KPU_RUNTIME_SECTION_FILL) {
        return fail_msg("runtime image section kind is invalid");
    }
    return 0;
}

static int runtime_check_fits(const struct kpu_runtime_check *check)
{
    const struct kpu_runtime_window *desc = runtime_window_desc(check->window);
    if (desc == NULL) {
        return fail_msg("runtime image check references an unknown window");
    }
    if (check->total_len > desc->size || check->offset > desc->size - check->total_len) {
        return fail_msg("runtime image check does not fit in its mmap window");
    }
    if (check->kind == KPU_RUNTIME_CHECK_BYTES) {
        if (check->expected_len > check->total_len || check->expected == NULL) {
            return fail_msg("runtime image check has invalid expected bytes");
        }
    } else if (check->kind != KPU_RUNTIME_CHECK_FNV1A64) {
        return fail_msg("runtime image check kind is invalid");
    }
    return 0;
}

static int runtime_map_window(int fd, enum kpu_runtime_window_id window, void **map)
{
    const struct kpu_runtime_window *desc = runtime_window_desc(window);
    if (desc == NULL) {
        return fail_msg("runtime image requested an unknown mmap window");
    }
    if (map[window] != NULL) {
        return 0;
    }

    void *mapped = mmap(NULL, desc->size, PROT_READ | PROT_WRITE, MAP_SHARED, fd,
                        desc->mmap_offset);
    if (mapped == MAP_FAILED) {
        return fail_errno(desc->name);
    }
    map[window] = mapped;
    return 0;
}

static int runtime_unmap_windows(void **map)
{
    int failed = 0;
    for (size_t i = 0; i < KPU_RUNTIME_WINDOW_COUNT; i++) {
        if (map[i] == NULL) {
            continue;
        }

        const struct kpu_runtime_window *desc = runtime_window_desc((enum kpu_runtime_window_id)i);
        if (desc == NULL) {
            failed = fail_msg("runtime image lost mmap window metadata");
            continue;
        }
        if (munmap(map[i], desc->size) != 0 && failed == 0) {
            failed = fail_errno(desc->name);
        }
    }
    return failed;
}

static int runtime_command_run_size(const struct kpu_runtime_command_run *run, size_t *size)
{
    const int has_inline_commands = run->commands != NULL && run->command_count != 0;
    const int has_file_commands = run->command_file_path != NULL && run->command_file_len != 0;
    if (!has_inline_commands && !has_file_commands) {
        return fail_msg("runtime image run has no commands");
    }
    if (has_inline_commands && has_file_commands) {
        return fail_msg("runtime image run has both inline and file commands");
    }
    if (run->command_count > SIZE_MAX / sizeof(run->commands[0])) {
        return fail_msg("runtime image run command stream is too large");
    }
    *size = has_file_commands ? run->command_file_len :
                                run->command_count * sizeof(run->commands[0]);
    return 0;
}

static int runtime_run_checks(const struct kpu_runtime_image *image,
                              const struct kpu_runtime_check *checks, size_t check_count,
                              void **map)
{
    for (size_t i = 0; i < check_count; i++) {
        const struct kpu_runtime_check *check = &checks[i];
        volatile uint8_t *base = map[check->window];
        if (check->kind == KPU_RUNTIME_CHECK_BYTES) {
            int failed =
                expect_bytes(base + check->offset, check->expected, check->expected_len,
                             check->tail, check->total_len, check->what);
            if (failed != 0) {
                return failed;
            }
        } else {
            uint64_t hash = fnv1a64_mmio(base + check->offset, check->total_len);
            if (hash != check->expected_hash) {
                const struct kpu_runtime_window *desc = runtime_window_desc(check->window);
                printf("KPU_SMOKE_FAIL: %s image=%s window=%s offset=0x%zx len=%zu "
                       "actual=0x%016llx expected=0x%016llx\n",
                       check->what, image->name, desc != NULL ? desc->name : "unknown",
                       check->offset, check->total_len, (unsigned long long)hash,
                       (unsigned long long)check->expected_hash);
                return 1;
            }
        }
    }
    return 0;
}

static int runtime_apply_sections(const struct kpu_runtime_section *sections, size_t section_count,
                                  void **map)
{
    for (size_t i = 0; i < section_count; i++) {
        const struct kpu_runtime_section *section = &sections[i];
        volatile uint8_t *base = map[section->window];
        if (section->kind == KPU_RUNTIME_SECTION_COPY) {
            copy_to_mmio(base + section->offset, section->data, section->len);
        } else if (section->kind == KPU_RUNTIME_SECTION_COPY_FILE) {
            int failed = copy_file_to_mmio(base + section->offset, section->file_path,
                                           section->file_offset, section->len);
            if (failed != 0) {
                return failed;
            }
        } else {
            memset_mmio(base + section->offset, section->fill, section->len);
        }
    }
    return 0;
}

static int run_runtime_image(int fd, const struct kpu_runtime_image *image, int expect_irq)
{
    const int has_inline_commands = image->commands != NULL && image->command_count != 0;
    const int has_file_commands = image->command_file_path != NULL && image->command_file_len != 0;
    const int has_legacy_command = has_inline_commands || has_file_commands;
    if (image->run_count != 0 && has_legacy_command) {
        return fail_msg("runtime image has both legacy command and command runs");
    }

    struct kpu_runtime_command_run legacy_run = {
        .command_paddr = image->command_paddr,
        .commands = image->commands,
        .command_count = image->command_count,
        .command_file_path = image->command_file_path,
        .command_file_offset = image->command_file_offset,
        .command_file_len = image->command_file_len,
        .sections = NULL,
        .section_count = 0,
        .checks = NULL,
        .check_count = 0,
    };
    const struct kpu_runtime_command_run *runs = image->runs;
    size_t run_count = image->run_count;
    if (run_count == 0) {
        runs = &legacy_run;
        run_count = 1;
    }
    if (run_count > 1) {
        printf("KPU_SMOKE: runtime_image_begin %s runs=%zu sections=%zu checks=%zu\n",
               image->name, run_count, image->section_count, image->check_count);
        fflush(stdout);
    }

    void *map[KPU_RUNTIME_WINDOW_COUNT] = {0};
    int failed = 0;
    for (size_t i = 0; failed == 0 && i < run_count; i++) {
        size_t command_size = 0;
        size_t command_offset = 0;
        failed = runtime_command_run_size(&runs[i], &command_size);
        if (failed == 0) {
            failed = runtime_range_to_offset(KPU_RUNTIME_WINDOW_COMMAND, runs[i].command_paddr,
                                             command_size, &command_offset);
        }
        if (failed == 0) {
            failed = runtime_map_window(fd, KPU_RUNTIME_WINDOW_COMMAND, map);
        }
        for (size_t j = 0; failed == 0 && j < runs[i].section_count; j++) {
            failed = runtime_section_fits(&runs[i].sections[j]);
            if (failed == 0) {
                failed = runtime_map_window(fd, runs[i].sections[j].window, map);
            }
        }
        for (size_t j = 0; failed == 0 && j < runs[i].check_count; j++) {
            failed = runtime_check_fits(&runs[i].checks[j]);
            if (failed == 0) {
                failed = runtime_map_window(fd, runs[i].checks[j].window, map);
            }
        }
    }
    for (size_t i = 0; failed == 0 && i < image->section_count; i++) {
        failed = runtime_section_fits(&image->sections[i]);
        if (failed == 0) {
            failed = runtime_map_window(fd, image->sections[i].window, map);
        }
    }
    for (size_t i = 0; failed == 0 && i < image->check_count; i++) {
        failed = runtime_check_fits(&image->checks[i]);
        if (failed == 0) {
            failed = runtime_map_window(fd, image->checks[i].window, map);
        }
    }

    if (failed == 0) {
        failed = runtime_apply_sections(image->sections, image->section_count, map);
    }

    uint64_t status = 0;
    uint64_t irq_before = 0;
    uint64_t irq_after = 0;
    uint64_t first_irq_before = 0;
    for (size_t i = 0; failed == 0 && i < run_count; i++) {
        const struct kpu_runtime_command_run *run = &runs[i];
        const int run_has_file_commands =
            run->command_file_path != NULL && run->command_file_len != 0;
        size_t command_size = 0;
        size_t command_offset = 0;
        failed = runtime_command_run_size(run, &command_size);
        if (failed == 0) {
            failed = runtime_range_to_offset(KPU_RUNTIME_WINDOW_COMMAND, run->command_paddr,
                                             command_size, &command_offset);
        }
        if (failed == 0) {
            failed = runtime_apply_sections(run->sections, run->section_count, map);
        }
        if (failed == 0) {
            volatile uint8_t *command = map[KPU_RUNTIME_WINDOW_COMMAND];
            if (run_has_file_commands) {
                failed = copy_file_to_mmio(command + command_offset, run->command_file_path,
                                           run->command_file_offset, command_size);
            } else {
                copy_to_mmio(command + command_offset, run->commands, command_size);
            }
        }
        if (failed == 0) {
            failed = run_command_and_wait(fd, run->command_paddr, command_size, expect_irq,
                                          "KPU_IOC_RUN runtime image command", &status,
                                          &irq_before, &irq_after);
            if (i == 0) {
                first_irq_before = irq_before;
            }
            if (failed == 0 && run_count > 1 &&
                (i == 0 || i + 1 == run_count || (i + 1) % 16 == 0)) {
                printf("KPU_SMOKE: runtime_image_progress %s run=%zu/%zu irq_count=%llu\n",
                       image->name, i + 1, run_count, (unsigned long long)irq_after);
                fflush(stdout);
            }
        }
        if (failed == 0) {
            failed = runtime_run_checks(image, run->checks, run->check_count, map);
        }
    }
    if (failed == 0) {
        failed = runtime_run_checks(image, image->checks, image->check_count, map);
    }
    if (failed == 0) {
        if (run_count == 1) {
            printf("KPU_SMOKE: runtime_image %s status=0x%016llx irq_count=%llu->%llu\n",
                   image->name, (unsigned long long)status, (unsigned long long)irq_before,
                   (unsigned long long)irq_after);
        } else {
            printf("KPU_SMOKE: runtime_image %s runs=%zu status=0x%016llx "
                   "irq_count=%llu->%llu\n",
                   image->name, run_count, (unsigned long long)status,
                   (unsigned long long)first_irq_before, (unsigned long long)irq_after);
        }
    }

    int unmap_failed = runtime_unmap_windows(map);
    return failed != 0 ? failed : unmap_failed;
}

static size_t split_runtime_image_tokens(char *line, char **tokens, size_t max_tokens)
{
    size_t count = 0;
    char *save = NULL;
    char *token = strtok_r(line, " \t\r\n", &save);
    while (token != NULL && count < max_tokens) {
        if (token[0] == '#') {
            break;
        }
        tokens[count++] = token;
        token = strtok_r(NULL, " \t\r\n", &save);
    }
    return count;
}

static int append_runtime_payload(struct kpu_runtime_loaded_image *loaded, char **tokens,
                                  size_t token_count, const uint8_t **data, size_t *len)
{
    if (token_count > KPU_RUNTIME_IMAGE_MAX_PAYLOAD - loaded->payload_len) {
        return fail_msg("runtime image payload is too large");
    }
    size_t want = loaded->payload_len + token_count;
    if (want > loaded->payload_cap) {
        size_t new_cap = loaded->payload_cap == 0 ? 256 : loaded->payload_cap;
        while (new_cap < want) {
            size_t next_cap = new_cap * 2;
            if (next_cap < new_cap || next_cap > KPU_RUNTIME_IMAGE_MAX_PAYLOAD) {
                next_cap = KPU_RUNTIME_IMAGE_MAX_PAYLOAD;
            }
            new_cap = next_cap;
        }

        uint8_t *payload = realloc(loaded->payload, new_cap);
        if (payload == NULL) {
            return fail_msg("runtime image payload allocation failed");
        }
        loaded->payload = payload;
        loaded->payload_cap = new_cap;
    }

    size_t start = loaded->payload_len;
    for (size_t i = 0; i < token_count; i++) {
        if (parse_u8_token(tokens[i], &loaded->payload[loaded->payload_len],
                           "runtime image byte is invalid") != 0) {
            return 1;
        }
        loaded->payload_len++;
    }
    *data = &loaded->payload[start];
    *len = token_count;
    return 0;
}

static void free_runtime_loaded_image(struct kpu_runtime_loaded_image *loaded)
{
    if (loaded->runs != NULL) {
        for (size_t i = 0; i < loaded->image.run_count; i++) {
            free((void *)loaded->runs[i].command_file_path);
        }
    }
    if (loaded->sections != NULL) {
        for (size_t i = 0; i < loaded->image.section_count; i++) {
            free((void *)loaded->sections[i].file_path);
        }
    }
    if (loaded->run_sections != NULL) {
        for (size_t i = 0; i < loaded->run_section_count; i++) {
            free((void *)loaded->run_sections[i].file_path);
        }
    }
    free(loaded->commands);
    free(loaded->command_file_path);
    free(loaded->runs);
    free(loaded->sections);
    free(loaded->run_sections);
    free(loaded->checks);
    free(loaded->run_checks);
    free(loaded->payload);
    memset(loaded, 0, sizeof(*loaded));
}

static int init_runtime_loaded_image(struct kpu_runtime_loaded_image *loaded)
{
    memset(loaded, 0, sizeof(*loaded));
    loaded->commands = calloc(KPU_RUNTIME_IMAGE_MAX_COMMANDS, sizeof(loaded->commands[0]));
    loaded->runs = calloc(KPU_RUNTIME_IMAGE_MAX_RUNS, sizeof(loaded->runs[0]));
    loaded->sections = calloc(KPU_RUNTIME_IMAGE_MAX_SECTIONS, sizeof(loaded->sections[0]));
    loaded->run_sections = calloc(KPU_RUNTIME_IMAGE_MAX_SECTIONS, sizeof(loaded->run_sections[0]));
    loaded->checks = calloc(KPU_RUNTIME_IMAGE_MAX_CHECKS, sizeof(loaded->checks[0]));
    loaded->run_checks = calloc(KPU_RUNTIME_IMAGE_MAX_CHECKS, sizeof(loaded->run_checks[0]));
    if (loaded->commands == NULL || loaded->runs == NULL || loaded->sections == NULL ||
        loaded->run_sections == NULL || loaded->checks == NULL || loaded->run_checks == NULL) {
        free_runtime_loaded_image(loaded);
        return fail_msg("runtime image allocation failed");
    }

    snprintf(loaded->name, sizeof(loaded->name), "file-runtime-image");
    loaded->image.name = loaded->name;
    loaded->image.commands = loaded->commands;
    loaded->image.runs = loaded->runs;
    loaded->image.sections = loaded->sections;
    loaded->image.checks = loaded->checks;
    loaded->current_run = SIZE_MAX;
    return 0;
}

static int set_runtime_command_file(struct kpu_runtime_loaded_image *loaded, char **tokens,
                                    size_t token_count)
{
    if (token_count != 4 || loaded->image.run_count != 0 ||
        loaded->image.command_count != 0 ||
        loaded->image.command_file_path != NULL ||
        parse_size_token(tokens[2], &loaded->image.command_file_offset,
                         "runtime image command_file offset is invalid") != 0 ||
        parse_size_token(tokens[3], &loaded->image.command_file_len,
                         "runtime image command_file length is invalid") != 0) {
        return 1;
    }

    char *path = strdup(tokens[1]);
    if (path == NULL) {
        return fail_msg("runtime image command_file path allocation failed");
    }
    loaded->command_file_path = path;
    loaded->image.command_file_path = path;
    return 0;
}

static int append_runtime_run_file(struct kpu_runtime_loaded_image *loaded, char **tokens,
                                   size_t token_count)
{
    if (token_count != 5 || loaded->image.command_count != 0 ||
        loaded->image.command_file_path != NULL ||
        loaded->image.run_count >= KPU_RUNTIME_IMAGE_MAX_RUNS) {
        return fail_msg("runtime image run_file line is invalid");
    }

    struct kpu_runtime_command_run *run = &loaded->runs[loaded->image.run_count];
    if (parse_u64_token(tokens[1], &run->command_paddr,
                        "runtime image run_file paddr is invalid") != 0 ||
        parse_size_token(tokens[3], &run->command_file_offset,
                         "runtime image run_file offset is invalid") != 0 ||
        parse_size_token(tokens[4], &run->command_file_len,
                         "runtime image run_file length is invalid") != 0) {
        return 1;
    }

    char *path = strdup(tokens[2]);
    if (path == NULL) {
        return fail_msg("runtime image run_file path allocation failed");
    }
    run->command_file_path = path;
    run->checks = &loaded->run_checks[loaded->run_check_count];
    loaded->current_run = loaded->image.run_count;
    loaded->image.run_count++;
    return 0;
}

static int append_runtime_command(struct kpu_runtime_loaded_image *loaded, char **tokens,
                                  size_t token_count)
{
    if (loaded->image.command_file_path != NULL || loaded->image.run_count != 0) {
        return fail_msg("runtime image has both inline and file commands");
    }
    for (size_t i = 1; i < token_count; i++) {
        if (loaded->image.command_count >= KPU_RUNTIME_IMAGE_MAX_COMMANDS) {
            return fail_msg("runtime image has too many commands");
        }
        if (parse_u32_token(tokens[i], &loaded->commands[loaded->image.command_count],
                            "runtime image command word is invalid") != 0) {
            return 1;
        }
        loaded->image.command_count++;
    }
    return 0;
}

static int append_runtime_file_section(struct kpu_runtime_section *section, char **tokens,
                                       size_t token_count)
{
    if (token_count != 6 ||
        parse_size_token(tokens[4], &section->file_offset,
                         "runtime image file copy offset is invalid") != 0 ||
        parse_size_token(tokens[5], &section->len,
                         "runtime image file copy length is invalid") != 0) {
        return 1;
    }

    char *path = strdup(tokens[3]);
    if (path == NULL) {
        return fail_msg("runtime image file copy path allocation failed");
    }
    section->kind = KPU_RUNTIME_SECTION_COPY_FILE;
    section->file_path = path;
    return 0;
}

static int parse_runtime_section(struct kpu_runtime_loaded_image *loaded,
                                 struct kpu_runtime_section *section, char **tokens,
                                 size_t token_count)
{
    if (token_count < 4) {
        return fail_msg("runtime image section line is invalid");
    }

    if (runtime_window_from_name(tokens[1], &section->window) != 0 ||
        parse_size_token(tokens[2], &section->offset,
                         "runtime image section offset is invalid") != 0) {
        return 1;
    }

    if (strcmp(tokens[0], "copy") == 0 || strcmp(tokens[0], "run_copy") == 0) {
        section->kind = KPU_RUNTIME_SECTION_COPY;
        if (append_runtime_payload(loaded, &tokens[3], token_count - 3, &section->data,
                                   &section->len) != 0) {
            return 1;
        }
    } else if (strcmp(tokens[0], "copy_file") == 0 ||
               strcmp(tokens[0], "run_copy_file") == 0) {
        if (append_runtime_file_section(section, tokens, token_count) != 0) {
            return 1;
        }
    } else if (strcmp(tokens[0], "fill") == 0 || strcmp(tokens[0], "run_fill") == 0) {
        section->kind = KPU_RUNTIME_SECTION_FILL;
        if (token_count != 5 ||
            parse_size_token(tokens[3], &section->len,
                             "runtime image fill length is invalid") != 0 ||
            parse_u8_token(tokens[4], &section->fill,
                           "runtime image fill byte is invalid") != 0) {
            return 1;
        }
    } else {
        return fail_msg("runtime image section kind is invalid");
    }

    return 0;
}

static int append_runtime_section(struct kpu_runtime_loaded_image *loaded, char **tokens,
                                  size_t token_count)
{
    if (loaded->image.section_count >= KPU_RUNTIME_IMAGE_MAX_SECTIONS) {
        return fail_msg("runtime image has too many sections");
    }

    struct kpu_runtime_section *section = &loaded->sections[loaded->image.section_count];
    if (parse_runtime_section(loaded, section, tokens, token_count) != 0) {
        return 1;
    }

    loaded->image.section_count++;
    return 0;
}

static int append_runtime_run_section(struct kpu_runtime_loaded_image *loaded, char **tokens,
                                      size_t token_count)
{
    if (loaded->current_run == SIZE_MAX ||
        loaded->run_section_count >= KPU_RUNTIME_IMAGE_MAX_SECTIONS) {
        return fail_msg("runtime image run section line is invalid");
    }

    struct kpu_runtime_command_run *run = &loaded->runs[loaded->current_run];
    struct kpu_runtime_section *section = &loaded->run_sections[loaded->run_section_count];
    if (run->sections == NULL) {
        run->sections = section;
    } else if (run->sections + run->section_count != section) {
        return fail_msg("runtime image run sections must be contiguous");
    }
    if (parse_runtime_section(loaded, section, tokens, token_count) != 0) {
        return 1;
    }

    loaded->run_section_count++;
    run->section_count++;
    return 0;
}

static int append_runtime_check(struct kpu_runtime_loaded_image *loaded, char **tokens,
                                size_t token_count)
{
    if (token_count < 5 || loaded->image.check_count >= KPU_RUNTIME_IMAGE_MAX_CHECKS) {
        return fail_msg("runtime image check line is invalid");
    }

    struct kpu_runtime_check *check = &loaded->checks[loaded->image.check_count];
    check->kind = KPU_RUNTIME_CHECK_BYTES;
    if (runtime_window_from_name(tokens[1], &check->window) != 0 ||
        parse_size_token(tokens[2], &check->offset,
                         "runtime image check offset is invalid") != 0 ||
        parse_size_token(tokens[3], &check->total_len,
                         "runtime image check length is invalid") != 0 ||
        parse_u8_token(tokens[4], &check->tail,
                       "runtime image check tail byte is invalid") != 0 ||
        append_runtime_payload(loaded, &tokens[5], token_count - 5, &check->expected,
                               &check->expected_len) != 0) {
        return 1;
    }
    check->what = "KPU runtime file output did not match expected bytes";
    loaded->image.check_count++;
    return 0;
}

static int append_runtime_hash_check(struct kpu_runtime_loaded_image *loaded, char **tokens,
                                     size_t token_count)
{
    if (token_count != 5 || loaded->image.check_count >= KPU_RUNTIME_IMAGE_MAX_CHECKS) {
        return fail_msg("runtime image hash check line is invalid");
    }

    struct kpu_runtime_check *check = &loaded->checks[loaded->image.check_count];
    check->kind = KPU_RUNTIME_CHECK_FNV1A64;
    if (runtime_window_from_name(tokens[1], &check->window) != 0 ||
        parse_size_token(tokens[2], &check->offset,
                         "runtime image hash check offset is invalid") != 0 ||
        parse_size_token(tokens[3], &check->total_len,
                         "runtime image hash check length is invalid") != 0 ||
        parse_u64_token(tokens[4], &check->expected_hash,
                        "runtime image hash check value is invalid") != 0) {
        return 1;
    }
    check->what = "KPU runtime file output hash did not match expected bytes";
    loaded->image.check_count++;
    return 0;
}

static int append_runtime_run_hash_check(struct kpu_runtime_loaded_image *loaded, char **tokens,
                                         size_t token_count)
{
    if (token_count != 5 || loaded->current_run == SIZE_MAX ||
        loaded->run_check_count >= KPU_RUNTIME_IMAGE_MAX_CHECKS) {
        return fail_msg("runtime image run hash check line is invalid");
    }

    struct kpu_runtime_command_run *run = &loaded->runs[loaded->current_run];
    struct kpu_runtime_check *check = &loaded->run_checks[loaded->run_check_count];
    check->kind = KPU_RUNTIME_CHECK_FNV1A64;
    if (runtime_window_from_name(tokens[1], &check->window) != 0 ||
        parse_size_token(tokens[2], &check->offset,
                         "runtime image run hash check offset is invalid") != 0 ||
        parse_size_token(tokens[3], &check->total_len,
                         "runtime image run hash check length is invalid") != 0 ||
        parse_u64_token(tokens[4], &check->expected_hash,
                        "runtime image run hash check value is invalid") != 0) {
        return 1;
    }
    check->what = "KPU runtime sequence output hash did not match expected bytes";
    loaded->run_check_count++;
    run->check_count++;
    return 0;
}

static int load_runtime_image_file(const char *path, struct kpu_runtime_loaded_image *loaded)
{
    if (init_runtime_loaded_image(loaded) != 0) {
        return 1;
    }

    FILE *file = fopen(path, "r");
    if (file == NULL) {
        return fail_errno(path);
    }

    char line[KPU_RUNTIME_IMAGE_MAX_LINE];
    int failed = 0;
    while (failed == 0 && fgets(line, sizeof(line), file) != NULL) {
        char *tokens[KPU_RUNTIME_IMAGE_MAX_TOKENS] = {0};
        size_t token_count = split_runtime_image_tokens(line, tokens, KPU_RUNTIME_IMAGE_MAX_TOKENS);
        if (token_count == 0) {
            continue;
        }

        if (strcmp(tokens[0], "kpu-runtime-image-v1") == 0) {
            continue;
        } else if (strcmp(tokens[0], "name") == 0 && token_count == 2) {
            snprintf(loaded->name, sizeof(loaded->name), "%s", tokens[1]);
        } else if (strcmp(tokens[0], "command_paddr") == 0 && token_count == 2) {
            failed = parse_u64_token(tokens[1], &loaded->image.command_paddr,
                                     "runtime image command_paddr is invalid");
        } else if (strcmp(tokens[0], "command_file") == 0) {
            failed = set_runtime_command_file(loaded, tokens, token_count);
        } else if (strcmp(tokens[0], "run_file") == 0) {
            failed = append_runtime_run_file(loaded, tokens, token_count);
        } else if (strcmp(tokens[0], "commands") == 0) {
            failed = append_runtime_command(loaded, tokens, token_count);
        } else if (strcmp(tokens[0], "copy") == 0 || strcmp(tokens[0], "copy_file") == 0 ||
                   strcmp(tokens[0], "fill") == 0) {
            failed = append_runtime_section(loaded, tokens, token_count);
        } else if (strcmp(tokens[0], "run_copy") == 0 ||
                   strcmp(tokens[0], "run_copy_file") == 0 ||
                   strcmp(tokens[0], "run_fill") == 0) {
            failed = append_runtime_run_section(loaded, tokens, token_count);
        } else if (strcmp(tokens[0], "check") == 0) {
            failed = append_runtime_check(loaded, tokens, token_count);
        } else if (strcmp(tokens[0], "check_hash") == 0) {
            failed = append_runtime_hash_check(loaded, tokens, token_count);
        } else if (strcmp(tokens[0], "run_check_hash") == 0) {
            failed = append_runtime_run_hash_check(loaded, tokens, token_count);
        } else {
            failed = fail_msg("runtime image line is unknown");
        }
    }
    if (ferror(file) != 0 && failed == 0) {
        failed = fail_errno(path);
    }
    if (fclose(file) != 0 && failed == 0) {
        failed = fail_errno(path);
    }

    if (failed == 0 && loaded->image.run_count == 0 && loaded->image.command_paddr == 0) {
        failed = fail_msg("runtime image file did not set command_paddr");
    }
    if (failed == 0 && loaded->image.command_count == 0 &&
        loaded->image.command_file_path == NULL && loaded->image.run_count == 0) {
        failed = fail_msg("runtime image file did not provide commands");
    }
    return failed;
}

static int check_runtime_image_file(int fd, int expect_irq, const char *path)
{
    struct kpu_runtime_loaded_image loaded;
    if (load_runtime_image_file(path, &loaded) != 0) {
        free_runtime_loaded_image(&loaded);
        return 1;
    }
    printf("KPU_SMOKE: loaded_runtime_image path=%s name=%s runs=%zu sections=%zu checks=%zu "
           "payload=%zu\n",
           path, loaded.image.name, loaded.image.run_count, loaded.image.section_count,
           loaded.image.check_count, loaded.payload_len);
    fflush(stdout);
    int failed = run_runtime_image(fd, &loaded.image, expect_irq);
    free_runtime_loaded_image(&loaded);
    return failed;
}

static int check_optional_runtime_image_file(int fd, int expect_irq, const char *path)
{
    if (access(path, R_OK) != 0) {
        if (errno == ENOENT) {
            printf("KPU_SMOKE: optional_runtime_image not installed path=%s\n", path);
            return 0;
        }
        return fail_errno(path);
    }
    printf("KPU_SMOKE: optional_runtime_image loading path=%s\n", path);
    fflush(stdout);
    return check_runtime_image_file(fd, expect_irq, path);
}

static int check_optional_yolov8n_runtime_image(int fd, int expect_irq)
{
    if (access(KPU_YOLOV8N_DELTA_CAPTURE_FILE_PATH, R_OK) == 0) {
        printf("KPU_SMOKE: optional_runtime_image selecting full_sequence_delta path=%s\n",
               KPU_YOLOV8N_DELTA_CAPTURE_FILE_PATH);
        fflush(stdout);
        return check_runtime_image_file(fd, expect_irq, KPU_YOLOV8N_DELTA_CAPTURE_FILE_PATH);
    }
    if (errno != ENOENT) {
        return fail_errno(KPU_YOLOV8N_DELTA_CAPTURE_FILE_PATH);
    }
    if (access(KPU_YOLOV8N_FULL_CAPTURE_FILE_PATH, R_OK) == 0) {
        printf("KPU_SMOKE: optional_runtime_image selecting full_sequence path=%s\n",
               KPU_YOLOV8N_FULL_CAPTURE_FILE_PATH);
        fflush(stdout);
        return check_runtime_image_file(fd, expect_irq, KPU_YOLOV8N_FULL_CAPTURE_FILE_PATH);
    }
    if (errno != ENOENT) {
        return fail_errno(KPU_YOLOV8N_FULL_CAPTURE_FILE_PATH);
    }
    return check_optional_runtime_image_file(fd, expect_irq, KPU_YOLOV8N_CAPTURE_FILE_PATH);
}

static int has_optional_yolov8n_runtime_image(int *available)
{
    if (access(KPU_YOLOV8N_DELTA_CAPTURE_FILE_PATH, R_OK) == 0) {
        *available = 1;
        return 0;
    }
    if (errno != ENOENT) {
        return fail_errno(KPU_YOLOV8N_DELTA_CAPTURE_FILE_PATH);
    }

    if (access(KPU_YOLOV8N_FULL_CAPTURE_FILE_PATH, R_OK) == 0) {
        *available = 1;
        return 0;
    }
    if (errno != ENOENT) {
        return fail_errno(KPU_YOLOV8N_FULL_CAPTURE_FILE_PATH);
    }

    if (access(KPU_YOLOV8N_CAPTURE_FILE_PATH, R_OK) == 0) {
        *available = 1;
        return 0;
    }
    if (errno != ENOENT) {
        return fail_errno(KPU_YOLOV8N_CAPTURE_FILE_PATH);
    }

    *available = 0;
    return 0;
}

static uint32_t read_u32_le(const uint8_t *bytes)
{
    return (uint32_t)bytes[0] | ((uint32_t)bytes[1] << 8) | ((uint32_t)bytes[2] << 16) |
           ((uint32_t)bytes[3] << 24);
}

static int check_real_kmodel_asset(const char *path)
{
    FILE *file = fopen(path, "rb");
    if (file == NULL) {
        if (errno == ENOENT) {
            printf("KPU_SMOKE: real_kmodel not installed path=%s\n", path);
            return 0;
        }
        return fail_errno(path);
    }

    int failed = 0;
    struct stat st;
    if (fstat(fileno(file), &st) != 0) {
        failed = fail_errno("fstat real kmodel");
    } else if (st.st_size < 16) {
        failed = fail_msg("real kmodel file is too small");
    }

    uint8_t header[16] = {0};
    if (failed == 0 && fread(header, 1, sizeof(header), file) != sizeof(header)) {
        failed = fail_msg("real kmodel header read failed");
    }
    if (failed == 0 && memcmp(header, "LDMK", 4) != 0) {
        failed = fail_msg("real kmodel header did not contain LDMK magic");
    }

    uint64_t hash = 0xcbf29ce484222325ull;
    if (failed == 0) {
        if (fseek(file, 0, SEEK_SET) != 0) {
            failed = fail_errno("rewind real kmodel");
        }
    }
    while (failed == 0) {
        uint8_t buf[4096];
        size_t nread = fread(buf, 1, sizeof(buf), file);
        for (size_t i = 0; i < nread; i++) {
            hash = fnv1a64_update(hash, buf[i]);
        }
        if (nread < sizeof(buf)) {
            if (ferror(file) != 0) {
                failed = fail_errno("read real kmodel");
            }
            break;
        }
    }
    if (fclose(file) != 0 && failed == 0) {
        failed = fail_errno(path);
    }
    if (failed != 0) {
        return failed;
    }

    printf("KPU_SMOKE: real_kmodel path=%s size=%lld magic=LDMK version=%u hash=0x%016llx\n",
           path, (long long)st.st_size, read_u32_le(&header[4]), (unsigned long long)hash);
    return 0;
}

static int check_device_node(const char *path)
{
    int fd = open(path, O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        return fail_errno(path);
    }
    close(fd);
    printf("KPU_SMOKE: opened %s\n", path);
    return 0;
}

static int check_pread_reg(int fd)
{
    uint32_t value = 0;
    ssize_t nread = pread(fd, &value, sizeof(value), KPU_MMAP_CFG_OFFSET);
    if (nread < 0) {
        return fail_errno("pread /dev/kpu");
    }
    if (nread != (ssize_t)sizeof(value)) {
        return fail_msg("pread /dev/kpu returned a short register value");
    }
    printf("KPU_SMOKE: reg0=0x%08x\n", value);
    return 0;
}

static int check_info_ioctl(int fd, int *irq_wait)
{
    struct k230_kpu_info info = {0};
    if (ioctl(fd, KPU_IOC_GET_INFO, &info) != 0) {
        return fail_errno("KPU_IOC_GET_INFO");
    }

    printf("KPU_SMOKE: info cfg=0x%llx+0x%llx l2=0x%llx+0x%llx irq=%u flags=0x%x\n",
           (unsigned long long)info.cfg_paddr, (unsigned long long)info.cfg_size,
           (unsigned long long)info.l2_paddr, (unsigned long long)info.l2_size, info.irq,
           info.flags);

    if (info.cfg_paddr != KPU_CFG_PADDR || info.cfg_size < KPU_CFG_SIZE ||
        info.l2_paddr != KPU_L2_PADDR || info.l2_size < KPU_L2_SIZE) {
        return fail_msg("KPU_IOC_GET_INFO returned unexpected K230 resources");
    }
    if (info.irq != 189 && info.irq != KPU_IRQ_NONE) {
        return fail_msg("KPU_IOC_GET_INFO returned an unexpected KPU IRQ");
    }
    if ((info.flags & KPU_INFO_F_FDT) == 0) {
        return fail_msg("KPU_IOC_GET_INFO did not report FDT-probed resources");
    }
    if (info.irq != KPU_IRQ_NONE && (info.flags & KPU_INFO_F_IRQ_WAIT) == 0) {
        return fail_msg("KPU_IOC_GET_INFO did not report IRQ-backed wait support");
    }
    if ((info.flags & KPU_INFO_F_FAKE_OUTPUT) == 0) {
        return fail_msg("KPU_IOC_GET_INFO did not report QEMU fake output mmap support");
    }
    if ((info.flags & KPU_INFO_F_RUNTIME_SCRATCH) == 0) {
        return fail_msg("KPU_IOC_GET_INFO did not report QEMU runtime scratch mmap support");
    }
    *irq_wait = (info.flags & KPU_INFO_F_IRQ_WAIT) != 0;
    return 0;
}

static int read_irq_count(int fd, uint64_t *count)
{
    if (ioctl(fd, KPU_IOC_GET_IRQ_COUNT, count) != 0) {
        return fail_errno("KPU_IOC_GET_IRQ_COUNT");
    }
    return 0;
}

static int read_reg32(int fd, off_t offset, uint32_t *value)
{
    ssize_t nread = pread(fd, value, sizeof(*value), offset);
    if (nread < 0) {
        return fail_errno("pread KPU register");
    }
    if (nread != (ssize_t)sizeof(*value)) {
        return fail_msg("pread KPU register returned a short value");
    }
    return 0;
}

static int run_command_and_wait(int fd, uint64_t start_paddr, size_t command_size, int expect_irq,
                                const char *what, uint64_t *status_out, uint64_t *irq_before_out,
                                uint64_t *irq_after_out)
{
    uint64_t irq_before = 0;
    uint64_t irq_after = 0;
    if (read_irq_count(fd, &irq_before) != 0) {
        return 1;
    }

    struct k230_kpu_command_range range = {
        .start_paddr = start_paddr,
        .end_paddr = start_paddr + command_size,
    };
    if (ioctl(fd, KPU_IOC_RUN, &range) != 0) {
        return fail_errno(what);
    }
    if (ioctl(fd, KPU_IOC_WAIT_DONE, 10000000) != 0) {
        return fail_errno("KPU_IOC_WAIT_DONE runtime command");
    }

    uint64_t status = 0;
    if (ioctl(fd, KPU_IOC_GET_STATUS, &status) != 0) {
        return fail_errno("KPU_IOC_GET_STATUS after runtime command");
    }
    if ((status & KPU_DONE_STATUS) != KPU_DONE_STATUS) {
        return fail_msg("KPU status did not report done after runtime command");
    }

    if (ioctl(fd, KPU_IOC_CLEAR, 0) != 0) {
        return fail_errno("KPU_IOC_CLEAR after runtime command");
    }
    if (read_irq_count(fd, &irq_after) != 0) {
        return 1;
    }
    if (expect_irq && irq_after <= irq_before) {
        return fail_msg("KPU IRQ count did not increase after runtime command");
    }

    if (status_out != NULL) {
        *status_out = status;
    }
    if (irq_before_out != NULL) {
        *irq_before_out = irq_before;
    }
    if (irq_after_out != NULL) {
        *irq_after_out = irq_after;
    }
    return 0;
}

static int check_status_ioctl(int fd)
{
    uint64_t status = 0;
    if (ioctl(fd, KPU_IOC_GET_STATUS, &status) != 0) {
        return fail_errno("KPU_IOC_GET_STATUS");
    }
    printf("KPU_SMOKE: status=0x%016llx\n", (unsigned long long)status);

    if (ioctl(fd, KPU_IOC_CLEAR, 0) != 0) {
        return fail_errno("KPU_IOC_CLEAR");
    }
    printf("KPU_SMOKE: clear_done ok\n");
    return 0;
}

static int check_program_command_ioctl(int fd)
{
    struct k230_kpu_command_range empty = {
        .start_paddr = KPU_L2_PADDR,
        .end_paddr = KPU_L2_PADDR,
    };
    errno = 0;
    if (ioctl(fd, KPU_IOC_PROGRAM_COMMAND, &empty) == 0) {
        return fail_msg("KPU_IOC_PROGRAM_COMMAND accepted an empty range");
    }
    printf("KPU_SMOKE: empty_command_rejected errno=%d\n", errno);

    struct k230_kpu_command_range range = {
        .start_paddr = KPU_L2_PADDR,
        .end_paddr = KPU_L2_PADDR + 4,
    };
    if (ioctl(fd, KPU_IOC_PROGRAM_COMMAND, &range) != 0) {
        return fail_errno("KPU_IOC_PROGRAM_COMMAND");
    }

    uint32_t start = 0;
    uint32_t end = 0;
    uint32_t hi = 0;
    if (read_reg32(fd, KPU_COMMAND_START, &start) != 0 ||
        read_reg32(fd, KPU_COMMAND_END, &end) != 0 ||
        read_reg32(fd, KPU_COMMAND_HI, &hi) != 0) {
        return 1;
    }
    if (start != (uint32_t)range.start_paddr || end != (uint32_t)range.end_paddr ||
        hi != (uint32_t)(range.start_paddr >> 32)) {
        return fail_msg("KPU command registers did not match programmed range");
    }

    printf("KPU_SMOKE: program_command start=0x%08x end=0x%08x hi=0x%08x\n", start, end, hi);
    return 0;
}

static int check_cfg_mmap(int fd)
{
    volatile uint32_t *cfg = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd,
                                  KPU_MMAP_CFG_OFFSET);
    if (cfg == MAP_FAILED) {
        return fail_errno("mmap KPU CFG");
    }

    uint64_t status = ((uint64_t)cfg[KPU_STATUS_HI / sizeof(uint32_t)] << 32) |
                      cfg[KPU_STATUS_LO / sizeof(uint32_t)];
    printf("KPU_SMOKE: mmap_status=0x%016llx\n", (unsigned long long)status);

    if (munmap((void *)cfg, 4096) != 0) {
        return fail_errno("munmap KPU CFG");
    }
    return 0;
}

static int check_l2_mmap(int fd)
{
    volatile uint32_t *l2 = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd,
                                 KPU_MMAP_L2_OFFSET);
    if (l2 == MAP_FAILED) {
        return fail_errno("mmap KPU L2");
    }

    const uint32_t marker = 0x4b505532;
    l2[0] = marker;
    if (l2[0] != marker) {
        munmap((void *)l2, 4096);
        return fail_msg("KPU L2 mmap readback mismatch");
    }
    printf("KPU_SMOKE: l2_mmap_rw=0x%08x\n", marker);

    if (munmap((void *)l2, 4096) != 0) {
        return fail_errno("munmap KPU L2");
    }
    return 0;
}

static int check_run_wait_done_ioctl(int fd, int expect_irq)
{
    volatile uint32_t *l2 = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd,
                                 KPU_MMAP_L2_OFFSET);
    if (l2 == MAP_FAILED) {
        return fail_errno("mmap KPU L2 for command");
    }

    uint64_t irq_before = 0;
    uint64_t irq_after = 0;
    if (read_irq_count(fd, &irq_before) != 0) {
        munmap((void *)l2, 4096);
        return 1;
    }

    l2[0] = 0;
    struct k230_kpu_command_range range = {
        .start_paddr = KPU_L2_PADDR,
        .end_paddr = KPU_L2_PADDR + sizeof(uint32_t),
    };
    if (ioctl(fd, KPU_IOC_RUN, &range) != 0) {
        munmap((void *)l2, 4096);
        return fail_errno("KPU_IOC_RUN");
    }
    if (ioctl(fd, KPU_IOC_WAIT_DONE, 10000000) != 0) {
        munmap((void *)l2, 4096);
        return fail_errno("KPU_IOC_WAIT_DONE");
    }

    uint64_t status = 0;
    if (ioctl(fd, KPU_IOC_GET_STATUS, &status) != 0) {
        munmap((void *)l2, 4096);
        return fail_errno("KPU_IOC_GET_STATUS after run");
    }
    if ((status & KPU_DONE_STATUS) != KPU_DONE_STATUS) {
        munmap((void *)l2, 4096);
        return fail_msg("KPU status did not report done after run");
    }
    if (ioctl(fd, KPU_IOC_CLEAR, 0) != 0) {
        munmap((void *)l2, 4096);
        return fail_errno("KPU_IOC_CLEAR after run");
    }
    if (read_irq_count(fd, &irq_after) != 0) {
        munmap((void *)l2, 4096);
        return 1;
    }
    if (expect_irq && irq_after <= irq_before) {
        munmap((void *)l2, 4096);
        return fail_msg("KPU IRQ count did not increase after run");
    }
    printf("KPU_SMOKE: run_wait_done status=0x%016llx irq_count=%llu->%llu\n",
           (unsigned long long)status, (unsigned long long)irq_before,
           (unsigned long long)irq_after);

    if (munmap((void *)l2, 4096) != 0) {
        return fail_errno("munmap KPU L2 command");
    }
    return 0;
}

static int check_fake_output_mmap(int fd, int expect_irq)
{
    volatile uint32_t *l2 = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd,
                                 KPU_MMAP_L2_OFFSET);
    if (l2 == MAP_FAILED) {
        return fail_errno("mmap KPU L2 for fake output command");
    }

    volatile uint8_t *output = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd,
                                    KPU_MMAP_FAKE_OUTPUT_OFFSET);
    if (output == MAP_FAILED) {
        munmap((void *)l2, 4096);
        return fail_errno("mmap KPU fake output");
    }

    for (size_t i = 0; i < 4096; i++) {
        output[i] = 0xa5;
    }

    uint64_t irq_before = 0;
    uint64_t irq_after = 0;
    if (read_irq_count(fd, &irq_before) != 0) {
        munmap((void *)output, 4096);
        munmap((void *)l2, 4096);
        return 1;
    }

    l2[0] = (uint32_t)(KPU_FAKE_OUTPUT_PADDR | 2u);
    struct k230_kpu_command_range range = {
        .start_paddr = KPU_L2_PADDR,
        .end_paddr = KPU_L2_PADDR + sizeof(uint32_t),
    };
    if (ioctl(fd, KPU_IOC_RUN, &range) != 0) {
        munmap((void *)output, 4096);
        munmap((void *)l2, 4096);
        return fail_errno("KPU_IOC_RUN fake output command");
    }
    if (ioctl(fd, KPU_IOC_WAIT_DONE, 10000000) != 0) {
        munmap((void *)output, 4096);
        munmap((void *)l2, 4096);
        return fail_errno("KPU_IOC_WAIT_DONE fake output command");
    }

    uint64_t status = 0;
    if (ioctl(fd, KPU_IOC_GET_STATUS, &status) != 0) {
        munmap((void *)output, 4096);
        munmap((void *)l2, 4096);
        return fail_errno("KPU_IOC_GET_STATUS after fake output command");
    }
    if ((status & KPU_DONE_STATUS) != KPU_DONE_STATUS) {
        munmap((void *)output, 4096);
        munmap((void *)l2, 4096);
        return fail_msg("KPU status did not report done after fake output command");
    }

    for (size_t i = 0; i < 4096; i++) {
        if (output[i] != 0) {
            munmap((void *)output, 4096);
            munmap((void *)l2, 4096);
            return fail_msg("KPU fake output page was not zeroed");
        }
    }

    if (ioctl(fd, KPU_IOC_CLEAR, 0) != 0) {
        munmap((void *)output, 4096);
        munmap((void *)l2, 4096);
        return fail_errno("KPU_IOC_CLEAR after fake output command");
    }
    if (read_irq_count(fd, &irq_after) != 0) {
        munmap((void *)output, 4096);
        munmap((void *)l2, 4096);
        return 1;
    }
    if (expect_irq && irq_after <= irq_before) {
        munmap((void *)output, 4096);
        munmap((void *)l2, 4096);
        return fail_msg("KPU IRQ count did not increase after fake output command");
    }

    printf("KPU_SMOKE: fake_output_zeroed paddr=0x%llx status=0x%016llx irq_count=%llu->%llu\n",
           (unsigned long long)KPU_FAKE_OUTPUT_PADDR, (unsigned long long)status,
           (unsigned long long)irq_before, (unsigned long long)irq_after);

    if (munmap((void *)output, 4096) != 0) {
        munmap((void *)l2, 4096);
        return fail_errno("munmap KPU fake output");
    }
    if (munmap((void *)l2, 4096) != 0) {
        return fail_errno("munmap KPU L2 fake output command");
    }
    return 0;
}

static int check_runtime_arg_table_direct_io(int fd, int expect_irq)
{
    const uint8_t source[] = {
        0x41, 0x42, 0x43, 0x44,
    };
    uint8_t arg_table[12] = {0};
    const uint32_t commands[] = {
        GNNE_ADDI(2, 0, 0x200),
        GNNE_ADDI(4, 0, 1),
        GNNE_ADDI(5, 0, sizeof(source)),
        GNNE_MMU_CONF(0, 4, 0),
        GNNE_LW(6, 0, 0),
        GNNE_SS_PACK_SHAPE(4, 4, 4, 5, 0),
        GNNE_SS_PACK_STRIDE(5, 5, 5, 0),
        GNNE_SS_PACK_STRIDE(5, 5, 5, 1),
        GNNE_L2_LOAD_CONF(1, 0, 0, 0),
        GNNE_L2_LOAD(2, 6, 0),
        GNNE_LW(7, 0, 4),
        GNNE_L2_STORE_CONF(1, 0, 0, 0),
        GNNE_L2_STORE(7, 2, 0),
    };
    const size_t source_offset = KPU_RUNTIME_DIRECT_SOURCE_PADDR - KPU_RUNTIME_DIRECT_IO_PADDR;
    const size_t output_offset = KPU_RUNTIME_DIRECT_OUTPUT_PADDR - KPU_RUNTIME_DIRECT_IO_PADDR;
    const size_t output_len = 8;

    write_u32_buf_le(arg_table, 0, (uint32_t)KPU_RUNTIME_DIRECT_SOURCE_PADDR);
    write_u32_buf_le(arg_table, 4, (uint32_t)KPU_RUNTIME_DIRECT_OUTPUT_PADDR);
    write_u32_buf_le(arg_table, 8, (uint32_t)KPU_RUNTIME_RDATA_BASE);

    const struct kpu_runtime_section sections[] = {
        {
            .window = KPU_RUNTIME_WINDOW_DIRECT_IO,
            .offset = source_offset,
            .kind = KPU_RUNTIME_SECTION_COPY,
            .data = source,
            .len = sizeof(source),
        },
        {
            .window = KPU_RUNTIME_WINDOW_DIRECT_IO,
            .offset = output_offset,
            .kind = KPU_RUNTIME_SECTION_FILL,
            .fill = 0xa5,
            .len = output_len,
        },
        {
            .window = KPU_RUNTIME_WINDOW_L2,
            .offset = KPU_RUNTIME_ARG_TABLE_PADDR - KPU_L2_PADDR,
            .kind = KPU_RUNTIME_SECTION_COPY,
            .data = arg_table,
            .len = sizeof(arg_table),
        },
    };
    const struct kpu_runtime_check checks[] = {
        {
            .window = KPU_RUNTIME_WINDOW_DIRECT_IO,
            .offset = output_offset,
            .expected = source,
            .expected_len = sizeof(source),
            .tail = 0xa5,
            .total_len = output_len,
            .what = "KPU runtime direct output did not match source bytes",
        },
    };
    const struct kpu_runtime_image image = {
        .name = "runtime_arg_table_direct_io",
        .command_paddr = KPU_RUNTIME_FUNCTION_COMMAND_PADDR,
        .commands = commands,
        .command_count = sizeof(commands) / sizeof(commands[0]),
        .sections = sections,
        .section_count = sizeof(sections) / sizeof(sections[0]),
        .checks = checks,
        .check_count = sizeof(checks) / sizeof(checks[0]),
    };
    return run_runtime_image(fd, &image, expect_irq);
}

static int check_runtime_ddr_mirror(int fd, int expect_irq)
{
    const uint8_t source[] = {
        0x61, 0x62, 0x63, 0x64,
    };
    uint8_t alias_word[4] = {0};
    const uint32_t commands[] = {
        GNNE_ADDI(2, 0, 0x100),
        GNNE_ADDI(4, 0, 1),
        GNNE_ADDI(5, 0, sizeof(source)),
        GNNE_MMU_CONF(0, 2, 0),
        GNNE_LW(3, 0, 8),
        GNNE_SS_PACK_SHAPE(4, 4, 4, 5, 0),
        GNNE_SS_PACK_STRIDE(5, 5, 5, 0),
        GNNE_SS_PACK_STRIDE(5, 5, 5, 1),
        GNNE_L2_STORE_CONF(1, 0, 0, 0),
        GNNE_L2_STORE(3, 2, 0),
        GNNE_LUI(6, 0x3c000),
        GNNE_ADDI(7, 0, 0x180),
        GNNE_L2_LOAD_CONF(1, 0, 0, 0),
        GNNE_L2_LOAD(7, 6, 0),
    };
    const size_t rdata_base_offset = KPU_RUNTIME_RDATA_BASE - KPU_RUNTIME_RDATA_PADDR;
    const size_t source_offset = rdata_base_offset + 0x100;
    const size_t alias_offset = rdata_base_offset + 8;
    const size_t output_offset = rdata_base_offset + 0x180;
    const size_t output_len = 8;

    write_u32_buf_le(alias_word, 0, 0xfc000000u);

    const struct kpu_runtime_section sections[] = {
        {
            .window = KPU_RUNTIME_WINDOW_RDATA,
            .offset = alias_offset,
            .kind = KPU_RUNTIME_SECTION_COPY,
            .data = alias_word,
            .len = sizeof(alias_word),
        },
        {
            .window = KPU_RUNTIME_WINDOW_RDATA,
            .offset = source_offset,
            .kind = KPU_RUNTIME_SECTION_COPY,
            .data = source,
            .len = sizeof(source),
        },
        {
            .window = KPU_RUNTIME_WINDOW_RDATA,
            .offset = rdata_base_offset,
            .kind = KPU_RUNTIME_SECTION_FILL,
            .fill = 0xa5,
            .len = output_len,
        },
        {
            .window = KPU_RUNTIME_WINDOW_RDATA,
            .offset = output_offset,
            .kind = KPU_RUNTIME_SECTION_FILL,
            .fill = 0xa5,
            .len = output_len,
        },
        {
            .window = KPU_RUNTIME_WINDOW_DDR,
            .offset = 0,
            .kind = KPU_RUNTIME_SECTION_FILL,
            .fill = 0,
            .len = sizeof(source),
        },
    };
    const struct kpu_runtime_check checks[] = {
        {
            .window = KPU_RUNTIME_WINDOW_DDR,
            .offset = 0,
            .expected = source,
            .expected_len = sizeof(source),
            .tail = 0,
            .total_len = sizeof(source),
            .what = "KPU runtime DDR mirror did not match source bytes",
        },
        {
            .window = KPU_RUNTIME_WINDOW_RDATA,
            .offset = output_offset,
            .expected = source,
            .expected_len = sizeof(source),
            .tail = 0xa5,
            .total_len = output_len,
            .what = "KPU runtime RDATA output did not match source bytes",
        },
    };
    const struct kpu_runtime_image image = {
        .name = "runtime_ddr_mirror",
        .command_paddr = KPU_RUNTIME_FUNCTION_COMMAND_PADDR,
        .commands = commands,
        .command_count = sizeof(commands) / sizeof(commands[0]),
        .sections = sections,
        .section_count = sizeof(sections) / sizeof(sections[0]),
        .checks = checks,
        .check_count = sizeof(checks) / sizeof(checks[0]),
    };
    return run_runtime_image(fd, &image, expect_irq);
}

int main(void)
{
    if (check_device_node(KPU_DEVICE_PATH) != 0 || check_device_node(KPU_DEVICE_ALIAS_PATH) != 0) {
        return 1;
    }

    int fd = open(KPU_DEVICE_PATH, O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        return fail_errno("open " KPU_DEVICE_PATH);
    }

    int irq_wait = 0;
    int failed = check_pread_reg(fd) != 0 || check_info_ioctl(fd, &irq_wait) != 0 ||
                 check_status_ioctl(fd) != 0 ||
                 check_program_command_ioctl(fd) != 0 || check_cfg_mmap(fd) != 0 ||
                 check_l2_mmap(fd) != 0;

    int has_yolov8n_capture = 0;
    if (!failed && has_optional_yolov8n_runtime_image(&has_yolov8n_capture) != 0) {
        failed = 1;
    }

    if (!failed && has_yolov8n_capture) {
        failed = check_optional_yolov8n_runtime_image(fd, irq_wait) != 0 ||
                 check_real_kmodel_asset(KPU_REAL_KMODEL_FILE_PATH) != 0;
    } else if (!failed) {
        failed = check_run_wait_done_ioctl(fd, irq_wait) != 0 ||
                 check_fake_output_mmap(fd, irq_wait) != 0 ||
                 check_runtime_ddr_mirror(fd, irq_wait) != 0 ||
                 check_runtime_arg_table_direct_io(fd, irq_wait) != 0 ||
                 check_runtime_image_file(fd, irq_wait, KPU_RUNTIME_IMAGE_FILE_PATH) != 0 ||
                 check_runtime_image_file(fd, irq_wait, KPU_RUNTIME_BLOB_IMAGE_FILE_PATH) != 0 ||
                 check_real_kmodel_asset(KPU_REAL_KMODEL_FILE_PATH) != 0;
    }
    close(fd);
    if (failed) {
        return 1;
    }

    printf("KPU_SMOKE_PASS\n");
    return 0;
}
