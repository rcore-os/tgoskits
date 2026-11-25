/*
 * Host-side unit tests for the ivshmem adapter.
 *
 * The tests run the real discovery, mapping, and backend code against a
 * synthetic sysfs tree, so profile matching, BAR-size parsing, and backend
 * explicitness are verified without a guest. They are host-side tests and are
 * not currently registered with xtask or CI; run them manually from the
 * repository root:
 *
 *   cc -std=c11 -Wall -Wextra -Werror -Iapps/linux/ivshmem/lib \
 *     apps/linux/ivshmem/lib/tests/adapter_test.c \
 *     apps/linux/ivshmem/lib/discovery.c \
 *     apps/linux/ivshmem/lib/backend_polling.c \
 *     apps/linux/ivshmem/lib/errors.c -o /tmp/ivshmem-adapter-test &&
 *   /tmp/ivshmem-adapter-test
 */
#define _POSIX_C_SOURCE 200809L

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#include "ivshmem.h"
#include "ivshmem_internal.h"

static int checks_run;
static int checks_failed;

#define CHECK(condition)                                                     \
    do {                                                                     \
        checks_run++;                                                        \
        if (!(condition)) {                                                  \
            checks_failed++;                                                 \
            fprintf(stderr, "CHECK failed at %s:%d: %s\n", __FILE__,        \
                    __LINE__, #condition);                                   \
        }                                                                    \
    } while (0)

#define CHECK_EQ(actual, expected)                                           \
    do {                                                                     \
        long long actual_value = (long long)(actual);                        \
        long long expected_value = (long long)(expected);                    \
        checks_run++;                                                        \
        if (actual_value != expected_value) {                                \
            checks_failed++;                                                 \
            fprintf(stderr, "CHECK failed at %s:%d: %s == %s (%lld != %lld)\n", \
                    __FILE__, __LINE__, #actual, #expected, actual_value,   \
                    expected_value);                                         \
        }                                                                    \
    } while (0)

static void write_file(const char *path, const char *contents)
{
    FILE *file = fopen(path, "w");

    if (file == NULL) {
        perror(path);
        exit(2);
    }
    fputs(contents, file);
    fclose(file);
}

static void put_le16(uint8_t *bytes, size_t offset, uint16_t value)
{
    bytes[offset] = (uint8_t)value;
    bytes[offset + 1] = (uint8_t)(value >> 8);
}

static void put_le32(uint8_t *bytes, size_t offset, uint32_t value)
{
    bytes[offset] = (uint8_t)value;
    bytes[offset + 1] = (uint8_t)(value >> 8);
    bytes[offset + 2] = (uint8_t)(value >> 16);
    bytes[offset + 3] = (uint8_t)(value >> 24);
}

/* Builds a Type-0 config image with MSI-X first and the AxVisor transport
 * capability second. The VID:DID is intentionally the QEMU alias, so these
 * fixtures also prove that discovery requires the full AxVisor signature. */
static void write_config(const char *dir, uint8_t revision)
{
    char path[512];
    FILE *file;
    uint8_t config[256] = { 0 };
    const size_t transport = 0x4c;
    const size_t body = transport + IVSHMEM_CAPABILITY_HEADER_SIZE;

    config[8] = revision;
    config[IVSHMEM_CONFIG_STATUS_OFFSET] |=
        (uint8_t)IVSHMEM_CONFIG_STATUS_CAPABILITY_LIST;
    config[IVSHMEM_CONFIG_CAPABILITY_POINTER_OFFSET] = 0x40;
    config[0x40] = 0x11;
    config[0x41] = (uint8_t)transport;
    config[transport] = IVSHMEM_CAP_ID_VENDOR;
    config[body] = IVSHMEM_TRANSPORT_CAP_LENGTH;
    config[body + 1] = IVSHMEM_TRANSPORT_CAP_VERSION;
    put_le32(config, body + 2, IVSHMEM_TRANSPORT_CAP_MAGIC);
    put_le16(config, body + 6, IVSHMEM_TRANSPORT_PROTOCOL);
    put_le32(config, body + 10, IVSHMEM_TRANSPORT_FEATURE_PEER_EPOCH);
    put_le32(config, body + 14, IVSHMEM_STATE_EPOCH_OFFSET);
    put_le32(config, body + 18, IVSHMEM_STATE_EPOCH_STRIDE);
    put_le32(config, body + 22, 0);
    put_le32(config, body + 26, 0x7000);
    put_le32(config, body + 30, 2);
    snprintf(path, sizeof(path), "%s/config", dir);
    file = fopen(path, "wb");
    if (file == NULL) {
        perror(path);
        exit(2);
    }
    fwrite(config, 1, sizeof(config), file);
    fclose(file);
}

static void make_device(const char *root, const char *bdf, uint8_t revision,
                        uint16_t vendor, uint16_t device, unsigned long class)
{
    char dir[256];

    snprintf(dir, sizeof(dir), "%s/%s", root, bdf);
    if (mkdir(dir, 0755) != 0 && errno != EEXIST) {
        perror(dir);
        exit(2);
    }
    char path[512];
    char value[32];
    snprintf(path, sizeof(path), "%s/vendor", dir);
    snprintf(value, sizeof(value), "0x%04x\n", vendor);
    write_file(path, value);
    snprintf(path, sizeof(path), "%s/device", dir);
    snprintf(value, sizeof(value), "0x%04x\n", device);
    write_file(path, value);
    snprintf(path, sizeof(path), "%s/class", dir);
    snprintf(value, sizeof(value), "0x%06lx\n", class);
    write_file(path, value);
    write_config(dir, revision);
}

/* Writes the six-line sysfs resource table: start end flags per BAR. */
static void write_resource(const char *dir, const unsigned long bars[6][3])
{
    char path[512];
    FILE *file;

    snprintf(path, sizeof(path), "%s/resource", dir);
    file = fopen(path, "w");
    if (file == NULL) {
        perror(path);
        exit(2);
    }
    for (int bar = 0; bar < 6; bar++) {
        fprintf(file, "0x%016lx 0x%016lx 0x%016lx\n", bars[bar][0],
                bars[bar][1], bars[bar][2]);
    }
    fclose(file);
}

/* Creates one resource backing file large enough to mmap at the BAR size. */
static void make_resource_file(const char *dir, const char *name,
                               unsigned long size)
{
    char path[512];
    FILE *file;

    snprintf(path, sizeof(path), "%s/%s", dir, name);
    file = fopen(path, "w");
    if (file == NULL) {
        perror(path);
        exit(2);
    }
    if (fseek(file, (long)size - 1, SEEK_SET) != 0 ||
        fputc(0, file) == EOF) {
        fprintf(stderr, "cannot size %s\n", path);
        exit(2);
    }
    fclose(file);
}

static void make_full_ivshmem(const char *root, const char *bdf)
{
    char dir[256];

    make_device(root, bdf, IVSHMEM_REVISION, IVSHMEM_VENDOR_ID,
                IVSHMEM_DEVICE_ID, IVSHMEM_CLASS);
    snprintf(dir, sizeof(dir), "%s/%s", root, bdf);
    const unsigned long bars[6][3] = {
        { 0x0c000000, 0x0c000fff, 0x200 }, /* BAR0: 4 KiB register page */
        { 0, 0, 0 },
        { 0x0c100000, 0x0c10ffff, 0x2200 }, /* BAR2: prefetchable shared RAM */
        { 0, 0, 0 },
        { 0, 0, 0 },
        { 0, 0, 0 },
    };
    write_resource(dir, bars);
    make_resource_file(dir, "resource0", 0x1000);
    make_resource_file(dir, "resource2_wc", 0x10000);
}

static void make_root(const char *name, char *out, size_t out_size)
{
    snprintf(out, out_size, "/tmp/ivshmem-test-%s-XXXXXX", name);
    if (mkdtemp(out) == NULL) {
        perror("mkdtemp");
        exit(2);
    }
}

static void test_single_device_is_found(char *root)
{
    struct ivshmem_device *dev = NULL;

    make_full_ivshmem(root, "0000:00:01.0");
    CHECK_EQ(ivshmem_find_at(root, NULL, &dev), IVSHMEM_OK);
    CHECK(dev != NULL);
    if (dev != NULL) {
        int original_stdin = dup(STDIN_FILENO);
        int input = open("/dev/null", O_RDONLY);

        CHECK_EQ(strcmp(dev->bdf, "0000:00:01.0"), 0);
        CHECK(input >= 0);
        if (input >= 0) {
            CHECK_EQ(dup2(input, STDIN_FILENO), STDIN_FILENO);
            if (input != STDIN_FILENO) {
                close(input);
            }
        }
        ivshmem_device_close(dev);
        /* Closing an unmapped handle must not close unrelated fd 0. */
        CHECK(fcntl(STDIN_FILENO, F_GETFD) >= 0);
        if (original_stdin >= 0) {
            dup2(original_stdin, STDIN_FILENO);
            close(original_stdin);
        } else {
            close(STDIN_FILENO);
        }
    }
}

static void test_missing_device_reports_not_found(char *root)
{
    struct ivshmem_device *dev = (struct ivshmem_device *)1;

    CHECK_EQ(ivshmem_find_at(root, NULL, &dev), IVSHMEM_ERR_NOT_FOUND);
    CHECK(dev == NULL);
}

static void test_two_devices_require_an_explicit_bdf(char *root)
{
    struct ivshmem_device *dev = NULL;

    make_full_ivshmem(root, "0000:00:01.0");
    make_full_ivshmem(root, "0000:00:02.0");
    CHECK_EQ(ivshmem_find_at(root, NULL, &dev), IVSHMEM_ERR_AMBIGUOUS);
    CHECK(dev == NULL);

    /* The explicit BDF disambiguates. */
    CHECK_EQ(ivshmem_find_at(root, "0000:00:02.0", &dev), IVSHMEM_OK);
    CHECK(dev != NULL);
    if (dev != NULL) {
        CHECK_EQ(strcmp(dev->bdf, "0000:00:02.0"), 0);
        ivshmem_device_close(dev);
    }
}

static void test_revision_mismatch_fails_the_profile(char *root)
{
    struct ivshmem_device *dev = NULL;

    /* Identity matches but the revision byte belongs to another profile. */
    make_device(root, "0000:00:03.0", 0x02, IVSHMEM_VENDOR_ID,
                IVSHMEM_DEVICE_ID, IVSHMEM_CLASS);
    CHECK_EQ(ivshmem_find_at(root, NULL, &dev), IVSHMEM_ERR_PROFILE);
    CHECK_EQ(ivshmem_find_at(root, "0000:00:03.0", &dev), IVSHMEM_ERR_PROFILE);
    CHECK(dev == NULL);
}

static void test_missing_axvisor_capability_fails_the_profile(char *root)
{
    struct ivshmem_device *dev = NULL;
    char path[512];
    FILE *file;
    uint8_t next = 0;

    make_full_ivshmem(root, "0000:00:03.0");
    snprintf(path, sizeof(path), "%s/0000:00:03.0/config", root);
    file = fopen(path, "r+b");
    if (file == NULL) {
        perror(path);
        exit(2);
    }
    /* Unlink the second capability while leaving VID:DID, class, revision,
     * and the first capability intact. */
    if (fseek(file, 0x41, SEEK_SET) != 0 ||
        fwrite(&next, 1, sizeof(next), file) != sizeof(next)) {
        fprintf(stderr, "cannot edit %s\n", path);
        exit(2);
    }
    fclose(file);

    CHECK_EQ(ivshmem_find_at(root, NULL, &dev), IVSHMEM_ERR_PROFILE);
    CHECK_EQ(ivshmem_find_at(root, "0000:00:03.0", &dev),
             IVSHMEM_ERR_PROFILE);
    CHECK(dev == NULL);
}

static void test_wrong_identity_is_not_a_match(char *root)
{
    struct ivshmem_device *dev = NULL;

    /* Another vendor's function is skipped by auto-detect, and an explicit
     * BDF fails the profile check instead of binding. */
    make_device(root, "0000:00:04.0", IVSHMEM_REVISION, 0x8086, 0x1234,
                0x060000);
    CHECK_EQ(ivshmem_find_at(root, NULL, &dev), IVSHMEM_ERR_NOT_FOUND);
    CHECK_EQ(ivshmem_find_at(root, "0000:00:04.0", &dev), IVSHMEM_ERR_PROFILE);
    CHECK(dev == NULL);
}

static void test_missing_explicit_bdf_reports_not_found(char *root)
{
    struct ivshmem_device *dev = NULL;

    make_full_ivshmem(root, "0000:00:01.0");
    CHECK_EQ(ivshmem_find_at(root, "0000:00:09.0", &dev),
             IVSHMEM_ERR_NOT_FOUND);
    CHECK(dev == NULL);
}

static void test_invalid_arguments_are_rejected(void)
{
    struct ivshmem_device *dev = NULL;

    CHECK_EQ(ivshmem_find_at(NULL, NULL, &dev), IVSHMEM_ERR_ARGS);
    CHECK_EQ(ivshmem_find_at("/tmp", NULL, NULL), IVSHMEM_ERR_ARGS);
}

static void test_enable_sets_memory_decoding(char *root)
{
    struct ivshmem_device *dev = NULL;
    char path[512];
    FILE *file;
    uint8_t command[2] = { 0, 0 };

    make_full_ivshmem(root, "0000:00:01.0");
    CHECK_EQ(ivshmem_find_at(root, "0000:00:01.0", &dev), IVSHMEM_OK);
    CHECK_EQ(ivshmem_enable_device(NULL), IVSHMEM_ERR_ARGS);
    CHECK_EQ(ivshmem_enable_device(dev), IVSHMEM_OK);

    /* The Command register in config space must now carry the Memory Space
     * Enable bit (the fixture file records the raw write). */
    snprintf(path, sizeof(path), "%s/0000:00:01.0/config", root);
    file = fopen(path, "rb");
    if (file == NULL) {
        perror(path);
        exit(2);
    }
    if (fseek(file, IVSHMEM_CONFIG_COMMAND_OFFSET, SEEK_SET) != 0 ||
        fread(command, 1, sizeof(command), file) != sizeof(command)) {
        fprintf(stderr, "cannot read back %s\n", path);
        exit(2);
    }
    fclose(file);
    CHECK_EQ(command[0] & IVSHMEM_COMMAND_MEMORY_ENABLE,
             IVSHMEM_COMMAND_MEMORY_ENABLE);
    ivshmem_device_close(dev);
}

static void test_bar_mapping_and_register_access(char *root)
{
    struct ivshmem_device *dev = NULL;
    void *registers = NULL;
    void *shared = NULL;
    void *again = NULL;
    size_t register_size = 0;
    size_t shared_size = 0;
    uint64_t epoch = 0;
    const uint64_t guest_epoch = 7;

    make_full_ivshmem(root, "0000:00:01.0");
    CHECK_EQ(ivshmem_find_at(root, "0000:00:01.0", &dev), IVSHMEM_OK);
    CHECK_EQ(ivshmem_observe_peer_epoch(dev, 0, &epoch),
             IVSHMEM_ERR_BACKEND);

    /* Linux owns the MSI-X table and PBA; userspace must not map BAR1. */
    CHECK_EQ(ivshmem_map_bar(dev, IVSHMEM_BAR_MSIX, &registers,
                             &register_size),
             IVSHMEM_ERR_BACKEND);
    CHECK_EQ(ivshmem_map_bar(dev, 5, &registers, &register_size),
             IVSHMEM_ERR_ARGS);
    CHECK_EQ(ivshmem_map_bar(NULL, IVSHMEM_BAR_REGISTERS, &registers,
                             &register_size),
             IVSHMEM_ERR_ARGS);

    CHECK_EQ(ivshmem_map_bar(dev, IVSHMEM_BAR_REGISTERS, &registers,
                             &register_size),
             IVSHMEM_OK);
    CHECK_EQ(register_size, 0x1000);
    CHECK_EQ(ivshmem_map_bar(dev, IVSHMEM_BAR_SHARED, &shared, &shared_size),
             IVSHMEM_OK);
    CHECK_EQ(shared_size, 0x10000);

    /* Remapping returns the same window without a second mmap. */
    CHECK_EQ(ivshmem_map_bar(dev, IVSHMEM_BAR_REGISTERS, &again,
                             &register_size),
             IVSHMEM_OK);
    CHECK(again == registers);

    /* Register access is 32-bit, page-bounded, and rejects bad offsets. */
    CHECK_EQ(ivshmem_read_reg32(dev, 0x1234), 0);
    ivshmem_write_reg32(dev, 0x2, 1); /* unaligned: ignored */
    ivshmem_write_reg32(dev, IVSHMEM_REG_STATE, 0x12345678);
    CHECK_EQ(ivshmem_read_reg32(dev, IVSHMEM_REG_STATE), 0x12345678);
    CHECK_EQ(ivshmem_read_reg32(NULL, IVSHMEM_REG_STATE), 0);

    /* The shared-memory helper returns the write-combining mapping. */
    CHECK(ivshmem_shared_memory(dev, &shared_size) == shared);
    CHECK_EQ(shared_size, 0x10000);
    CHECK(ivshmem_shared_memory(NULL, &shared_size) == NULL);
    memcpy((char *)shared + IVSHMEM_STATE_EPOCH_OFFSET, &guest_epoch,
           sizeof(guest_epoch));
    CHECK_EQ(ivshmem_observe_peer_epoch(dev, 0, &epoch), IVSHMEM_OK);
    CHECK_EQ(epoch, guest_epoch);
    CHECK_EQ(ivshmem_observe_peer_epoch(dev, IVSHMEM_MAX_PROFILE_PEERS,
                                        &epoch),
             IVSHMEM_ERR_ARGS);
    CHECK_EQ(ivshmem_observe_peer_epoch(dev, 0, NULL), IVSHMEM_ERR_ARGS);

    ivshmem_device_close(dev);
}

static void test_shared_mapping_requires_write_combining_file(char *root)
{
    struct ivshmem_device *dev = NULL;
    void *shared = NULL;
    size_t shared_size = 0;
    char device_dir[256];
    char path[512];

    make_full_ivshmem(root, "0000:00:01.0");
    snprintf(device_dir, sizeof(device_dir), "%s/0000:00:01.0", root);
    make_resource_file(device_dir, "resource2", 0x10000);
    snprintf(path, sizeof(path), "%s/resource2_wc", device_dir);
    CHECK_EQ(unlink(path), 0);

    CHECK_EQ(ivshmem_find_at(root, "0000:00:01.0", &dev), IVSHMEM_OK);
    CHECK_EQ(ivshmem_map_bar(dev, IVSHMEM_BAR_SHARED, &shared, &shared_size),
             IVSHMEM_ERR_MMAP);
    CHECK(shared == NULL);
    ivshmem_device_close(dev);
}

static void test_backends_are_explicit(char *root)
{
    struct ivshmem_device *dev = NULL;
    struct ivshmem_backend *backend = NULL;
    void *registers = NULL;
    size_t register_size = 0;
    volatile uint32_t *event_status;

    make_full_ivshmem(root, "0000:00:01.0");
    CHECK_EQ(ivshmem_find_at(root, "0000:00:01.0", &dev), IVSHMEM_OK);
    CHECK_EQ(ivshmem_map_bar(dev, IVSHMEM_BAR_REGISTERS, &registers,
                             &register_size),
             IVSHMEM_OK);
    event_status =
        (volatile uint32_t *)((char *)registers + IVSHMEM_REG_EVENT_STATUS);

    /* Without a matching UIO binding, interrupt mode refuses explicitly;
     * there is no silent polling fallback. */
    CHECK_EQ(ivshmem_backend_open(dev, IVSHMEM_BACKEND_INTERRUPT, &backend),
             IVSHMEM_ERR_BACKEND);
    CHECK(backend == NULL);
    CHECK_EQ(ivshmem_backend_open(NULL, IVSHMEM_BACKEND_POLLING, &backend),
             IVSHMEM_ERR_ARGS);

    /* A clear status times out. */
    CHECK_EQ(ivshmem_backend_open(dev, IVSHMEM_BACKEND_POLLING, &backend),
             IVSHMEM_OK);
    *event_status = 0;
    CHECK_EQ(ivshmem_backend_wait_event(backend, 30), 0);

    /* A pending status returns once, and the backend issues the W1C clear
     * write. The fixture file records the raw write value because it has no
     * write-one-to-clear hardware logic; the actual bit clear is covered by
     * the shared QEMU case. */
    *event_status = 0;
    CHECK_EQ(ivshmem_backend_wait_event(backend, 30), 0);
    *event_status = 1;
    CHECK_EQ(ivshmem_backend_wait_event(backend, 100), 1);
    CHECK_EQ(*event_status, 1);

    CHECK_EQ(ivshmem_backend_wait_event(NULL, 100), IVSHMEM_ERR_ARGS);
    ivshmem_backend_close(backend);
    ivshmem_device_close(dev);
}

static void test_interrupt_backend_discovers_uio_and_rearms(char *root)
{
    struct ivshmem_device *dev = NULL;
    struct ivshmem_backend *backend = NULL;
    void *registers = NULL;
    size_t register_size = 0;
    char device_dir[512];
    char class_root[128];
    char class_uio[256];
    char class_device[512];
    char device_root[256];
    char uio_node[512];
    uint32_t event_count = 7;
    uint32_t rearm = 0;
    int writer;
    int observer;

    make_full_ivshmem(root, "0000:00:01.0");
    snprintf(device_dir, sizeof(device_dir), "%s/0000:00:01.0", root);
    snprintf(class_root, sizeof(class_root), "%s/uio-class", root);
    snprintf(class_uio, sizeof(class_uio), "%s/uio0", class_root);
    snprintf(class_device, sizeof(class_device), "%s/device", class_uio);
    snprintf(device_root, sizeof(device_root), "%s/dev", root);
    snprintf(uio_node, sizeof(uio_node), "%s/uio0", device_root);
    CHECK_EQ(mkdir(class_root, 0755), 0);
    CHECK_EQ(mkdir(class_uio, 0755), 0);
    CHECK_EQ(mkdir(device_root, 0755), 0);
    CHECK_EQ(symlink(device_dir, class_device), 0);
    CHECK_EQ(mkfifo(uio_node, 0600), 0);

    CHECK_EQ(ivshmem_find_at(root, "0000:00:01.0", &dev), IVSHMEM_OK);
    CHECK_EQ(ivshmem_map_bar(dev, IVSHMEM_BAR_REGISTERS, &registers,
                             &register_size),
             IVSHMEM_OK);
    CHECK_EQ(ivshmem_backend_open_at(dev, IVSHMEM_BACKEND_INTERRUPT,
                                     class_root, device_root, &backend),
             IVSHMEM_OK);
    CHECK(backend != NULL);

    writer = open(uio_node, O_WRONLY | O_NONBLOCK);
    CHECK(writer >= 0);
    if (writer >= 0) {
        CHECK_EQ(write(writer, &event_count, sizeof(event_count)),
                 (ssize_t)sizeof(event_count));
        CHECK_EQ(ivshmem_backend_wait_event(backend, 100), 1);
        close(writer);
    }
    observer = open(uio_node, O_RDONLY | O_NONBLOCK);
    CHECK(observer >= 0);
    if (observer >= 0) {
        CHECK_EQ(read(observer, &rearm, sizeof(rearm)),
                 (ssize_t)sizeof(rearm));
        CHECK_EQ(rearm, 1);
        close(observer);
    }
    CHECK_EQ(ivshmem_backend_wait_event(backend, 10), 0);

    ivshmem_backend_close(backend);
    ivshmem_device_close(dev);
}

static void test_strerror_covers_every_code(void)
{
    int code;

    for (code = IVSHMEM_OK; code >= IVSHMEM_ERR_ARGS; code--) {
        const char *text = ivshmem_strerror(code);

        CHECK(text != NULL);
        CHECK(strlen(text) > 0);
    }
    CHECK_EQ(strcmp(ivshmem_strerror(42), "unknown ivshmem error"), 0);
}

int main(void)
{
    char root[64];

    make_root("single", root, sizeof(root));
    test_single_device_is_found(root);

    make_root("missing", root, sizeof(root));
    test_missing_device_reports_not_found(root);

    make_root("ambiguous", root, sizeof(root));
    test_two_devices_require_an_explicit_bdf(root);

    make_root("revision", root, sizeof(root));
    test_revision_mismatch_fails_the_profile(root);

    make_root("capability", root, sizeof(root));
    test_missing_axvisor_capability_fails_the_profile(root);

    make_root("identity", root, sizeof(root));
    test_wrong_identity_is_not_a_match(root);

    make_root("explicit", root, sizeof(root));
    test_missing_explicit_bdf_reports_not_found(root);

    test_invalid_arguments_are_rejected();

    make_root("enable", root, sizeof(root));
    test_enable_sets_memory_decoding(root);

    make_root("mapping", root, sizeof(root));
    test_bar_mapping_and_register_access(root);

    make_root("mapping-wc", root, sizeof(root));
    test_shared_mapping_requires_write_combining_file(root);

    make_root("backend", root, sizeof(root));
    test_backends_are_explicit(root);

    make_root("interrupt", root, sizeof(root));
    test_interrupt_backend_discovers_uio_and_rearms(root);

    test_strerror_covers_every_code();

    printf("adapter tests: %d checks, %d failures\n", checks_run,
           checks_failed);
    return checks_failed == 0 ? 0 : 1;
}
