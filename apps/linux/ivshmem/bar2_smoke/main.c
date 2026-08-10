#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

#define PCI_DEVICES_DIR "/sys/bus/pci/devices"
#define UIO_CLASS_DIR "/sys/class/uio"
#define UIO_PCI_GENERIC_DRIVER "/sys/bus/pci/drivers/uio_pci_generic"
#define IVSHMEM_VENDOR "0x1af4"
#define IVSHMEM_DEVICE "0x1110"
#define IVSHMEM_DEFAULT_BDF "0000:00:05.0"
#define IVSHMEM_DEFAULT_DEVICE_PATH PCI_DEVICES_DIR "/" IVSHMEM_DEFAULT_BDF
#define BAR_SIZE 0x200000UL
#define PAYLOAD_SIZE 4096
#define MAGIC 0x41584232U
#define READY_SEQ 0x52454144U
#define A_TO_B_SEQ 1U
#define B_TO_A_SEQ 2U
#define DOORBELL_STATUS 1U
#define BAR0_INT_STATUS_WORD 1
#define BAR0_PEER_ID_WORD 2
#define BAR0_DOORBELL_WORD 3
#define TIMEOUT_NS (60ULL * 1000ULL * 1000ULL * 1000ULL)
#define UIO_WAIT_MS 2000

struct bar2_mailbox {
    volatile uint32_t magic;
    volatile uint32_t a_seq;
    volatile uint32_t b_seq;
    volatile uint32_t a_checksum;
    volatile uint32_t b_checksum;
    volatile uint8_t a_payload[PAYLOAD_SIZE];
    volatile uint8_t b_payload[PAYLOAD_SIZE];
};

struct uio_context {
    int fd;
    char devnode[128];
    int irq_control_supported;
};

static void raw_write_literal(const char *msg)
{
    size_t len = 0;
    while (msg[len] != '\0') {
        len++;
    }
    ssize_t ret = write(STDERR_FILENO, msg, len);
    (void)ret;
}

static void raw_write_hex(uintptr_t value)
{
    char buf[2 + sizeof(uintptr_t) * 2 + 1];
    const char hex[] = "0123456789abcdef";
    buf[0] = '0';
    buf[1] = 'x';
    for (size_t i = 0; i < sizeof(uintptr_t) * 2; i++) {
        unsigned int shift = (unsigned int)((sizeof(uintptr_t) * 2 - 1 - i) * 4);
        buf[2 + i] = hex[(value >> shift) & 0xfU];
    }
    buf[sizeof(buf) - 1] = '\0';
    raw_write_literal(buf);
}

static void fault_handler(int sig, siginfo_t *info, void *context)
{
    (void)context;
    raw_write_literal("ivshmem bar2 smoke fault signal=");
    raw_write_hex((uintptr_t)sig);
    raw_write_literal(" addr=");
    raw_write_hex((uintptr_t)info->si_addr);
    raw_write_literal(" code=");
    raw_write_hex((uintptr_t)info->si_code);
    raw_write_literal("\n");
    sync();
    _exit(128 + sig);
}

static void install_fault_handlers(void)
{
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_sigaction = fault_handler;
    action.sa_flags = SA_SIGINFO | SA_RESETHAND;
    sigemptyset(&action.sa_mask);
    sigaction(SIGSEGV, &action, NULL);
    sigaction(SIGBUS, &action, NULL);
    sigaction(SIGILL, &action, NULL);
}

static void checkpoint(const char *msg)
{
    printf("ivshmem checkpoint %s\n", msg);
}

static void die(const char *msg)
{
    fprintf(stderr, "ivshmem bar2 smoke failed: %s: %s\n", msg, strerror(errno));
    sync();
    _exit(1);
}

static void poweroff_after_success(void)
{
    sync();
    syscall(SYS_reboot, 0xfee1dead, 672274793, 0x4321fedc, NULL);
    for (;;) {
        pause();
    }
}

static uint64_t monotonic_ns(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        die("clock_gettime");
    }
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

static int remaining_timeout_ms(uint64_t start, int cap_ms)
{
    uint64_t elapsed = monotonic_ns() - start;
    if (elapsed >= TIMEOUT_NS) {
        return 0;
    }
    uint64_t remaining = TIMEOUT_NS - elapsed;
    uint64_t ms = remaining / (1000ULL * 1000ULL);
    if (cap_ms > 0 && ms > (uint64_t)cap_ms) {
        return cap_ms;
    }
    return ms > (uint64_t)INT32_MAX ? INT32_MAX : (int)ms;
}

static void wait_for(volatile uint32_t *field, uint32_t value, const char *what)
{
    uint64_t start = monotonic_ns();
    while (*field != value) {
        if (monotonic_ns() - start > TIMEOUT_NS) {
            fprintf(stderr, "ivshmem bar2 smoke failed: timeout waiting for %s\n", what);
            sync();
            _exit(1);
        }
        usleep(1000);
    }
    __sync_synchronize();
}

static void clear_status(volatile uint32_t *bar0)
{
    bar0[BAR0_INT_STATUS_WORD] = DOORBELL_STATUS;
    __sync_synchronize();
}

static int wait_uio_event(struct uio_context *uio, uint64_t start, const char *what)
{
    if (uio == NULL || uio->fd < 0) {
        return 0;
    }

    struct pollfd pfd = {
        .fd = uio->fd,
        .events = POLLIN,
    };
    while (1) {
        int timeout_ms = remaining_timeout_ms(start, UIO_WAIT_MS);
        if (timeout_ms == 0) {
            return 0;
        }
        int ret = poll(&pfd, 1, timeout_ms);
        if (ret < 0 && errno == EINTR) {
            continue;
        }
        if (ret < 0) {
            fprintf(stderr, "ivshmem bar2 smoke failed: poll %s: %s\n", what, strerror(errno));
            sync();
            _exit(1);
        }
        if (ret == 0) {
            return 0;
        }

        uint32_t count = 0;
        ssize_t n = read(uio->fd, &count, sizeof(count));
        if (n < 0 && errno == EINTR) {
            continue;
        }
        if (n != (ssize_t)sizeof(count)) {
            fprintf(stderr, "ivshmem bar2 smoke failed: read %s uio event: %s\n",
                    what, n < 0 ? strerror(errno) : "short read");
            sync();
            _exit(1);
        }
        printf("ivshmem uio event count=%u\n", count);
        if (uio->irq_control_supported) {
            uint32_t enable = 1;
            if (write(uio->fd, &enable, sizeof(enable)) != (ssize_t)sizeof(enable)) {
                if (errno == ENOSYS) {
                    uio->irq_control_supported = 0;
                    return 1;
                }
                fprintf(stderr, "ivshmem bar2 smoke failed: re-enable %s uio irq: %s\n",
                        what, strerror(errno));
                sync();
                _exit(1);
            }
        }
        return 1;
    }
}

static void wait_status(volatile uint32_t *bar0, struct uio_context *uio, const char *what)
{
    uint64_t start = monotonic_ns();
    if (uio != NULL && uio->fd >= 0 && wait_uio_event(uio, start, what)) {
        clear_status(bar0);
        return;
    }

    while ((bar0[BAR0_INT_STATUS_WORD] & DOORBELL_STATUS) == 0) {
        if (monotonic_ns() - start > TIMEOUT_NS) {
            fprintf(stderr, "ivshmem bar2 smoke failed: timeout waiting for %s\n", what);
            sync();
            _exit(1);
        }
        usleep(1000);
    }
    __sync_synchronize();
    printf("ivshmem wait fallback-poll what=%s\n", what);
    clear_status(bar0);
}

static void write_doorbell(volatile uint32_t *bar0, uint32_t target_peer, uint32_t vector)
{
    bar0[BAR0_DOORBELL_WORD] = (target_peer << 16) | (vector & 0xffffU);
    __sync_synchronize();
}

static void ensure_sysfs(void)
{
    mkdir("/sys", 0555);
    if (mount("sysfs", "/sys", "sysfs", 0, "") != 0 && errno != EBUSY) {
        die("mount sysfs");
    }
    mkdir("/proc", 0555);
    if (mount("proc", "/proc", "proc", 0, "") != 0 && errno != EBUSY) {
        printf("ivshmem procfs=unavailable (%s)\n", strerror(errno));
    }
    mkdir("/dev", 0755);
    if (mount("devtmpfs", "/dev", "devtmpfs", 0, "") != 0 && errno != EBUSY) {
        printf("ivshmem devtmpfs=unavailable (%s)\n", strerror(errno));
    }
}

static int read_trimmed_file(const char *path, char *buf, size_t len)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    ssize_t n = read(fd, buf, len - 1);
    close(fd);
    if (n < 0) {
        return -1;
    }
    buf[n] = '\0';
    char *newline = strchr(buf, '\n');
    if (newline != NULL) {
        *newline = '\0';
    }
    return 0;
}

#define SYSFS_PATH_MAX 384

static void make_sysfs_path(char *path, size_t path_len, const char *device_path,
                            const char *name)
{
    size_t device_len = strlen(device_path);
    size_t name_len = strlen(name);
    if (device_len + 1 + name_len + 1 > path_len) {
        fprintf(stderr, "ivshmem bar2 smoke failed: sysfs path too long\n");
        sync();
        _exit(1);
    }
    memcpy(path, device_path, device_len);
    path[device_len] = '/';
    memcpy(path + device_len + 1, name, name_len);
    path[device_len + 1 + name_len] = '\0';
}

static int write_text_file(const char *path, const char *value)
{
    int fd = open(path, O_WRONLY);
    if (fd < 0) {
        return -1;
    }
    size_t len = strlen(value);
    ssize_t n = write(fd, value, len);
    int saved_errno = errno;
    close(fd);
    if (n != (ssize_t)len) {
        errno = n < 0 ? saved_errno : EIO;
        return -1;
    }
    return 0;
}

static const char *path_basename(const char *path)
{
    const char *slash = strrchr(path, '/');
    return slash == NULL ? path : slash + 1;
}

static int sysfs_value_equals(const char *device_path, const char *name, const char *expected)
{
    char path[SYSFS_PATH_MAX];
    char value[32];
    make_sysfs_path(path, sizeof(path), device_path, name);
    return read_trimmed_file(path, value, sizeof(value)) == 0 && strcmp(value, expected) == 0;
}

static void find_ivshmem_device(char *device_path, size_t len)
{
    if (sysfs_value_equals(IVSHMEM_DEFAULT_DEVICE_PATH, "vendor", IVSHMEM_VENDOR)
        && sysfs_value_equals(IVSHMEM_DEFAULT_DEVICE_PATH, "device", IVSHMEM_DEVICE)) {
        snprintf(device_path, len, "%s", IVSHMEM_DEFAULT_DEVICE_PATH);
        printf("ivshmem-pci vendor=%s device=%s bdf=%s\n",
               IVSHMEM_VENDOR, IVSHMEM_DEVICE, IVSHMEM_DEFAULT_BDF);
        return;
    }

    fprintf(stderr, "ivshmem bar2 smoke failed: ivshmem-pci %s:%s not found\n",
            IVSHMEM_VENDOR, IVSHMEM_DEVICE);
    sync();
    _exit(1);
}

static void read_device_irq(const char *device_path, char *irq, size_t len)
{
    char path[SYSFS_PATH_MAX];
    make_sysfs_path(path, sizeof(path), device_path, "irq");
    if (read_trimmed_file(path, irq, len) == 0) {
        printf("ivshmem irq=%s\n", irq);
    } else {
        snprintf(irq, len, "unknown");
        printf("ivshmem irq=unknown\n");
    }
}

static void print_device_driver(const char *device_path)
{
    char path[SYSFS_PATH_MAX];
    char target[SYSFS_PATH_MAX];
    make_sysfs_path(path, sizeof(path), device_path, "driver");
    ssize_t len = readlink(path, target, sizeof(target) - 1);
    if (len < 0) {
        printf("ivshmem driver=none\n");
        return;
    }
    target[len] = '\0';
    char *driver = strrchr(target, '/');
    printf("ivshmem driver=%s\n", driver == NULL ? target : driver + 1);
}

static void print_irq_users(const char *label, const char *irq)
{
    FILE *file = fopen("/proc/interrupts", "r");
    if (file == NULL) {
        printf("ivshmem interrupts label=%s unavailable reason=%s\n", label, strerror(errno));
        return;
    }

    char line[256];
    size_t irq_len = strlen(irq);
    while (fgets(line, sizeof(line), file) != NULL) {
        char *cursor = line;
        while (*cursor == ' ') {
            cursor++;
        }
        if (strncmp(cursor, irq, irq_len) == 0 && cursor[irq_len] == ':') {
            char *newline = strchr(cursor, '\n');
            if (newline != NULL) {
                *newline = '\0';
            }
            printf("ivshmem interrupts label=%s %s\n", label, cursor);
            fclose(file);
            return;
        }
    }

    fclose(file);
    printf("ivshmem interrupts label=%s irq=%s not-found\n", label, irq);
}

static int try_unbind_current_driver(const char *device_path, const char *bdf)
{
    char driver_path[SYSFS_PATH_MAX];
    char unbind_path[SYSFS_PATH_MAX];
    char target[SYSFS_PATH_MAX];
    make_sysfs_path(driver_path, sizeof(driver_path), device_path, "driver");
    ssize_t len = readlink(driver_path, target, sizeof(target) - 1);
    if (len < 0) {
        return 0;
    }
    target[len] = '\0';
    const char *driver = path_basename(target);
    if (strcmp(driver, "uio_pci_generic") == 0) {
        return 0;
    }
    make_sysfs_path(unbind_path, sizeof(unbind_path), driver_path, "unbind");
    if (write_text_file(unbind_path, bdf) != 0) {
        printf("ivshmem uio bind=skip unbind-driver=%s reason=%s\n", driver, strerror(errno));
        return -1;
    }
    printf("ivshmem uio unbound-driver=%s\n", driver);
    return 0;
}

static int make_uio_devnode(const char *uio_name, char *devnode, size_t len)
{
    char sys_path[SYSFS_PATH_MAX];
    char value[32];
    snprintf(sys_path, sizeof(sys_path), "%s/%s/dev", UIO_CLASS_DIR, uio_name);
    if (read_trimmed_file(sys_path, value, sizeof(value)) != 0) {
        return -1;
    }

    unsigned int major = 0;
    unsigned int minor = 0;
    if (sscanf(value, "%u:%u", &major, &minor) != 2) {
        errno = EINVAL;
        return -1;
    }
    const char prefix[] = "/dev/";
    size_t prefix_len = sizeof(prefix) - 1;
    size_t name_len = strlen(uio_name);
    if (prefix_len + name_len + 1 > len) {
        errno = ENAMETOOLONG;
        return -1;
    }
    memcpy(devnode, prefix, prefix_len);
    memcpy(devnode + prefix_len, uio_name, name_len);
    devnode[prefix_len + name_len] = '\0';
    if (access(devnode, R_OK) == 0) {
        return 0;
    }
    if (mknod(devnode, S_IFCHR | 0600, makedev(major, minor)) != 0 && errno != EEXIST) {
        return -1;
    }
    return 0;
}

static int uio_device_matches_bdf(const char *uio_name, const char *bdf)
{
    char sys_path[SYSFS_PATH_MAX];
    char target[SYSFS_PATH_MAX];
    snprintf(sys_path, sizeof(sys_path), "%s/%s/device", UIO_CLASS_DIR, uio_name);
    ssize_t len = readlink(sys_path, target, sizeof(target) - 1);
    if (len < 0) {
        return 0;
    }
    target[len] = '\0';
    return strcmp(path_basename(target), bdf) == 0;
}

static int find_uio_devnode(const char *bdf, char *devnode, size_t len)
{
    char uio_name[16];
    for (unsigned int i = 0; i < 8; i++) {
        snprintf(uio_name, sizeof(uio_name), "uio%u", i);
        if (!uio_device_matches_bdf(uio_name, bdf)) {
            continue;
        }
        return make_uio_devnode(uio_name, devnode, len);
    }

    errno = ENOENT;
    return -1;
}

static int try_open_uio(const char *device_path, const char *bdf, struct uio_context *uio)
{
    uio->fd = -1;
    uio->devnode[0] = '\0';
    uio->irq_control_supported = 0;

    if (access(UIO_PCI_GENERIC_DRIVER, W_OK) != 0) {
        printf("ivshmem uio=unavailable driver=uio_pci_generic reason=%s\n", strerror(errno));
        return -1;
    }
    if (try_unbind_current_driver(device_path, bdf) != 0) {
        return -1;
    }

    char override_path[SYSFS_PATH_MAX];
    make_sysfs_path(override_path, sizeof(override_path), device_path, "driver_override");
    if (write_text_file(override_path, "uio_pci_generic") != 0) {
        printf("ivshmem uio bind=skip driver_override reason=%s\n", strerror(errno));
        return -1;
    }

    char bind_path[SYSFS_PATH_MAX];
    snprintf(bind_path, sizeof(bind_path), "%s/bind", UIO_PCI_GENERIC_DRIVER);
    if (write_text_file(bind_path, bdf) != 0 && errno != EBUSY) {
        printf("ivshmem uio bind=skip bind reason=%s\n", strerror(errno));
        return -1;
    }
    if (find_uio_devnode(bdf, uio->devnode, sizeof(uio->devnode)) != 0) {
        printf("ivshmem uio bind=skip find-devnode reason=%s\n", strerror(errno));
        return -1;
    }

    uio->fd = open(uio->devnode, O_RDWR);
    if (uio->fd < 0) {
        printf("ivshmem uio bind=skip open=%s reason=%s\n", uio->devnode, strerror(errno));
        return -1;
    }
    uint32_t enable = 1;
    if (write(uio->fd, &enable, sizeof(enable)) != (ssize_t)sizeof(enable)) {
        if (errno == ENOSYS) {
            printf("ivshmem uio irq-control=unavailable reason=%s\n", strerror(errno));
        } else {
            printf("ivshmem uio bind=skip enable-irq=%s reason=%s\n", uio->devnode, strerror(errno));
            close(uio->fd);
            uio->fd = -1;
            return -1;
        }
    } else {
        uio->irq_control_supported = 1;
    }
    printf("ivshmem uio device=%s\n", uio->devnode);
    return 0;
}

static void *map_resource(const char *device_path, const char *name, size_t size)
{
    char path[SYSFS_PATH_MAX];
    make_sysfs_path(path, sizeof(path), device_path, name);
    int fd = open(path, O_RDWR | O_SYNC);
    if (fd < 0) {
        die(path);
    }
    void *addr = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (addr == MAP_FAILED) {
        die(path);
    }
    printf("ivshmem map %s\n", name);
    return addr;
}

static uint32_t checksum(const volatile uint8_t *data, size_t len)
{
    uint32_t sum = 0x12345678U;
    for (size_t i = 0; i < len; i++) {
        sum = (sum << 5) | (sum >> 27);
        sum ^= data[i];
        sum += (uint32_t)i;
    }
    return sum;
}

static void fill_payload(volatile uint8_t *data, size_t len, uint32_t seed)
{
    uint32_t x = seed;
    for (size_t i = 0; i < len; i++) {
        x = x * 1664525U + 1013904223U;
        data[i] = (uint8_t)(x >> 24);
    }
}

static void clear_mailbox(struct bar2_mailbox *box)
{
    volatile uint8_t *bytes = (volatile uint8_t *)box;
    for (size_t i = 0; i < sizeof(*box); i++) {
        bytes[i] = 0;
    }
    __sync_synchronize();
}

static void peer0(volatile uint32_t *bar0, struct bar2_mailbox *box, struct uio_context *uio,
                  const char *irq)
{
    clear_mailbox(box);
    fill_payload(box->a_payload, PAYLOAD_SIZE, 0xa11c0000U);
    box->a_checksum = checksum(box->a_payload, PAYLOAD_SIZE);
    __sync_synchronize();
    box->magic = MAGIC;
    box->a_seq = A_TO_B_SEQ;
    __sync_synchronize();
    puts("VM A writes BAR2");
    write_doorbell(bar0, 1, 1);
    puts("VM A writes doorbell(target=B)");

    wait_status(bar0, uio, "VM B doorbell");
    print_irq_users("after-peer0-doorbell", irq);
    puts("VM A observes doorbell event");
    wait_for(&box->b_seq, B_TO_A_SEQ, "VM B response");
    if (checksum(box->b_payload, PAYLOAD_SIZE) != box->b_checksum) {
        fprintf(stderr, "ivshmem bar2 smoke failed: VM A checksum mismatch\n");
        _exit(1);
    }
    puts("VM A reads same data");
    puts("ivshmem bar2 shared memory pass");
    poweroff_after_success();
}

static void peer1(volatile uint32_t *bar0, struct bar2_mailbox *box, struct uio_context *uio,
                  const char *irq)
{
    uint64_t start = monotonic_ns();
    while (box->magic != MAGIC) {
        box->b_seq = READY_SEQ;
        __sync_synchronize();
        if (monotonic_ns() - start > TIMEOUT_NS) {
            fprintf(stderr, "ivshmem bar2 smoke failed: timeout waiting for BAR2 magic\n");
            sync();
            _exit(1);
        }
        usleep(1000);
    }
    __sync_synchronize();
    wait_for(&box->a_seq, A_TO_B_SEQ, "VM A payload");
    wait_status(bar0, uio, "VM A doorbell");
    print_irq_users("after-peer1-doorbell", irq);
    puts("VM B observes doorbell event");
    if (checksum(box->a_payload, PAYLOAD_SIZE) != box->a_checksum) {
        fprintf(stderr, "ivshmem bar2 smoke failed: VM B checksum mismatch\n");
        _exit(1);
    }
    puts("VM B reads same data");

    fill_payload(box->b_payload, PAYLOAD_SIZE, 0xb22d0001U);
    box->b_checksum = checksum(box->b_payload, PAYLOAD_SIZE);
    __sync_synchronize();
    puts("VM B writes BAR2");
    write_doorbell(bar0, 0, 2);
    puts("VM B writes doorbell(target=A)");
    box->b_seq = B_TO_A_SEQ;
    __sync_synchronize();
}

int main(void)
{
    raw_write_literal("ivshmem bar2 smoke init-start\n");
    install_fault_handlers();
    setvbuf(stdout, NULL, _IONBF, 0);
    setvbuf(stderr, NULL, _IONBF, 0);
    checkpoint("stdio-ready");
    ensure_sysfs();
    checkpoint("sysfs-ready");

    char device_path[SYSFS_PATH_MAX];
    find_ivshmem_device(device_path, sizeof(device_path));
    checkpoint("device-found");
    char irq[32];
    read_device_irq(device_path, irq, sizeof(irq));
    print_device_driver(device_path);

    volatile uint32_t *bar0 = map_resource(device_path, "resource0", 0x1000);
    checkpoint("bar0-mapped");
    struct bar2_mailbox *bar2 = map_resource(device_path, "resource2", BAR_SIZE);
    checkpoint("bar2-mapped");
    bar0[0] = DOORBELL_STATUS;
    __sync_synchronize();
    checkpoint("bar0-status-written");
    uint32_t peer_id = bar0[BAR0_PEER_ID_WORD];
    checkpoint("peer-id-read");
    struct uio_context uio;
    try_open_uio(device_path, path_basename(device_path), &uio);
    print_device_driver(device_path);
    print_irq_users("after-bind", irq);

    printf("ivshmem bar2 smoke peer_id=%u\n", peer_id);
    if (peer_id == 0) {
        peer0(bar0, bar2, &uio, irq);
    } else if (peer_id == 1) {
        peer1(bar0, bar2, &uio, irq);
    } else {
        fprintf(stderr, "ivshmem bar2 smoke failed: unexpected peer_id=%u\n", peer_id);
        return 1;
    }

    sync();
    for (;;) {
        pause();
    }
}
