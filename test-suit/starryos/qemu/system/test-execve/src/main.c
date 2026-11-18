#define _GNU_SOURCE

/*
 * Focused StarryOS conformance test for the execve(2) family — execve() and
 * execveat(). fork() and wait4() appear only as scaffolding: each exec runs
 * in a forked child so the test harness survives, and wait4() collects the
 * child's exit status to confirm the image was actually replaced.
 *
 * execve() cases: replace a child with a shell or report ENOENT, observed via
 * the child's exit status.
 *
 * execveat() cases (Linux funnels both through do_execveat_common) exercise
 * the resolution modes it adds on top of execve — a relative path against a
 * directory fd or AT_FDCWD, an absolute path, AT_EMPTY_PATH — plus the
 * EINVAL/EBADF/ENOTDIR/ENOENT error returns.
 *
 * This is intentionally narrower than linux-compatible-testsuit's
 * test_fork_v2.c, which also covers clone/clone3, fd inheritance,
 * copy-on-write, session/process-group behavior, and more wait4 modes.
 */
#include "test_framework.h"

#include <elf.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdatomic.h>
#include <stdint.h>
#include <sys/mount.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

#ifndef SYS_execveat
#if defined(__x86_64__)
#define SYS_execveat 322
#elif defined(__aarch64__) || defined(__riscv) || defined(__loongarch__)
#define SYS_execveat 281
#else
#error "SYS_execveat is unknown for this architecture"
#endif
#endif

#ifndef AT_EMPTY_PATH
#define AT_EMPTY_PATH 0x1000
#endif

#ifndef SYS_memfd_create
#if defined(__x86_64__)
#define SYS_memfd_create 319
#elif defined(__aarch64__) || defined(__riscv) || defined(__loongarch__)
#define SYS_memfd_create 279
#else
#error "SYS_memfd_create is unknown for this architecture"
#endif
#endif

#ifndef MFD_ALLOW_SEALING
#define MFD_ALLOW_SEALING 0x0002U
#endif
#ifndef F_ADD_SEALS
#define F_ADD_SEALS 1033
#endif
#ifndef F_SEAL_WRITE
#define F_SEAL_WRITE 0x0008
#endif

static void check_exited_with(int status, int expected_code, const char *msg)
{
    CHECK(WIFEXITED(status), msg);
    if (WIFEXITED(status)) {
        CHECK(WEXITSTATUS(status) == expected_code, "child exit status matches");
    }
}

static void test_fork_child_exit_wait4(void)
{
    pid_t pid = fork();
    CHECK(pid >= 0, "fork creates a child process");
    if (pid < 0) {
        return;
    }

    if (pid == 0) {
        _exit(42);
    }

    int status = 0;
    // resource of sub process
    struct rusage usage;
    errno = 0;
    pid_t waited = wait4(pid, &status, 0, &usage);
    CHECK(waited == pid, "wait4 returns the forked child pid");
    if (waited == pid) {
        check_exited_with(status, 42, "forked child exits normally");
    }
}

static void test_fork_execve_shell_exit_wait4(void)
{
    pid_t pid = fork();
    CHECK(pid >= 0, "fork before execve succeeds");
    if (pid < 0) {
        return;
    }

    if (pid == 0) {
        char *const argv[] = { "/bin/sh", "-c", "exit 7", NULL };
        execve("/bin/sh", argv, environ);
        _exit(126);
    }

    int status = 0;
    struct rusage usage;
    errno = 0;
    pid_t waited = wait4(pid, &status, 0, &usage);
    CHECK(waited == pid, "wait4 observes execve child");
    if (waited == pid) {
        check_exited_with(status, 7, "execve child exits with shell status");
    }
}

static void test_execve_missing_path_reports_enoent(void)
{
    pid_t pid = fork();
    CHECK(pid >= 0, "fork for failing execve succeeds");
    if (pid < 0) {
        return;
    }

    if (pid == 0) {
        char *const argv[] = { "/no-such-starry-test-binary", NULL };
        char *const envp[] = { NULL };
        execve(argv[0], argv, envp);
        _exit(errno == ENOENT ? 127 : 126);
    }

    int status = 0;
    struct rusage usage;
    errno = 0;
    pid_t waited = wait4(pid, &status, 0, &usage);
    CHECK(waited == pid, "wait4 observes child after failed execve");
    if (waited == pid) {
        check_exited_with(status, 127, "execve missing path reports ENOENT in child");
    }
}

/* A raw exec must return its errno, not silently replace this child with sh. */
static void check_exec_rejection(const char *path, bool use_execveat, int expected)
{
    pid_t pid = fork();
    CHECK(pid >= 0, "fork for raw executable rejection");
    if (pid < 0) {
        return;
    }
    if (pid == 0) {
        char *const argv[] = {(char *)path, "-c", "exit 125", NULL};
        long result = use_execveat
            ? syscall(SYS_execveat, AT_FDCWD, path, argv, environ, 0)
            : syscall(SYS_execve, path, argv, environ);
        int actual = errno;
        if (result != -1 || actual != expected) {
            dprintf(STDERR_FILENO, "%s(%s): result=%ld errno=%d expected=%d\n",
                    use_execveat ? "execveat" : "execve", path, result, actual,
                    expected);
        }
        _exit(result == -1 && actual == expected ? 123 : 124);
    }
    int status = 0;
    CHECK(waitpid(pid, &status, 0) == pid, "collect rejected executable child");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 123,
          "raw exec preserves the caller and returns the expected errno");
}

static void test_exec_permission_and_format_rejection(void)
{
    char path[] = "/tmp/exec-rejection-XXXXXX.sh";
    int fd = mkstemps(path, 3);
    CHECK(fd >= 0, "create executable rejection fixture");
    if (fd < 0) {
        return;
    }
    CHECK(fchmod(fd, 0444) == 0, "remove every execute bit");
    close(fd);
    for (int api = 0; api < 2; api++) {
        check_exec_rejection(path, api != 0, EACCES);
    }
    CHECK(chmod(path, 0755) == 0, "permit execution of the invalid image");
    for (int api = 0; api < 2; api++) {
        check_exec_rejection(path, api != 0, ENOEXEC);
    }
    CHECK(unlink(path) == 0, "remove executable rejection fixture");
}

/* The shell command whose exit status proves the new image really ran. */
#define EXECVEAT_OK_STATUS 42
static char *const SH_ARGV[] = {"sh", "-c", "exit 42", NULL};

/* Descriptor spellings still traverse the caller's mounted procfs view. */
static void test_execve_descriptor_paths_obey_mount_view(void)
{
    int fd = open("/bin/sh", O_PATH | O_CLOEXEC);
    CHECK(fd >= 0, "retain executable through an O_PATH description");
    if (fd < 0) {
        return;
    }
    const char *prefixes[] = {"/proc/self/fd/", "/dev/fd/"};
    for (unsigned index = 0; index < sizeof(prefixes) / sizeof(prefixes[0]); index++) {
        char path[64];
        snprintf(path, sizeof(path), "%s%d", prefixes[index], fd);
        for (int covered = 0; covered < 2; covered++) {
            pid_t pid = fork();
            CHECK(pid >= 0, "fork descriptor-path executable actor");
            if (pid < 0) {
                continue;
            }
            if (pid == 0) {
                if (covered &&
                    (unshare(CLONE_NEWNS) != 0 ||
                     mount(NULL, "/", NULL, MS_PRIVATE | MS_REC, NULL) != 0 ||
                     mount("tmpfs", "/proc", "tmpfs", MS_NOSUID | MS_NODEV, NULL) != 0)) {
                    perror("cover procfs in executable actor");
                    _exit(125);
                }
                errno = 0;
                long result = syscall(SYS_execve, path, SH_ARGV, environ);
                int saved_errno = errno;
                if (covered && result == -1 && saved_errno == ENOENT) {
                    _exit(123);
                }
                dprintf(STDERR_FILENO, "execve(%s) covered=%d ret=%ld errno=%d\n",
                        path, covered, result, saved_errno);
                _exit(126);
            }
            int status = 0;
            CHECK(waitpid(pid, &status, 0) == pid,
                  "collect descriptor-path executable actor");
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) ==
                      (covered ? 123 : EXECVEAT_OK_STATUS),
                  "descriptor-path exec follows the mounted view and preserves rejection");
        }
    }
    close(fd);
}

static long do_execveat(int dirfd, const char *path, int flags)
{
    return syscall(SYS_execveat, dirfd, path, SH_ARGV, environ, flags);
}

/*
 * Run an execveat() expected to succeed, in a child, and return its wait
 * status. The child only reaches _exit(126) if exec returned (i.e. failed).
 */
static int execveat_success_status(int dirfd, const char *path, int flags)
{
    pid_t pid = fork();
    CHECK(pid >= 0, "fork before execveat succeeds");
    if (pid < 0) {
        return -1;
    }
    if (pid == 0) {
        do_execveat(dirfd, path, flags);
        _exit(126);
    }

    int status = 0;
    pid_t waited = waitpid(pid, &status, 0);
    CHECK(waited == pid, "waitpid collects the execveat child");
    return (waited == pid) ? status : -1;
}

/*
 * Run an execveat() expected to fail, in a child, and return the errno it
 * reported (as the child's exit code). Running in a child keeps a wrongly
 * succeeding exec from replacing this test harness.
 */
static int execveat_failure_errno(int dirfd, const char *path, int flags)
{
    pid_t pid = fork();
    CHECK(pid >= 0, "fork before failing execveat succeeds");
    if (pid < 0) {
        return -1;
    }
    if (pid == 0) {
        do_execveat(dirfd, path, flags);
        _exit(errno);
    }

    int status = 0;
    pid_t waited = waitpid(pid, &status, 0);
    if (waited != pid || !WIFEXITED(status)) {
        return -1;
    }
    return WEXITSTATUS(status);
}

static void check_execveat_ran(int status, const char *msg)
{
    CHECK(WIFEXITED(status), msg);
    if (WIFEXITED(status)) {
        CHECK(WEXITSTATUS(status) == EXECVEAT_OK_STATUS,
              "execveat child exits with the shell's status");
    }
}

static void test_execveat_relative_path_via_dirfd(void)
{
    int bin = open("/bin", O_RDONLY | O_DIRECTORY);
    CHECK(bin >= 0, "open /bin as a directory fd");
    if (bin < 0) {
        return;
    }
    int status = execveat_success_status(bin, "sh", 0);
    check_execveat_ran(status, "execveat resolves a relative path against dirfd");
    close(bin);
}

static void test_execveat_absolute_path_ignores_dirfd(void)
{
    /* 999 is not an open fd: an absolute pathname must ignore dirfd entirely. */
    int status = execveat_success_status(999, "/bin/sh", 0);
    check_execveat_ran(status, "execveat absolute path ignores dirfd");
}

static void test_execveat_relative_path_via_fdcwd(void)
{
    char cwd[256];
    CHECK(getcwd(cwd, sizeof(cwd)) != NULL, "snapshot cwd");
    CHECK(chdir("/bin") == 0, "chdir into /bin");

    int status = execveat_success_status(AT_FDCWD, "sh", 0);
    check_execveat_ran(status,
                       "execveat resolves a relative path against AT_FDCWD");

    CHECK(chdir(cwd) == 0, "restore cwd");
}

static void test_execveat_at_empty_path_executes_fd(void)
{
    int fd = open("/bin/sh", O_RDONLY);
    CHECK(fd >= 0, "open /bin/sh for AT_EMPTY_PATH exec");
    if (fd < 0) {
        return;
    }
    int status = execveat_success_status(fd, "", AT_EMPTY_PATH);
    check_execveat_ran(status, "execveat AT_EMPTY_PATH executes the open fd");
    close(fd);
}

/* Copy every byte of `src_path` into the already-open `dst_fd`. */
static int copy_file_into(int dst_fd, const char *src_path)
{
    int src = open(src_path, O_RDONLY);
    if (src < 0) {
        return -1;
    }
    char buf[4096];
    ssize_t n;
    while ((n = read(src, buf, sizeof(buf))) > 0) {
        ssize_t off = 0;
        while (off < n) {
            ssize_t w = write(dst_fd, buf + off, (size_t)(n - off));
            if (w <= 0) {
                close(src);
                return -1;
            }
            off += w;
        }
    }
    close(src);
    return n < 0 ? -1 : 0;
}

/* Redirect the copied native shell to a private copy of its actual loader. */
static int redirect_interpreter(int fd, const char *replacement,
                                char *original, size_t capacity)
{
    Elf64_Ehdr header;
    if (pread(fd, &header, sizeof(header), 0) != (ssize_t)sizeof(header)
        || memcmp(header.e_ident, ELFMAG, SELFMAG) != 0
        || header.e_ident[EI_CLASS] != ELFCLASS64
        || header.e_phentsize != sizeof(Elf64_Phdr)) {
        errno = ENOEXEC;
        return -1;
    }
    for (unsigned index = 0; index < header.e_phnum; index++) {
        off_t offset = (off_t)header.e_phoff + index * sizeof(Elf64_Phdr);
        Elf64_Phdr ph;
        if (pread(fd, &ph, sizeof(ph), offset) != (ssize_t)sizeof(ph)) {
            return -1;
        }
        if (ph.p_type != PT_INTERP) {
            continue;
        }
        size_t length = strlen(replacement) + 1;
        if (ph.p_filesz < length || ph.p_filesz > capacity
            || pread(fd, original, ph.p_filesz, (off_t)ph.p_offset)
                   != (ssize_t)ph.p_filesz
            || original[ph.p_filesz - 1] != '\0') {
            errno = ENOEXEC;
            return -1;
        }
        if (pwrite(fd, replacement, length, (off_t)ph.p_offset)
            != (ssize_t)length) {
            return -1;
        }
        ph.p_filesz = length;
        ph.p_memsz = length;
        return pwrite(fd, &ph, sizeof(ph), offset) == (ssize_t)sizeof(ph) ? 0 : -1;
    }
    errno = ENOEXEC;
    return -1;
}

/* Reject an invalid native header, then execute after repairing the same inode. */
static void check_header_recovery(const char *image, const char *target,
                                  off_t offset, const void *invalid,
                                  size_t field_size, int expected)
{
    printf("ELF_HEADER_REJECTION target=%s offset=%lld size=%zu errno=%d\n",
           target, (long long)offset, field_size, expected);
    fflush(stdout);
    int fd = open(target, O_RDWR);
    CHECK(fd >= 0, "open the private ELF header for mutation");
    if (fd < 0) {
        return;
    }
    unsigned char original[sizeof(Elf64_Off)];
    CHECK(field_size <= sizeof(original), "ELF field fits the recovery buffer");
    if (field_size > sizeof(original)) {
        close(fd);
        return;
    }
    ssize_t read = pread(fd, original, field_size, offset);
    CHECK(read == (ssize_t)field_size, "read the original ELF header field");
    if (read != (ssize_t)field_size) {
        close(fd);
        return;
    }
    ssize_t written = pwrite(fd, invalid, field_size, offset);
    CHECK(written == (ssize_t)field_size, "write the invalid ELF header field");
    close(fd);
    if (written != (ssize_t)field_size) {
        return;
    }

    check_exec_rejection(image, false, expected);
    check_exec_rejection(image, true, expected);

    fd = open(target, O_WRONLY);
    CHECK(fd >= 0, "rejected ELF header releases its inode's write exclusion");
    if (fd < 0) {
        return;
    }
    written = pwrite(fd, original, field_size, offset);
    CHECK(written == (ssize_t)field_size, "repair the same ELF header inode");
    close(fd);
    if (written == (ssize_t)field_size) {
        check_execveat_ran(execveat_success_status(AT_FDCWD, image, 0),
                          "repaired native headers execute the shell image");
    }
}

/* Native code for a minimal ELF that has no interpreter or libc dependency. */
#if defined(__x86_64__)
#define NATIVE_EXIT_CODE "mov $42, %edi\nmov $60, %eax\nsyscall\n"
#elif defined(__aarch64__)
#define NATIVE_EXIT_CODE "mov x0, #42\nmov x8, #93\nsvc #0\n"
#elif defined(__riscv)
#define NATIVE_EXIT_CODE "li a0, 42\nli a7, 93\necall\n"
#elif defined(__loongarch__)
#define NATIVE_EXIT_CODE "li.w $a0, 42\nli.w $a7, 93\nsyscall 0\n"
#else
#error "Native exit code is unknown for this architecture"
#endif
__asm__(".pushsection .rodata\n"
        ".balign 4\n"
        ".global exec_native_exit_start, exec_native_exit_end\n"
        "exec_native_exit_start:\n"
        NATIVE_EXIT_CODE
        "exec_native_exit_end:\n"
        ".popsection\n");
extern const unsigned char exec_native_exit_start[], exec_native_exit_end[];

/* File offsets need not have the alignment required by host Rust references. */
static void check_unaligned_program_table(const char *image, const char *target)
{
    int source = open(image, O_RDONLY);
    Elf64_Ehdr header;
    int ready = source >= 0
        && pread(source, &header, sizeof(header), 0) == (ssize_t)sizeof(header);
    CHECK(ready, "read the native ABI for the standalone ELF");
    if (source >= 0) {
        close(source);
    }
    if (!ready) {
        return;
    }
    header.e_type = ET_EXEC;
    header.e_entry = 0x400000 + 128;
    header.e_phoff = sizeof(header) + 1;
    header.e_phnum = 1;
    header.e_shoff = 0;
    header.e_shnum = 0;
    header.e_shstrndx = SHN_UNDEF;
    size_t code_size = (uintptr_t)exec_native_exit_end - (uintptr_t)exec_native_exit_start;
    Elf64_Phdr ph = {
        .p_type = PT_LOAD,
        .p_flags = PF_R | PF_X,
        .p_vaddr = 0x400000,
        .p_filesz = 128 + code_size,
        .p_memsz = 128 + code_size,
        .p_align = 4096,
    };
    int fd = open(target, O_CREAT | O_EXCL | O_WRONLY, 0755);
    CHECK(fd >= 0, "create the standalone native ELF");
    if (fd < 0) {
        return;
    }
    ready = pwrite(fd, &header, sizeof(header), 0) == (ssize_t)sizeof(header)
        && pwrite(fd, &ph, sizeof(ph), header.e_phoff) == (ssize_t)sizeof(ph)
        && pwrite(fd, exec_native_exit_start, code_size, 128) == (ssize_t)code_size;
    CHECK(ready, "stage the unaligned program table and native entry");
    close(fd);
    if (ready) {
        printf("ELF_UNALIGNED_PROGRAM_TABLE offset=%llu\n",
               (unsigned long long)header.e_phoff);
        fflush(stdout);
        for (int api = 0; api < 2; api++) {
            pid_t pid = fork();
            CHECK(pid >= 0, "fork before executing the standalone native ELF");
            if (pid == 0) {
                char *const argv[] = {(char *)target, NULL};
                if (api == 0) {
                    syscall(SYS_execve, target, argv, environ);
                } else {
                    syscall(SYS_execveat, AT_FDCWD, target, argv, environ, 0);
                }
                _exit(126);
            }
            if (pid > 0) {
                int status = 0;
                CHECK(waitpid(pid, &status, 0) == pid, "collect the native ELF child");
                check_exited_with(status, EXECVEAT_OK_STATUS,
                                  "a legal unaligned program table executes its image");
            }
        }
    }
    CHECK(unlink(target) == 0, "remove the standalone native ELF");
}

/* A failed interpreter load must preserve the caller and release both leases. */
static void test_interpreter_format_recovery(void)
{
    char directory[] = "/tmp/xi-XXXXXX";
    char *created = mkdtemp(directory);
    CHECK(created != NULL, "create a private interpreter fixture");
    if (created == NULL) {
        return;
    }
    char image[64], interpreter[64], original[4096];
    snprintf(image, sizeof(image), "%s/image", directory);
    snprintf(interpreter, sizeof(interpreter), "%s/i", directory);
    int image_fd = open(image, O_CREAT | O_EXCL | O_RDWR, 0755);
    int interpreter_fd = -1;
    CHECK(image_fd >= 0, "create the private executable image");
    if (image_fd < 0) {
        goto cleanup;
    }
    int prepared = copy_file_into(image_fd, "/bin/sh");
    if (prepared == 0) {
        prepared = redirect_interpreter(image_fd, interpreter, original,
                                       sizeof(original));
    }
    CHECK(prepared == 0, "stage the shell with a private PT_INTERP path");
    if (prepared != 0) {
        goto cleanup;
    }
    interpreter_fd = open(interpreter, O_CREAT | O_EXCL | O_RDWR, 0755);
    CHECK(interpreter_fd >= 0, "create the private ELF interpreter");
    if (interpreter_fd < 0) {
        goto cleanup;
    }
    prepared = copy_file_into(interpreter_fd, original);
    CHECK(prepared == 0, "copy the actual dynamic linker");
    if (prepared != 0) {
        goto cleanup;
    }
    CHECK(pwrite(interpreter_fd, "BAD!", SELFMAG, 0) == SELFMAG,
          "corrupt the complete interpreter's ELF magic");
    close(image_fd);
    close(interpreter_fd);
    image_fd = interpreter_fd = -1;

    check_exec_rejection(image, false, ELIBBAD);
    check_exec_rejection(image, true, ELIBBAD);
    interpreter_fd = open(interpreter, O_WRONLY);
    CHECK(interpreter_fd >= 0, "reopen the rejected interpreter for truncation");
    if (interpreter_fd < 0) {
        goto cleanup;
    }
    CHECK(ftruncate(interpreter_fd, SELFMAG) == 0,
          "truncate the interpreter below a complete native ELF header");
    close(interpreter_fd);
    interpreter_fd = -1;
    check_exec_rejection(image, false, EIO);
    check_exec_rejection(image, true, EIO);
    image_fd = open(image, O_WRONLY);
    CHECK(image_fd >= 0, "failed exec releases the main image's write exclusion");
    if (image_fd >= 0) {
        close(image_fd);
        image_fd = -1;
    }
    interpreter_fd = open(interpreter, O_WRONLY);
    CHECK(interpreter_fd >= 0,
          "failed exec releases the interpreter's write exclusion");
    if (interpreter_fd < 0) {
        goto cleanup;
    }
    prepared = copy_file_into(interpreter_fd, original);
    CHECK(prepared == 0, "restore the same interpreter inode's complete contents");
    if (prepared != 0) {
        goto cleanup;
    }
    close(interpreter_fd);
    interpreter_fd = -1;
    check_execveat_ran(execveat_success_status(AT_FDCWD, image, 0),
                      "repaired interpreter executes the original shell image");
    Elf64_Half invalid = EM_NONE;
    check_header_recovery(image, image, offsetof(Elf64_Ehdr, e_machine),
                          &invalid, sizeof(invalid), ENOEXEC);
    check_header_recovery(image, interpreter, offsetof(Elf64_Ehdr, e_machine),
                          &invalid, sizeof(invalid), ELIBBAD);
    invalid = ET_REL;
    check_header_recovery(image, image, offsetof(Elf64_Ehdr, e_type),
                          &invalid, sizeof(invalid), ENOEXEC);

    const struct {
        off_t offset;
        Elf64_Half value;
    } invalid_tables[] = {
        {offsetof(Elf64_Ehdr, e_phentsize), 0},
        {offsetof(Elf64_Ehdr, e_phentsize), sizeof(Elf64_Phdr) - 1},
        {offsetof(Elf64_Ehdr, e_phnum), 0},
        {offsetof(Elf64_Ehdr, e_phnum), 65536 / sizeof(Elf64_Phdr) + 1},
    };
    for (size_t i = 0; i < sizeof(invalid_tables) / sizeof(invalid_tables[0]); i++) {
        const void *value = &invalid_tables[i].value;
        size_t size = sizeof(invalid_tables[i].value);
        check_header_recovery(image, image, invalid_tables[i].offset,
                              value, size, ENOEXEC);
        check_header_recovery(image, interpreter, invalid_tables[i].offset,
                              value, size, ELIBBAD);
    }
    const char *targets[] = {image, interpreter};
    for (size_t i = 0; i < sizeof(targets) / sizeof(targets[0]); i++) {
        struct stat st;
        int ready = stat(targets[i], &st);
        CHECK(ready == 0, "inspect the original ELF table extent");
        if (ready != 0) {
            continue;
        }
        const Elf64_Off offsets[] = {UINT64_MAX - 27, st.st_size - 1};
        for (size_t j = 0; j < sizeof(offsets) / sizeof(offsets[0]); j++) {
            check_header_recovery(image, targets[i], offsetof(Elf64_Ehdr, e_phoff),
                                  &offsets[j], sizeof(offsets[j]),
                                  i == 0 ? ENOEXEC : ELIBBAD);
        }
    }
    char standalone[64];
    snprintf(standalone, sizeof(standalone), "%s/unaligned", directory);
    check_unaligned_program_table(image, standalone);

cleanup:
    if (image_fd >= 0) {
        close(image_fd);
    }
    if (interpreter_fd >= 0) {
        close(interpreter_fd);
    }
    CHECK(unlink(image) == 0 || errno == ENOENT, "remove the private executable");
    CHECK(unlink(interpreter) == 0 || errno == ENOENT,
          "remove the private interpreter");
    CHECK(rmdir(directory) == 0, "remove the interpreter fixture directory");
}

/*
 * systemd's pattern: stage an executable into an anonymous memfd, seal it
 * write-only-once, then execveat(fd, "", AT_EMPTY_PATH). The initial anonymous
 * memfd description does not acquire pathname-open write access on Linux;
 * sealing protects contents independently of executable/write exclusion.
 */
static void test_execveat_memfd_sealed_exec(void)
{
    int mfd = (int)syscall(SYS_memfd_create, "execve-test", MFD_ALLOW_SEALING);
    CHECK(mfd >= 0, "memfd_create for execveat");
    if (mfd < 0) {
        return;
    }

    CHECK(copy_file_into(mfd, "/bin/sh") == 0, "stage /bin/sh into the memfd");
    CHECK(fcntl(mfd, F_ADD_SEALS, F_SEAL_WRITE) == 0,
          "seal the memfd write access before exec");

    int status = execveat_success_status(mfd, "", AT_EMPTY_PATH);
    check_execveat_ran(status,
                       "execveat AT_EMPTY_PATH executes a sealed memfd image");
    close(mfd);
}

static void test_execveat_error_returns(void)
{
    /* 0x4 is outside the accepted AT_EMPTY_PATH|AT_SYMLINK_NOFOLLOW set. */
    CHECK(execveat_failure_errno(AT_FDCWD, "/bin/sh", 0x4) == EINVAL,
          "execveat rejects unknown flag bits with EINVAL");
    CHECK(execveat_failure_errno(999, "sh", 0) == EBADF,
          "execveat relative path against a closed dirfd returns EBADF");
    CHECK(execveat_failure_errno(AT_FDCWD, "/no-such-starry-execveat", 0)
              == ENOENT,
          "execveat missing program returns ENOENT");
}

static void test_execveat_non_directory_dirfd_enotdir(void)
{
    int fd = open("/bin/sh", O_RDONLY);
    CHECK(fd >= 0, "open /bin/sh as a non-directory fd");
    if (fd < 0) {
        return;
    }
    CHECK(execveat_failure_errno(fd, "sh", 0) == ENOTDIR,
          "execveat relative path against a non-directory fd returns ENOTDIR");
    close(fd);
}

/*
 * Each payload string and the pointer vectors below are individually valid.
 * Their aggregate is deliberately just over StarryOS's 2 MiB execve budget.
 * The child uses a shell exit status that differs from the E2BIG sentinel, so
 * a missing limit cannot accidentally look like a passing test.
 */
#define EXECVE_PAYLOAD_BYTES 8192
#define EXECVE_LIMIT_PAYLOAD_COUNT 257

static int execve_payload_status(size_t argv_payload_count, size_t envp_payload_count)
{
    pid_t pid = fork();
    if (pid < 0) {
        return -1;
    }
    if (pid == 0) {
        char *payload = malloc(EXECVE_PAYLOAD_BYTES);
        char **argv = calloc(argv_payload_count + 4, sizeof(*argv));
        char **envp = calloc(envp_payload_count + 1, sizeof(*envp));
        if (payload == NULL || argv == NULL || envp == NULL) {
            _exit(125);
        }

        memset(payload, 'x', EXECVE_PAYLOAD_BYTES - 1);
        payload[0] = 'X';
        payload[1] = '=';
        payload[EXECVE_PAYLOAD_BYTES - 1] = '\0';

        argv[0] = "/bin/sh";
        argv[1] = "-c";
        argv[2] = "exit 42";
        for (size_t i = 0; i < argv_payload_count; ++i) {
            argv[i + 3] = payload;
        }
        for (size_t i = 0; i < envp_payload_count; ++i) {
            envp[i] = payload;
        }

        execve("/bin/sh", argv, envp);
        _exit(errno == E2BIG ? 0 : 1);
    }

    int status = 0;
    return waitpid(pid, &status, 0) == pid && WIFEXITED(status)
               ? WEXITSTATUS(status)
               : -1;
}

static void test_execve_aggregate_argument_budget(void)
{
    CHECK(execve_payload_status(EXECVE_LIMIT_PAYLOAD_COUNT, 0) == 0,
          "execve rejects aggregate argv bytes with E2BIG");
    CHECK(execve_payload_status(0, EXECVE_LIMIT_PAYLOAD_COUNT) == 0,
          "execve rejects aggregate envp bytes with E2BIG");
    CHECK(execve_payload_status(EXECVE_LIMIT_PAYLOAD_COUNT / 2,
                               EXECVE_LIMIT_PAYLOAD_COUNT / 2) == 0,
          "execve applies one shared argv/envp byte budget");
}

#define EXEC_ASPACE_RACE_ROUNDS 32
#define EXEC_ASPACE_RACE_THREADS 8

struct exec_aspace_race {
    atomic_uint ready_threads;
    atomic_bool keep_running;
    long online_cpus;
};

static void *exec_aspace_sibling(void *arg)
{
    struct exec_aspace_race *race = arg;
    unsigned int sibling = atomic_fetch_add_explicit(
        &race->ready_threads, 1, memory_order_release);

    if (race->online_cpus > 1) {
        cpu_set_t cpus;
        CPU_ZERO(&cpus);
        CPU_SET((int)(sibling % (unsigned int)race->online_cpus), &cpus);
        (void)pthread_setaffinity_np(pthread_self(), sizeof(cpus), &cpus);
    }

    while (atomic_load_explicit(&race->keep_running, memory_order_acquire)) {
        (void)syscall(SYS_gettid);
        sched_yield();
    }
    return NULL;
}

/*
 * A successful exec destroys every sibling thread. The retiring siblings may
 * already be absent from the thread-group registry while still finishing
 * their final kernel return on another CPU. Repeating this interleaving makes
 * the page-table ownership boundary observable: the new image must run and
 * exit cleanly every time, without an instruction-page-fault loop or hang.
 */
static void test_multithread_exec_keeps_retiring_page_table_roots_alive(void)
{
    int failed_round = -1;
    int failed_status = 0;

    for (int round = 0; round < EXEC_ASPACE_RACE_ROUNDS; ++round) {
        pid_t pid = fork();
        if (pid < 0) {
            failed_round = round;
            failed_status = errno;
            break;
        }
        if (pid == 0) {
            struct exec_aspace_race race;
            pthread_t siblings[EXEC_ASPACE_RACE_THREADS];
            atomic_init(&race.ready_threads, 0);
            atomic_init(&race.keep_running, true);
            race.online_cpus = sysconf(_SC_NPROCESSORS_ONLN);

            int created = 0;
            for (; created < EXEC_ASPACE_RACE_THREADS; ++created) {
                int create_error = pthread_create(&siblings[created], NULL,
                                                  exec_aspace_sibling, &race);
                if (create_error != 0) {
                    fprintf(stderr,
                            "multithread exec pthread_create failed: "
                            "round=%d created=%d error=%d\n",
                            round, created, create_error);
                    atomic_store_explicit(&race.keep_running, false,
                                          memory_order_release);
                    for (int i = 0; i < created; ++i) {
                        pthread_join(siblings[i], NULL);
                    }
                    _exit(125);
                }
            }
            while (atomic_load_explicit(&race.ready_threads,
                                        memory_order_acquire)
                   != EXEC_ASPACE_RACE_THREADS) {
                sched_yield();
            }

            char *const argv[] = { "/bin/true", NULL };
            syscall(SYS_execve, argv[0], argv, environ);

            atomic_store_explicit(&race.keep_running, false,
                                  memory_order_release);
            for (int i = 0; i < created; ++i) {
                pthread_join(siblings[i], NULL);
            }
            _exit(126);
        }

        int status = 0;
        if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status)
            || WEXITSTATUS(status) != 0) {
            failed_round = round;
            failed_status = status;
            break;
        }
    }

    if (failed_round >= 0) {
        fprintf(stderr, "multithread exec failed at round %d status=%d\n",
                failed_round, failed_status);
    }
    CHECK(failed_round < 0,
          "multithread exec keeps retiring siblings' page-table roots alive");
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("execve/execveat family semantics");

    test_fork_child_exit_wait4();
    test_fork_execve_shell_exit_wait4();
    test_execve_missing_path_reports_enoent();
    test_exec_permission_and_format_rejection();

    test_execve_descriptor_paths_obey_mount_view();
    test_execveat_relative_path_via_dirfd();
    test_execveat_absolute_path_ignores_dirfd();
    test_execveat_relative_path_via_fdcwd();
    test_execveat_at_empty_path_executes_fd();
    test_interpreter_format_recovery();
    test_execveat_memfd_sealed_exec();
    test_execveat_error_returns();
    test_execveat_non_directory_dirfd_enotdir();
    test_execve_aggregate_argument_budget();
    test_multithread_exec_keeps_retiring_page_table_roots_alive();

    TEST_DONE();
}
