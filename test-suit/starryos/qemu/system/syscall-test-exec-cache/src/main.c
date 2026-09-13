#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#if defined(__aarch64__)
static const uint32_t code42[] = {0x52800540, 0xd65f03c0};
static const uint32_t code99[] = {0x52800c60, 0xd65f03c0};
#elif defined(__riscv)
static const uint32_t code42[] = {0x02a00513, 0x00008067};
static const uint32_t code99[] = {0x06300513, 0x00008067};
#elif defined(__x86_64__)
static const unsigned char code42[] = {0xb8, 42, 0, 0, 0, 0xc3};
static const unsigned char code99[] = {0xb8, 99, 0, 0, 0, 0xc3};
#elif defined(__loongarch__)
static const uint32_t code42[] = {0x0380a804, 0x4c000020};
static const uint32_t code99[] = {0x03818c04, 0x4c000020};
#else
#error unsupported architecture
#endif

static int run(void)
{
    long page_size = sysconf(_SC_PAGESIZE);
    int fd = memfd_create("executable-refault", MFD_CLOEXEC);
    if (page_size <= 0 || fd < 0 || ftruncate(fd, page_size) ||
        pwrite(fd, code42, sizeof(code42), 0) != sizeof(code42))
        return 1;
    void *text = mmap(NULL, page_size, PROT_READ | PROT_EXEC, MAP_SHARED, fd, 0);
    if (text == MAP_FAILED)
        return 1;
    /* Establish an old instruction-cache image, independent of kernel sync. */
    volatile unsigned char byte = *(volatile unsigned char *)text;
    (void)byte;
    __builtin___clear_cache(text, (char *)text + sizeof(code42));
    if (((int (*)(void))text)() != 42 || munmap(text, page_size))
        return 1;
    /* The kernel writes the file page. This is not userspace self-modifying
       code: a new executable mapping must observe the completed file write. */
    if (pwrite(fd, code99, sizeof(code99), 0) != sizeof(code99))
        return 1;
    unsigned char observed[sizeof(code99)];
    if (pread(fd, observed, sizeof(observed), 0) != sizeof(observed) ||
        memcmp(observed, code99, sizeof(observed)))
        return 1;
    void *next = mmap(text, page_size, PROT_READ | PROT_EXEC,
                      MAP_SHARED | MAP_FIXED_NOREPLACE, fd, 0);
    if (next == MAP_FAILED)
        return 1;
    int actual = ((int (*)(void))next)();
    printf("executable file refault: expected=99 actual=%d\n", actual);
    int failed = actual != 99;
    if (munmap(next, page_size) || close(fd))
        failed = 1;
    return failed;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    alarm(30);
    int failed = run();
    printf("STARRY_EXEC_CACHE_%s errno=%d\n", failed ? "FAILED" : "OK", errno);
    return failed;
}
