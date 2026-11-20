/*
 * Sysfs discovery of AxVisor ivshmem PCI functions.
 *
 * Discovery reads the standard PCI sysfs attributes (vendor, device, class)
 * plus the revision byte from config space and verifies all four against the
 * frozen profile before handing out a device handle. The devices root is a
 * parameter so host-side tests can exercise the same code against a fixture
 * tree; ivshmem_find_device() always passes the real sysfs mount.
 */
#define _POSIX_C_SOURCE 200809L

#include "ivshmem_internal.h"

#include <dirent.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#define IVSHMEM_SYSFS_DEVICES "/sys/bus/pci/devices"

static int attr_hex_value(const char *dir, const char *name,
                          unsigned long *value)
{
    char path[IVSHMEM_PATH_MAX];
    char line[64];
    FILE *file;

    snprintf(path, sizeof(path), "%s/%s", dir, name);
    file = fopen(path, "r");
    if (file == NULL) {
        return IVSHMEM_ERR_IO;
    }
    if (fgets(line, sizeof(line), file) == NULL) {
        fclose(file);
        return IVSHMEM_ERR_IO;
    }
    fclose(file);

    errno = 0;
    *value = strtoul(line, NULL, 0);
    if (errno != 0) {
        return IVSHMEM_ERR_IO;
    }
    return IVSHMEM_OK;
}

static int config_revision(const char *dir, uint8_t *revision)
{
    char path[IVSHMEM_PATH_MAX];
    FILE *file;
    int byte;

    snprintf(path, sizeof(path), "%s/config", dir);
    file = fopen(path, "rb");
    if (file == NULL) {
        return IVSHMEM_ERR_IO;
    }
    if (fseek(file, IVSHMEM_CONFIG_REVISION_OFFSET, SEEK_SET) != 0) {
        fclose(file);
        return IVSHMEM_ERR_IO;
    }
    byte = fgetc(file);
    fclose(file);
    if (byte == EOF) {
        return IVSHMEM_ERR_IO;
    }
    *revision = (uint8_t)byte;
    return IVSHMEM_OK;
}

/**
 * Verifies one candidate directory against the full profile.
 *
 * @return IVSHMEM_OK when the device matches the profile,
 *         IVSHMEM_ERR_PROFILE when identity or revision differ, and
 *         IVSHMEM_ERR_IO when the attributes cannot be read.
 */
static int verify_profile(const char *dir)
{
    unsigned long vendor = 0;
    unsigned long device = 0;
    unsigned long class = 0;
    uint8_t revision = 0;
    int result;

    result = attr_hex_value(dir, "vendor", &vendor);
    if (result != IVSHMEM_OK) {
        return result;
    }
    result = attr_hex_value(dir, "device", &device);
    if (result != IVSHMEM_OK) {
        return result;
    }
    result = attr_hex_value(dir, "class", &class);
    if (result != IVSHMEM_OK) {
        return result;
    }
    result = config_revision(dir, &revision);
    if (result != IVSHMEM_OK) {
        return result;
    }

    if (vendor != IVSHMEM_VENDOR_ID || device != IVSHMEM_DEVICE_ID ||
        class != IVSHMEM_CLASS) {
        return IVSHMEM_ERR_PROFILE;
    }
    /* A revision mismatch must surface, not silently bind. */
    if (revision != IVSHMEM_REVISION) {
        return IVSHMEM_ERR_PROFILE;
    }
    return IVSHMEM_OK;
}

static int device_matches_identity(const char *dir, int *matches)
{
    unsigned long vendor = 0;
    unsigned long device = 0;
    int result;

    result = attr_hex_value(dir, "vendor", &vendor);
    if (result != IVSHMEM_OK) {
        return result;
    }
    result = attr_hex_value(dir, "device", &device);
    if (result != IVSHMEM_OK) {
        return result;
    }
    *matches = vendor == IVSHMEM_VENDOR_ID && device == IVSHMEM_DEVICE_ID;
    return IVSHMEM_OK;
}

static int open_device(const char *devices_root, const char *bdf,
                       struct ivshmem_device **out)
{
    struct stat directory;
    char path[IVSHMEM_PATH_MAX];
    struct ivshmem_device *dev;
    int result;

    snprintf(path, sizeof(path), "%s/%s", devices_root, bdf);
    /* A missing explicit BDF is an absent device, not an IO failure. */
    if (stat(path, &directory) != 0 || !S_ISDIR(directory.st_mode)) {
        return IVSHMEM_ERR_NOT_FOUND;
    }
    /* A failed profile check must be reported, never skipped: an adapter
     * bound to an unadapted revision would silently misbehave. */
    dev = calloc(1, sizeof(*dev));
    if (dev == NULL) {
        return IVSHMEM_ERR_NOMEM;
    }
    for (size_t bar = 0; bar < IVSHMEM_MAX_BARS; bar++) {
        dev->bars[bar].fd = -1;
    }
    snprintf(dev->bdf, sizeof(dev->bdf), "%s", bdf);
    snprintf(dev->sysfs_dir, sizeof(dev->sysfs_dir), "%s/%s", devices_root,
             bdf);

    result = verify_profile(dev->sysfs_dir);
    if (result != IVSHMEM_OK) {
        free(dev);
        return result;
    }
    *out = dev;
    return IVSHMEM_OK;
}

static int name_is_bdf(const char *name)
{
    /* PCI BDFs are "dddd:bb:dd.f"; reject dot-prefixed sysfs noise. */
    return name[0] != '.';
}

static int compare_names(const void *left, const void *right)
{
    const char *const *left_name = left;
    const char *const *right_name = right;

    return strcmp(*left_name, *right_name);
}

/**
 * Scans one devices root for profile-matching functions.
 *
 * Identity matches are collected first and verified as a whole so the scan
 * is deterministic (sorted by BDF): AMBIGUOUS and PROFILE outcomes never
 * depend on readdir order.
 */
static int scan_devices(const char *devices_root, struct ivshmem_device **out)
{
    DIR *dir;
    struct dirent *entry;
    char **names = NULL;
    size_t count = 0;
    size_t capacity = 0;
    size_t index;
    size_t verified_matches = 0;
    char verified_bdf[16] = { 0 };
    int result = IVSHMEM_ERR_NOT_FOUND;

    dir = opendir(devices_root);
    if (dir == NULL) {
        return IVSHMEM_ERR_IO;
    }
    while ((entry = readdir(dir)) != NULL) {
        char **resized;
        char *name;

        if (!name_is_bdf(entry->d_name)) {
            continue;
        }
        if (count == capacity) {
            capacity = capacity == 0 ? 8 : capacity * 2;
            resized = realloc(names, capacity * sizeof(*names));
            if (resized == NULL) {
                free(names);
                closedir(dir);
                return IVSHMEM_ERR_NOMEM;
            }
            names = resized;
        }
        name = strdup(entry->d_name);
        if (name == NULL) {
            free(names);
            closedir(dir);
            return IVSHMEM_ERR_NOMEM;
        }
        names[count++] = name;
    }
    closedir(dir);

    /* Sorted order makes the AMBIGUOUS and PROFILE outcomes independent of
     * readdir order. The handle is opened only once the scan proves the
     * match is unique, so ambiguous results never leak a device. */
    qsort(names, count, sizeof(*names), compare_names);
    for (index = 0; index < count; index++) {
        char path[IVSHMEM_PATH_MAX];
        int identity_matches = 0;

        snprintf(path, sizeof(path), "%s/%s", devices_root, names[index]);
        result = device_matches_identity(path, &identity_matches);
        if (result != IVSHMEM_OK) {
            break;
        }
        if (!identity_matches) {
            continue;
        }
        /* Verify the full profile now so a revision mismatch surfaces even
         * when another matching device follows. */
        result = verify_profile(path);
        if (result != IVSHMEM_OK) {
            break;
        }
        if (verified_matches == 1) {
            result = IVSHMEM_ERR_AMBIGUOUS;
            break;
        }
        verified_matches = 1;
        snprintf(verified_bdf, sizeof(verified_bdf), "%s", names[index]);
    }
    if (result == IVSHMEM_OK && verified_matches == 1) {
        result = open_device(devices_root, verified_bdf, out);
    } else if (result == IVSHMEM_OK) {
        result = IVSHMEM_ERR_NOT_FOUND;
    }
    for (index = 0; index < count; index++) {
        free(names[index]);
    }
    free(names);
    return result;
}

int ivshmem_find_at(const char *devices_root, const char *bdf,
                    struct ivshmem_device **out)
{
    if (devices_root == NULL || out == NULL) {
        return IVSHMEM_ERR_ARGS;
    }
    *out = NULL;
    if (bdf == NULL) {
        return scan_devices(devices_root, out);
    }
    return open_device(devices_root, bdf, out);
}

int ivshmem_find_device(const char *bdf, struct ivshmem_device **out)
{
    return ivshmem_find_at(IVSHMEM_SYSFS_DEVICES, bdf, out);
}

void ivshmem_device_close(struct ivshmem_device *dev)
{
    size_t bar;

    if (dev == NULL) {
        return;
    }
    for (bar = 0; bar < IVSHMEM_MAX_BARS; bar++) {
        if (dev->bars[bar].map != NULL) {
            munmap(dev->bars[bar].map, dev->bars[bar].size);
        }
        if (dev->bars[bar].fd >= 0) {
            close(dev->bars[bar].fd);
        }
    }
    free(dev);
}
