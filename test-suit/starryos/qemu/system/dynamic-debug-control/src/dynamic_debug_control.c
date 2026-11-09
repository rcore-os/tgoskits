/*
 * Dynamic debug control writes, from one writer and from concurrent writers.
 *
 * Changing the print flag of a dynamic debug callsite patches kernel text on
 * every CPU while the control file lock is held. A write must change the flags
 * that the control file reports for the callsite. Two threads toggling the
 * same callsite at the same time must both finish every write: on SMP one of
 * them waits for the control file lock while the other one stops all CPUs to
 * patch the text.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define CONTROL "/proc/dynamic_debug/control"
#define CONCURRENT_WRITERS 2
#define TOGGLES_PER_WRITER 128

static char site[512];
static char enable_cmd[600];
static char disable_cmd[600];
static pthread_barrier_t start;

struct writer {
    int completed;
    int failed_errno;
};

static char *read_control(void) {
    int fd = open(CONTROL, O_RDONLY);
    if (fd < 0) {
        printf("open %s: %s\n", CONTROL, strerror(errno));
        return NULL;
    }
    size_t cap = 1 << 16, len = 0;
    char *buf = malloc(cap + 1);
    for (;;) {
        if (buf == NULL) {
            close(fd);
            printf("out of memory reading %s\n", CONTROL);
            return NULL;
        }
        ssize_t n = read(fd, buf + len, cap - len);
        if (n < 0) {
            printf("read %s: %s\n", CONTROL, strerror(errno));
            free(buf);
            close(fd);
            return NULL;
        }
        if (n == 0) {
            break;
        }
        len += (size_t)n;
        if (len == cap) {
            cap *= 2;
            buf = realloc(buf, cap + 1);
        }
    }
    close(fd);
    buf[len] = '\0';
    return buf;
}

/* Selects the first callsite and builds "file <path> line <n> +p/-p". */
static int choose_site(void) {
    char *text = read_control();
    if (text == NULL) {
        return -1;
    }
    char *line = text;
    while (*line == '#') {
        line = strchr(line, '\n');
        if (line == NULL) {
            break;
        }
        line++;
    }
    char *space = line ? strchr(line, ' ') : NULL;
    char *colon = space ? memrchr(line, ':', (size_t)(space - line)) : NULL;
    if (colon == NULL) {
        printf("no dynamic debug callsite in %s\n", CONTROL);
        free(text);
        return -1;
    }
    snprintf(site, sizeof site, "%.*s", (int)(space - line), line);
    snprintf(enable_cmd, sizeof enable_cmd, "file %.*s line %.*s +p", (int)(colon - line), line,
             (int)(space - colon - 1), colon + 1);
    snprintf(disable_cmd, sizeof disable_cmd, "file %.*s line %.*s -p", (int)(colon - line), line,
             (int)(space - colon - 1), colon + 1);
    free(text);
    return 0;
}

/* Returns 1 if the callsite reports the p flag, 0 if not, -1 on error. */
static int site_prints(void) {
    char *text = read_control();
    if (text == NULL) {
        return -1;
    }
    size_t site_len = strlen(site);
    int result = -1;
    for (char *line = text; line != NULL && *line != '\0';) {
        char *next = strchr(line, '\n');
        if (strncmp(line, site, site_len) == 0 && line[site_len] == ' ') {
            /* "<file>:<line> [module]function flags "format"" */
            char flags[32];
            if (sscanf(line + site_len, " %*s %31s", flags) == 1) {
                result = strchr(flags, 'p') != NULL;
            }
            break;
        }
        line = next ? next + 1 : NULL;
    }
    if (result < 0) {
        printf("callsite %s missing from %s\n", site, CONTROL);
    }
    free(text);
    return result;
}

static int write_command(int fd, const char *cmd) {
    size_t len = strlen(cmd);
    ssize_t n = write(fd, cmd, len);
    if (n != (ssize_t)len) {
        return n < 0 ? errno : EIO;
    }
    return 0;
}

static int write_once(const char *cmd) {
    int fd = open(CONTROL, O_WRONLY);
    if (fd < 0) {
        return errno;
    }
    int err = write_command(fd, cmd);
    close(fd);
    return err;
}

static void *toggle_site(void *arg) {
    struct writer *w = arg;
    int fd = open(CONTROL, O_WRONLY);
    w->failed_errno = fd < 0 ? errno : 0;
    pthread_barrier_wait(&start);
    if (fd < 0) {
        return NULL;
    }
    for (int i = 0; i < TOGGLES_PER_WRITER; i++) {
        int err = write_command(fd, enable_cmd);
        if (err == 0) {
            err = write_command(fd, disable_cmd);
        }
        if (err != 0) {
            w->failed_errno = err;
            break;
        }
        w->completed++;
    }
    close(fd);
    return NULL;
}

static int expect_flag(const char *cmd, int expected) {
    int err = write_once(cmd);
    if (err != 0) {
        printf("write \"%s\": %s\n", cmd, strerror(err));
        return 1;
    }
    int prints = site_prints();
    if (prints != expected) {
        printf("after \"%s\": callsite %s print flag %d, expected %d\n", cmd, site, prints,
               expected);
        return 1;
    }
    return 0;
}

int main(void) {
    int failures = 0;

    if (choose_site() != 0) {
        return 1;
    }
    failures += expect_flag(enable_cmd, 1);
    failures += expect_flag(disable_cmd, 0);

    struct writer writers[CONCURRENT_WRITERS] = {0};
    pthread_t threads[CONCURRENT_WRITERS];
    pthread_barrier_init(&start, NULL, CONCURRENT_WRITERS);
    for (int i = 0; i < CONCURRENT_WRITERS; i++) {
        if (pthread_create(&threads[i], NULL, toggle_site, &writers[i]) != 0) {
            printf("pthread_create failed\n");
            return 1;
        }
    }
    for (int i = 0; i < CONCURRENT_WRITERS; i++) {
        pthread_join(threads[i], NULL);
        if (writers[i].completed != TOGGLES_PER_WRITER) {
            printf("writer %d finished %d of %d toggles: %s\n", i, writers[i].completed,
                   TOGGLES_PER_WRITER, strerror(writers[i].failed_errno));
            failures++;
        }
    }
    pthread_barrier_destroy(&start);
    failures += expect_flag(disable_cmd, 0);

    if (failures != 0) {
        return 1;
    }
    printf("STARRY_DYNAMIC_DEBUG_CONTROL_OK\n");
    return 0;
}
