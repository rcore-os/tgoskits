#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <elf.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#define MISALIGNED_ELF_PATH "/tmp/elf-load-misaligned"
#define OVERSIZED_INTERP_ELF_PATH "/tmp/elf-interp-oversized"
#define TRUNCATED_INTERP_ELF_PATH "/tmp/elf-interp-truncated"
#define MAX_INTERPRETER_PATH_LEN 4096

static int write_all(int fd, const unsigned char *buffer, size_t count)
{
    while (count > 0) {
        ssize_t written = write(fd, buffer, count);
        if (written <= 0) {
            return -1;
        }
        buffer += (size_t)written;
        count -= (size_t)written;
    }
    return 0;
}

static int copy_file(const char *source, const char *destination)
{
    unsigned char buffer[4096];
    int input = open(source, O_RDONLY);
    int output = -1;
    int result = -1;

    if (input < 0) {
        perror("open /proc/self/exe");
        goto out;
    }
    output = open(destination, O_WRONLY | O_CREAT | O_TRUNC, 0755);
    if (output < 0) {
        perror("open destination");
        goto out;
    }

    for (;;) {
        ssize_t bytes = read(input, buffer, sizeof(buffer));
        if (bytes == 0) {
            result = 0;
            break;
        }
        if (bytes < 0 || write_all(output, buffer, (size_t)bytes) != 0) {
            perror("copy executable");
            break;
        }
    }

out:
    if (output >= 0 && close(output) < 0) {
        perror("close destination");
        result = -1;
    }
    if (input >= 0) {
        close(input);
    }
    return result;
}

static int read_exact_at(int fd, void *buffer, size_t count, off_t offset)
{
    unsigned char *cursor = buffer;

    while (count > 0) {
        ssize_t bytes = pread(fd, cursor, count, offset);
        if (bytes <= 0) {
            return -1;
        }
        cursor += (size_t)bytes;
        count -= (size_t)bytes;
        offset += bytes;
    }
    return 0;
}

static int corrupt_load_alignment(const char *path)
{
    Elf64_Ehdr header;
    struct stat stat_buffer;
    int fd = open(path, O_RDWR);
    int result = -1;

    if (fd < 0) {
        perror("open ELF for modification");
        return -1;
    }
    if (fstat(fd, &stat_buffer) != 0 ||
        read_exact_at(fd, &header, sizeof(header), 0) != 0 ||
        memcmp(header.e_ident, ELFMAG, SELFMAG) != 0 ||
        header.e_ident[EI_CLASS] != ELFCLASS64 ||
        header.e_phentsize != sizeof(Elf64_Phdr) ||
        header.e_phnum == 0 ||
        header.e_phoff > (uint64_t)stat_buffer.st_size ||
        (uint64_t)stat_buffer.st_size - header.e_phoff <
            (uint64_t)header.e_phnum * sizeof(Elf64_Phdr)) {
        fputs("unexpected ELF layout\n", stderr);
        goto out;
    }

    for (size_t index = 0; index < header.e_phnum; index++) {
        const off_t offset = (off_t)(header.e_phoff + index * sizeof(Elf64_Phdr));
        Elf64_Phdr program_header;
        if (read_exact_at(fd, &program_header, sizeof(program_header), offset) != 0) {
            perror("read program header");
            goto out;
        }
        if (program_header.p_type != PT_LOAD || program_header.p_vaddr == UINT64_MAX) {
            continue;
        }

        // Preserve a structurally valid header while making p_vaddr and
        // p_offset disagree within their page. The loader must reject the
        // malformed image before attempting to map it.
        program_header.p_vaddr++;
        if (pwrite(fd, &program_header, sizeof(program_header), offset) !=
            (ssize_t)sizeof(program_header)) {
            perror("write program header");
            goto out;
        }
        result = 0;
        break;
    }

    if (result != 0) {
        fputs("ELF has no mutable PT_LOAD header\n", stderr);
    }

out:
    close(fd);
    return result;
}

static int corrupt_interpreter(const char *path, int oversized)
{
    Elf64_Ehdr header;
    struct stat stat_buffer;
    const uint32_t target_types[] = {PT_INTERP, PT_LOAD};
    int fd = open(path, O_RDWR);
    int result = -1;

    if (fd < 0) {
        perror("open ELF for modification");
        return -1;
    }
    if (fstat(fd, &stat_buffer) != 0 ||
        read_exact_at(fd, &header, sizeof(header), 0) != 0 ||
        memcmp(header.e_ident, ELFMAG, SELFMAG) != 0 ||
        header.e_ident[EI_CLASS] != ELFCLASS64 ||
        header.e_phentsize != sizeof(Elf64_Phdr) ||
        header.e_phnum == 0 || stat_buffer.st_size < 2 ||
        header.e_phoff > (uint64_t)stat_buffer.st_size ||
        (uint64_t)stat_buffer.st_size - header.e_phoff <
            (uint64_t)header.e_phnum * sizeof(Elf64_Phdr)) {
        fputs("unexpected ELF layout\n", stderr);
        goto out;
    }

    for (size_t target = 0;
         target < sizeof(target_types) / sizeof(target_types[0]) && result != 0;
         target++) {
        for (size_t index = 0; index < header.e_phnum; index++) {
            const off_t offset = (off_t)(header.e_phoff + index * sizeof(Elf64_Phdr));
            Elf64_Phdr program_header;
            if (read_exact_at(fd, &program_header, sizeof(program_header), offset) != 0) {
                perror("read program header");
                goto out;
            }
            if (program_header.p_type != target_types[target]) {
                continue;
            }

            // Exercise the interpreter the loader actually consumes. Static
            // test binaries have no PT_INTERP, so repurpose a PT_LOAD only as
            // a fallback for that case.
            if (target_types[target] == PT_LOAD) {
                program_header.p_type = PT_INTERP;
            }
            if (oversized) {
                program_header.p_filesz = MAX_INTERPRETER_PATH_LEN + 1;
            } else {
                program_header.p_offset = (Elf64_Off)stat_buffer.st_size - 1;
                program_header.p_filesz = 2;
            }
            if (pwrite(fd, &program_header, sizeof(program_header), offset) !=
                (ssize_t)sizeof(program_header)) {
                perror("write program header");
                goto out;
            }
            result = 0;
            break;
        }
    }

    if (result != 0) {
        fputs("ELF has no mutable PT_LOAD header\n", stderr);
    }

out:
    close(fd);
    return result;
}

static int expect_enoexec(const char *path)
{
    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return -1;
    }
    if (child == 0) {
        char *const argv[] = {(char *)path, NULL};
        char *const envp[] = {"LOADER_ALIGNMENT_TARGET=1", NULL};
        execve(path, argv, envp);
        if (errno != ENOEXEC) {
            fprintf(stderr, "execve returned errno=%d, expected ENOEXEC\n", errno);
            _exit(1);
        }
        _exit(0);
    }

    int status = 0;
    if (waitpid(child, &status, 0) < 0) {
        perror("waitpid");
        return -1;
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "malformed ELF child status=0x%x\n", status);
        return -1;
    }
    return 0;
}

int main(void)
{
    char executable[PATH_MAX];

    // If the corrupted copy unexpectedly reaches user space, make the parent
    // fail rather than recursively creating another malformed copy.
    if (getenv("LOADER_ALIGNMENT_TARGET") != NULL) {
        return 99;
    }

    ssize_t length = readlink("/proc/self/exe", executable, sizeof(executable) - 1);
    if (length < 0 || (size_t)length >= sizeof(executable) - 1) {
        perror("readlink /proc/self/exe");
        return 1;
    }
    executable[length] = '\0';

    unlink(MISALIGNED_ELF_PATH);
    unlink(OVERSIZED_INTERP_ELF_PATH);
    unlink(TRUNCATED_INTERP_ELF_PATH);
    if (copy_file(executable, MISALIGNED_ELF_PATH) != 0 ||
        corrupt_load_alignment(MISALIGNED_ELF_PATH) != 0 ||
        expect_enoexec(MISALIGNED_ELF_PATH) != 0 ||
        copy_file(executable, OVERSIZED_INTERP_ELF_PATH) != 0 ||
        corrupt_interpreter(OVERSIZED_INTERP_ELF_PATH, 1) != 0 ||
        expect_enoexec(OVERSIZED_INTERP_ELF_PATH) != 0 ||
        copy_file(executable, TRUNCATED_INTERP_ELF_PATH) != 0 ||
        corrupt_interpreter(TRUNCATED_INTERP_ELF_PATH, 0) != 0 ||
        expect_enoexec(TRUNCATED_INTERP_ELF_PATH) != 0) {
        unlink(MISALIGNED_ELF_PATH);
        unlink(OVERSIZED_INTERP_ELF_PATH);
        unlink(TRUNCATED_INTERP_ELF_PATH);
        return 1;
    }
    unlink(MISALIGNED_ELF_PATH);
    unlink(OVERSIZED_INTERP_ELF_PATH);
    unlink(TRUNCATED_INTERP_ELF_PATH);

    puts("ELF_LOADER_VALIDATION_OK");
    return 0;
}
