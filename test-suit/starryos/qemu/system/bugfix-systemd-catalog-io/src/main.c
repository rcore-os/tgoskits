#define _GNU_SOURCE

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define CATALOG_FILE_COUNT 17
#define CATALOG_FILE_SIZE (24 * 1024)
#define DATABASE_SIZE 289361
#define TEST_TIMEOUT_SECONDS 30

static char fixture_root[192];
static char source_dir[224];
static char database_path[224];
static char database_tmp_path[224];

enum test_stage {
    STAGE_SETUP = 1,
    STAGE_ENUMERATE,
    STAGE_READ,
    STAGE_WRITE,
    STAGE_RENAME,
};

static volatile sig_atomic_t current_stage = STAGE_SETUP;

static void timeout_handler(int signal_number)
{
    const char *message = "TIMEOUT: unknown stage\n";
    size_t message_length = sizeof("TIMEOUT: unknown stage\n") - 1;

    (void)signal_number;
    switch (current_stage) {
    case STAGE_SETUP:
        message = "TIMEOUT: fixture setup\n";
        message_length = sizeof("TIMEOUT: fixture setup\n") - 1;
        break;
    case STAGE_ENUMERATE:
        message = "TIMEOUT: catalog directory enumeration\n";
        message_length = sizeof("TIMEOUT: catalog directory enumeration\n") - 1;
        break;
    case STAGE_READ:
        message = "TIMEOUT: procfd buffered catalog reads\n";
        message_length = sizeof("TIMEOUT: procfd buffered catalog reads\n") - 1;
        break;
    case STAGE_WRITE:
        message = "TIMEOUT: buffered database write\n";
        message_length = sizeof("TIMEOUT: buffered database write\n") - 1;
        break;
    case STAGE_RENAME:
        message = "TIMEOUT: database rename\n";
        message_length = sizeof("TIMEOUT: database rename\n") - 1;
        break;
    }

    (void)write(STDERR_FILENO, message, message_length);
    (void)write(
        STDERR_FILENO,
        "STARRY_GROUPED_TEST_FAILED: bugfix-systemd-catalog-io\n",
        sizeof("STARRY_GROUPED_TEST_FAILED: bugfix-systemd-catalog-io\n") - 1
    );
    _exit(124);
}

static int fail_errno(const char *operation, const char *path)
{
    fprintf(
        stderr,
        "FAIL: %s path=%s errno=%d (%s)\n",
        operation,
        path,
        errno,
        strerror(errno)
    );
    puts("STARRY_GROUPED_TEST_FAILED: bugfix-systemd-catalog-io");
    return 1;
}

static int fail_message(const char *operation, const char *message)
{
    fprintf(stderr, "FAIL: %s: %s\n", operation, message);
    puts("STARRY_GROUPED_TEST_FAILED: bugfix-systemd-catalog-io");
    return 1;
}

static int write_all(int fd, const void *buffer, size_t size)
{
    const unsigned char *bytes = buffer;
    size_t written = 0;

    while (written < size) {
        ssize_t result = write(fd, bytes + written, size - written);
        if (result < 0) {
            return -1;
        }
        if (result == 0) {
            errno = EIO;
            return -1;
        }
        written += (size_t)result;
    }
    return 0;
}

static int initialize_paths(const char *base_directory)
{
    int fixture_length = snprintf(
        fixture_root,
        sizeof(fixture_root),
        "%s/bugfix-systemd-catalog-io",
        base_directory
    );
    if (fixture_length < 0 || (size_t)fixture_length >= sizeof(fixture_root)) {
        return fail_message("initialize fixture path", "base directory is too long");
    }

    int source_length = snprintf(
        source_dir,
        sizeof(source_dir),
        "%s/catalog",
        fixture_root
    );
    int database_length = snprintf(
        database_path,
        sizeof(database_path),
        "%s/database",
        fixture_root
    );
    int temporary_length = snprintf(
        database_tmp_path,
        sizeof(database_tmp_path),
        "%s/database.tmp",
        fixture_root
    );
    if (source_length < 0 || (size_t)source_length >= sizeof(source_dir) ||
        database_length < 0 || (size_t)database_length >= sizeof(database_path) ||
        temporary_length < 0 || (size_t)temporary_length >= sizeof(database_tmp_path)) {
        return fail_message("initialize fixture paths", "derived path is too long");
    }
    return 0;
}

static int create_fixture(void)
{
    char path[160];
    char line[128];

    if (mkdir(fixture_root, 0755) < 0 && errno != EEXIST) {
        return fail_errno("mkdir fixture root", fixture_root);
    }
    if (mkdir(source_dir, 0755) < 0 && errno != EEXIST) {
        return fail_errno("mkdir catalog source", source_dir);
    }
    unlink(database_path);
    unlink(database_tmp_path);

    for (int file_index = 0; file_index < CATALOG_FILE_COUNT; file_index++) {
        int path_length = snprintf(
            path,
            sizeof(path),
            "%s/catalog-%02d.catalog",
            source_dir,
            file_index
        );
        if (path_length < 0 || (size_t)path_length >= sizeof(path)) {
            errno = ENAMETOOLONG;
            return fail_errno("format catalog path", source_dir);
        }

        int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
        if (fd < 0) {
            return fail_errno("create catalog file", path);
        }

        size_t file_size = 0;
        for (unsigned line_index = 0; file_size < CATALOG_FILE_SIZE; line_index++) {
            int line_length = snprintf(
                line,
                sizeof(line),
                "catalog=%02d line=%06u payload=systemd-journal-catalog-update\n",
                file_index,
                line_index
            );
            if (line_length < 0 || (size_t)line_length >= sizeof(line)) {
                close(fd);
                errno = EOVERFLOW;
                return fail_errno("format catalog line", path);
            }

            size_t remaining = CATALOG_FILE_SIZE - file_size;
            size_t chunk = (size_t)line_length < remaining ? (size_t)line_length : remaining;
            if (write_all(fd, line, chunk) < 0) {
                close(fd);
                return fail_errno("write catalog file", path);
            }
            file_size += chunk;
        }
        if (close(fd) < 0) {
            return fail_errno("close catalog file", path);
        }
    }

    printf(
        "PASS: fixture contains %d catalog files and %d bytes\n",
        CATALOG_FILE_COUNT,
        CATALOG_FILE_COUNT * CATALOG_FILE_SIZE
    );
    return 0;
}

static int compare_names(const void *left, const void *right)
{
    const char *const *left_name = left;
    const char *const *right_name = right;

    return strcmp(*left_name, *right_name);
}

static int enumerate_catalogs(char *names[CATALOG_FILE_COUNT])
{
    DIR *directory = opendir(source_dir);
    if (directory == NULL) {
        return fail_errno("opendir catalog source", source_dir);
    }

    size_t count = 0;
    errno = 0;
    for (;;) {
        struct dirent *entry = readdir(directory);
        if (entry == NULL) {
            break;
        }

        size_t name_length = strlen(entry->d_name);
        if (name_length < sizeof(".catalog") - 1 ||
            strcmp(entry->d_name + name_length - (sizeof(".catalog") - 1), ".catalog") != 0) {
            continue;
        }
        if (count >= CATALOG_FILE_COUNT) {
            closedir(directory);
            return fail_message("enumerate catalog source", "too many catalog files");
        }

        names[count] = strdup(entry->d_name);
        if (names[count] == NULL) {
            closedir(directory);
            return fail_errno("duplicate catalog name", entry->d_name);
        }
        count++;
    }
    int saved_errno = errno;
    if (closedir(directory) < 0 && saved_errno == 0) {
        return fail_errno("closedir catalog source", source_dir);
    }
    if (saved_errno != 0) {
        errno = saved_errno;
        return fail_errno("readdir catalog source", source_dir);
    }
    if (count != CATALOG_FILE_COUNT) {
        return fail_message("enumerate catalog source", "catalog count mismatch");
    }

    qsort(names, count, sizeof(names[0]), compare_names);
    printf("PASS: enumerated and sorted %zu catalog files\n", count);
    return 0;
}

static int read_catalogs(char *const names[CATALOG_FILE_COUNT])
{
    int directory_fd = open(source_dir, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (directory_fd < 0) {
        return fail_errno("open catalog source directory", source_dir);
    }

    size_t total_bytes = 0;
    size_t total_lines = 0;
    for (size_t index = 0; index < CATALOG_FILE_COUNT; index++) {
        int source_fd = openat(directory_fd, names[index], O_RDONLY | O_CLOEXEC);
        if (source_fd < 0) {
            close(directory_fd);
            return fail_errno("open catalog file", names[index]);
        }

        char proc_fd_path[64];
        int path_length = snprintf(
            proc_fd_path,
            sizeof(proc_fd_path),
            "/proc/self/fd/%d",
            source_fd
        );
        if (path_length < 0 || (size_t)path_length >= sizeof(proc_fd_path)) {
            close(source_fd);
            close(directory_fd);
            errno = ENAMETOOLONG;
            return fail_errno("format procfd path", names[index]);
        }

        FILE *stream = fopen(proc_fd_path, "re");
        if (stream == NULL) {
            close(source_fd);
            close(directory_fd);
            return fail_errno("fopen procfd catalog", proc_fd_path);
        }

        char *line = NULL;
        size_t capacity = 0;
        ssize_t line_length;
        while ((line_length = getline(&line, &capacity, stream)) >= 0) {
            total_bytes += (size_t)line_length;
            total_lines++;
        }
        free(line);
        if (ferror(stream)) {
            fclose(stream);
            close(source_fd);
            close(directory_fd);
            return fail_errno("getline catalog", names[index]);
        }
        if (fclose(stream) < 0) {
            close(source_fd);
            close(directory_fd);
            return fail_errno("fclose catalog stream", names[index]);
        }
        if (close(source_fd) < 0) {
            close(directory_fd);
            return fail_errno("close catalog fd", names[index]);
        }
    }
    if (close(directory_fd) < 0) {
        return fail_errno("close catalog directory", source_dir);
    }
    if (total_bytes != (size_t)CATALOG_FILE_COUNT * CATALOG_FILE_SIZE) {
        return fail_message("read catalog files", "total byte count mismatch");
    }

    printf(
        "PASS: procfd buffered reads consumed %zu bytes across %zu lines\n",
        total_bytes,
        total_lines
    );
    return 0;
}

static int write_database(void)
{
    unsigned char *database = malloc(DATABASE_SIZE);
    if (database == NULL) {
        return fail_errno("allocate database buffer", database_tmp_path);
    }
    for (size_t index = 0; index < DATABASE_SIZE; index++) {
        database[index] = (unsigned char)(index % 251);
    }

    FILE *stream = fopen(database_tmp_path, "w+e");
    if (stream == NULL) {
        free(database);
        return fail_errno("open temporary database", database_tmp_path);
    }

    const size_t header_size = 32;
    const size_t item_size = 64 * 1024;
    const size_t strings_size = DATABASE_SIZE - header_size - item_size;
    if (fwrite(database, header_size, 1, stream) != 1 ||
        fwrite(database + header_size, item_size, 1, stream) != 1 ||
        fwrite(database + header_size + item_size, strings_size, 1, stream) != 1) {
        fclose(stream);
        free(database);
        errno = EIO;
        return fail_errno("write temporary database", database_tmp_path);
    }
    if (fflush(stream) < 0) {
        fclose(stream);
        free(database);
        return fail_errno("flush temporary database", database_tmp_path);
    }
    if (fchmod(fileno(stream), 0644) < 0) {
        fclose(stream);
        free(database);
        return fail_errno("chmod temporary database", database_tmp_path);
    }

    current_stage = STAGE_RENAME;
    if (rename(database_tmp_path, database_path) < 0) {
        fclose(stream);
        free(database);
        return fail_errno("rename database", database_path);
    }
    if (fclose(stream) < 0) {
        free(database);
        return fail_errno("close database", database_path);
    }
    free(database);

    struct stat metadata;
    if (stat(database_path, &metadata) < 0) {
        return fail_errno("stat database", database_path);
    }
    if (metadata.st_size != DATABASE_SIZE || (metadata.st_mode & 0777) != 0644) {
        return fail_message("validate database", "size or mode mismatch");
    }

    printf("PASS: wrote, flushed, chmodded, and renamed %d-byte database\n", DATABASE_SIZE);
    return 0;
}

static void free_names(char *names[CATALOG_FILE_COUNT])
{
    for (size_t index = 0; index < CATALOG_FILE_COUNT; index++) {
        free(names[index]);
    }
}

int main(int argc, char *argv[])
{
    char *names[CATALOG_FILE_COUNT] = {};
    struct sigaction action = {
        .sa_handler = timeout_handler,
    };

    setvbuf(stdout, NULL, _IONBF, 0);
    setvbuf(stderr, NULL, _IONBF, 0);
    if (argc > 2) {
        return fail_message("parse arguments", "expected at most one base directory");
    }
    if (initialize_paths(argc == 2 ? argv[1] : "/root") != 0) {
        return 1;
    }
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGALRM, &action, NULL) < 0) {
        return fail_errno("install timeout handler", "SIGALRM");
    }
    alarm(TEST_TIMEOUT_SECONDS);

    puts("STARRY_SYSTEM_TEST_BEGIN: bugfix-systemd-catalog-io");
    if (create_fixture() != 0) {
        return 1;
    }

    current_stage = STAGE_ENUMERATE;
    if (enumerate_catalogs(names) != 0) {
        free_names(names);
        return 1;
    }

    current_stage = STAGE_READ;
    if (read_catalogs(names) != 0) {
        free_names(names);
        return 1;
    }
    free_names(names);

    current_stage = STAGE_WRITE;
    if (write_database() != 0) {
        return 1;
    }

    alarm(0);
    puts("STARRY_GROUPED_TEST_PASSED: bugfix-systemd-catalog-io");
    return 0;
}
