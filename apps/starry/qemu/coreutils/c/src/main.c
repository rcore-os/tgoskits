/*
 * coreutils-test.c -- GNU coreutils 9.x validation for StarryOS
 *
 * Key dependencies: stat, readlink, chmod, chown, mkfifo, mknod
 * Acceptance criteria: ls -la /usr, cp -r, mv, rm -rf work correctly
 *
 * Note: BusyBox ash intercepts command names as built-in applets,
 * bypassing coreutils symlinks.  For commands that need strict
 * coreutils verification (e.g. stat -c), we use fork+exec+pipe
 * to bypass the shell entirely.
 */

#define _GNU_SOURCE
#define _POSIX_C_SOURCE 200809L

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/wait.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <errno.h>
#include <stdarg.h>
#include <sys/syscall.h>
#include <sys/sysmacros.h>

static int pass = 0, fail = 0, skip = 0;

/* Run a shell command via system(), return exit status (0-255) */
static int run(const char *cmd)
{
    int ret = system(cmd);
    if (WIFEXITED(ret))
        return WEXITSTATUS(ret);
    return -1;
}

/*
 * Capture first line of a command's stdout into buf via popen().
 * Returns length of captured line (>=0) or -1 on error.
 */
static int capture_first_line(const char *cmd, char *buf, int bufsz)
{
    FILE *p = popen(cmd, "r");
    if (!p) return -1;
    buf[0] = '\0';
    if (!fgets(buf, bufsz, p)) { pclose(p); return -1; }
    pclose(p);
    int len = (int)strlen(buf);
    while (len > 0 && (buf[len - 1] == '\n' || buf[len - 1] == '\r'))
        buf[--len] = '\0';
    return len;
}

/*
 * Execute a binary directly via fork+exec+pipe (bypasses BusyBox ash).
 * Captures the first line of stdout into buf.
 * Returns exit status (0-255) or -1 on error. Sets *outlen to captured length.
 */
static int exec_capture(const char *binary, char *buf, int bufsz, int *outlen, ...)
{
    va_list ap;
    va_start(ap, outlen);

    int pipefd[2];
    if (pipe(pipefd) < 0) { va_end(ap); return -1; }

    pid_t pid = fork();
    if (pid < 0) { close(pipefd[0]); close(pipefd[1]); va_end(ap); return -1; }

    if (pid == 0) {
        close(pipefd[0]);
        dup2(pipefd[1], STDOUT_FILENO);
        dup2(pipefd[1], STDERR_FILENO);
        close(pipefd[1]);

        /* Build argv: binary, variadic args, NULL */
        char *argv[16];
        int i = 0;
        argv[i++] = (char *)binary;
        char *arg;
        while ((arg = va_arg(ap, char *)) != NULL) {
            if (i < 15) argv[i++] = arg;
        }
        argv[i] = NULL;
        va_end(ap);

        execv(binary, argv);
        _exit(127);
    }
    va_end(ap);

    close(pipefd[1]);
    size_t total = 0;
    ssize_t n;
    while ((n = read(pipefd[0], buf + total, bufsz - 1 - total)) > 0) {
        total += (size_t)n;
        if (total >= (size_t)(bufsz - 1)) break;
    }
    close(pipefd[0]);
    buf[total] = '\0';

    /* Strip trailing newlines */
    while (total > 0 && (buf[total - 1] == '\n' || buf[total - 1] == '\r'))
        buf[--total] = '\0';

    int status;
    waitpid(pid, &status, 0);
    if (outlen) *outlen = (int)total;
    if (WIFEXITED(status)) return WEXITSTATUS(status);
    return -1;
}

/* Write data to a file, return 0 on success */
static int write_file(const char *path, const char *data)
{
    int fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd < 0) return -1;
    size_t len = strlen(data);
    ssize_t w = write(fd, data, len);
    close(fd);
    return (w == (ssize_t)len) ? 0 : -1;
}

/* Get file permission bits via C stat() */
static int get_mode(const char *path, mode_t *mode)
{
    struct stat st;
    if (stat(path, &st) < 0) return -1;
    *mode = st.st_mode & 07777;
    return 0;
}

/* Get file type string via C stat() */
static int get_file_type(const char *path, char *buf, int bufsz)
{
    struct stat st;
    if (stat(path, &st) < 0) return -1;
    const char *type;
    if (S_ISREG(st.st_mode))       type = "regular file";
    else if (S_ISDIR(st.st_mode))  type = "directory";
    else if (S_ISCHR(st.st_mode))  type = "character special file";
    else if (S_ISBLK(st.st_mode))  type = "block special file";
    else if (S_ISFIFO(st.st_mode)) type = "fifo";
    else if (S_ISLNK(st.st_mode))  type = "symbolic link";
    else type = "unknown";
    strncpy(buf, type, bufsz - 1);
    buf[bufsz - 1] = '\0';
    return 0;
}

/* Check owner uid/gid via C stat() */
static int get_owner(const char *path, uid_t *uid, gid_t *gid)
{
    struct stat st;
    if (stat(path, &st) < 0) return -1;
    *uid = st.st_uid;
    *gid = st.st_gid;
    return 0;
}

#define PASS(name) do { printf("  PASS | %s\n", name); pass++; } while(0)
#define FAIL(name, ...) do { printf("  FAIL | %s ", name); printf(__VA_ARGS__); printf("\n"); fail++; } while(0)
#define SKIP(name, ...) do { printf("  SKIP | %s ", name); printf(__VA_ARGS__); printf("\n"); skip++; } while(0)

/* ==================================================================
 *  Group 1: stat — /bin/stat is GNU coreutils (symlink → coreutils)
 *  Use fork+exec to bypass BusyBox ash applet interception.
 * ================================================================== */
static void test_stat(void)
{
    printf("[stat]\n");
    char buf[256];
    int len;
    int rc;

    /* 1. stat default format: output contains "File:" and "Size:" */
    rc = exec_capture("/bin/stat", buf, sizeof(buf), &len, "/etc/passwd", NULL);
    if (rc == 0 && len > 0 && strstr(buf, "File:") != NULL) {
        PASS("stat default format");
    } else {
        FAIL("stat default format", "(rc=%d len=%d output='%s')", rc, len, buf);
    }

    /* 2. stat -c '%s' returns file size (positive integer) */
    rc = exec_capture("/bin/stat", buf, sizeof(buf), &len, "-c", "%s", "/etc/passwd", NULL);
    if (rc == 0 && len > 0 && atoi(buf) > 0) {
        PASS("stat -c %%s size");
    } else {
        FAIL("stat -c %%s size", "(rc=%d len=%d output='%s')", rc, len, buf);
    }

    /* 3. stat -c '%F' returns "regular file" */
    rc = exec_capture("/bin/stat", buf, sizeof(buf), &len, "-c", "%F", "/etc/passwd", NULL);
    if (rc == 0 && len > 0 && strcmp(buf, "regular file") == 0) {
        PASS("stat -c %%F type");
    } else {
        FAIL("stat -c %%F type", "(rc=%d len=%d output='%s')", rc, len, buf);
    }

    /* 4. stat -c '%a' returns octal permissions */
    rc = exec_capture("/bin/stat", buf, sizeof(buf), &len, "-c", "%a", "/etc/passwd", NULL);
    if (rc == 0 && len > 0) {
        char *end;
        long val = strtol(buf, &end, 8);
        if (*end == '\0' && val > 0 && val <= 07777) {
            PASS("stat -c %%a perms");
        } else {
            FAIL("stat -c %%a perms", "(output='%s' not valid octal)", buf);
        }
    } else {
        FAIL("stat -c %%a perms", "(rc=%d len=%d output='%s')", rc, len, buf);
    }

    /* 5. stat on directory: -c '%F' returns "directory" */
    rc = exec_capture("/bin/stat", buf, sizeof(buf), &len, "-c", "%F", "/etc", NULL);
    if (rc == 0 && len > 0 && strcmp(buf, "directory") == 0) {
        PASS("stat -c %%F directory");
    } else {
        FAIL("stat -c %%F directory", "(rc=%d output='%s')", rc, buf);
    }
}

/* ==================================================================
 *  Group 2: readlink
 * ================================================================== */
static void test_readlink(void)
{
    printf("[readlink]\n");
    char buf[256];

    /* 1. Basic symlink read */
    {
        write_file("/tmp/cu_rlk_target", "hello");
        unlink("/tmp/cu_rlk_link");
        symlink("/tmp/cu_rlk_target", "/tmp/cu_rlk_link");

        int rc = capture_first_line("readlink /tmp/cu_rlk_link", buf, sizeof(buf));
        if (rc > 0 && strcmp(buf, "/tmp/cu_rlk_target") == 0) {
            PASS("readlink basic");
        } else {
            FAIL("readlink basic", "(rc=%d output='%s')", rc, buf);
        }
        unlink("/tmp/cu_rlk_link");
        unlink("/tmp/cu_rlk_target");
    }

    /* 2. readlink -f canonical path */
    {
        int rc = capture_first_line("readlink -f /etc/passwd", buf, sizeof(buf));
        if (rc > 0 && strcmp(buf, "/etc/passwd") == 0) {
            PASS("readlink -f canonical");
        } else {
            FAIL("readlink -f canonical", "(rc=%d output='%s')", rc, buf);
        }
    }

    /* 3. readlink on broken symlink: should return target even if target missing */
    {
        unlink("/tmp/cu_rlk_broken_link");
        unlink("/tmp/cu_rlk_no_such_target");
        symlink("/tmp/cu_rlk_no_such_target", "/tmp/cu_rlk_broken_link");

        int rc = capture_first_line("readlink /tmp/cu_rlk_broken_link", buf, sizeof(buf));
        if (rc > 0 && strcmp(buf, "/tmp/cu_rlk_no_such_target") == 0) {
            PASS("readlink broken symlink");
        } else {
            FAIL("readlink broken symlink", "(rc=%d output='%s')", rc, buf);
        }
        unlink("/tmp/cu_rlk_broken_link");
    }
}

/* ==================================================================
 *  Group 3: chmod
 * ================================================================== */
static void test_chmod(void)
{
    printf("[chmod]\n");
    mode_t mode;

    /* 1. chmod 0755 */
    {
        write_file("/tmp/cu_chmod_test", "x");
        run("chmod 0755 /tmp/cu_chmod_test");
        if (get_mode("/tmp/cu_chmod_test", &mode) == 0 && mode == 0755) {
            PASS("chmod 0755");
        } else {
            FAIL("chmod 0755", "(mode=%o)", mode);
        }
        unlink("/tmp/cu_chmod_test");
    }

    /* 2. chmod 0600 */
    {
        write_file("/tmp/cu_chmod_test", "x");
        run("chmod 0600 /tmp/cu_chmod_test");
        if (get_mode("/tmp/cu_chmod_test", &mode) == 0 && mode == 0600) {
            PASS("chmod 0600");
        } else {
            FAIL("chmod 0600", "(mode=%o)", mode);
        }
        unlink("/tmp/cu_chmod_test");
    }

    /* 3. chmod u+x (symbolic) */
    {
        write_file("/tmp/cu_chmod_test", "x");
        run("chmod 0644 /tmp/cu_chmod_test");
        run("chmod u+x /tmp/cu_chmod_test");
        if (get_mode("/tmp/cu_chmod_test", &mode) == 0 && mode == 0744) {
            PASS("chmod u+x symbolic");
        } else {
            FAIL("chmod u+x symbolic", "(mode=%o)", mode);
        }
        unlink("/tmp/cu_chmod_test");
    }

    /* 4. chmod go-rwx (symbolic) */
    {
        write_file("/tmp/cu_chmod_test", "x");
        run("chmod 0755 /tmp/cu_chmod_test");
        run("chmod go-rwx /tmp/cu_chmod_test");
        if (get_mode("/tmp/cu_chmod_test", &mode) == 0 && mode == 0700) {
            PASS("chmod go-rwx symbolic");
        } else {
            FAIL("chmod go-rwx symbolic", "(mode=%o)", mode);
        }
        unlink("/tmp/cu_chmod_test");
    }

    /* 5. chmod on directory */
    {
        run("rm -rf /tmp/cu_chmod_dir");
        run("mkdir /tmp/cu_chmod_dir");
        run("chmod 0755 /tmp/cu_chmod_dir");
        if (get_mode("/tmp/cu_chmod_dir", &mode) == 0 && mode == 0755) {
            PASS("chmod directory");
        } else {
            FAIL("chmod directory", "(mode=%o)", mode);
        }
        run("rm -rf /tmp/cu_chmod_dir");
    }
}

/* ==================================================================
 *  Group 4: chown
 * ================================================================== */
static void test_chown(void)
{
    printf("[chown]\n");
    uid_t uid;
    gid_t gid;

    /* 1. chown root:root (named) */
    {
        write_file("/tmp/cu_chown_test", "x");
        int rc = run("chown root:root /tmp/cu_chown_test");
        if (rc == 0) {
            if (get_owner("/tmp/cu_chown_test", &uid, &gid) == 0 &&
                uid == 0 && gid == 0) {
                PASS("chown root:root named");
            } else {
                FAIL("chown root:root named", "(uid=%d gid=%d)", uid, gid);
            }
        } else {
            FAIL("chown root:root named", "(rc=%d)", rc);
        }
        unlink("/tmp/cu_chown_test");
    }

    /* 2. chown 0:0 (numeric) */
    {
        write_file("/tmp/cu_chown_test", "x");
        int rc = run("chown 0:0 /tmp/cu_chown_test");
        if (rc == 0) {
            if (get_owner("/tmp/cu_chown_test", &uid, &gid) == 0 &&
                uid == 0 && gid == 0) {
                PASS("chown 0:0 numeric");
            } else {
                FAIL("chown 0:0 numeric", "(uid=%d gid=%d)", uid, gid);
            }
        } else {
            FAIL("chown 0:0 numeric", "(rc=%d)", rc);
        }
        unlink("/tmp/cu_chown_test");
    }

    /* 3. chown :root (group only) */
    {
        write_file("/tmp/cu_chown_test", "x");
        int rc = run("chown :root /tmp/cu_chown_test");
        if (rc == 0) {
            if (get_owner("/tmp/cu_chown_test", &uid, &gid) == 0 && gid == 0) {
                PASS("chown :root group-only");
            } else {
                FAIL("chown :root group-only", "(uid=%d gid=%d)", uid, gid);
            }
        } else {
            FAIL("chown :root group-only", "(rc=%d)", rc);
        }
        unlink("/tmp/cu_chown_test");
    }

    /* 4. chown clears setuid/setgid even when uid/gid unchanged */
    {
        write_file("/tmp/cu_chown_suid", "x");
        /* Set mode to 06755 (setuid + setgid + 0755) */
        chmod("/tmp/cu_chown_suid", 06755);
        /* chown root:root — uid/gid don't change, but Linux still clears
         * setuid and setgid because the parameters participate (not -1). */
        if (chown("/tmp/cu_chown_suid", 0, 0) == 0) {
            mode_t mode;
            if (get_mode("/tmp/cu_chown_suid", &mode) == 0 && mode == 0755) {
                PASS("chown clears setuid/setgid on same owner");
            } else {
                FAIL("chown clears setuid/setgid on same owner",
                     "(mode=%o expected 0755)", mode);
            }
        } else {
            FAIL("chown clears setuid/setgid on same owner",
                 "(chown errno=%d)", errno);
        }
        unlink("/tmp/cu_chown_suid");
    }

    /* 5. chown(-1, -1) clears setuid/setgid on regular file */
    {
        write_file("/tmp/cu_chown_neg1", "x");
        chmod("/tmp/cu_chown_neg1", 06755);
        /* chown(path, -1, -1) — neither parameter participates, but Linux
         * still clears setuid and setgid for non-directory files. */
        if (chown("/tmp/cu_chown_neg1", (uid_t)-1, (gid_t)-1) == 0) {
            mode_t mode;
            if (get_mode("/tmp/cu_chown_neg1", &mode) == 0 && mode == 0755) {
                PASS("chown(-1,-1) clears setuid/setgid");
            } else {
                FAIL("chown(-1,-1) clears setuid/setgid",
                     "(mode=%o expected 0755)", mode);
            }
        } else {
            FAIL("chown(-1,-1) clears setuid/setgid", "(errno=%d)", errno);
        }
        unlink("/tmp/cu_chown_neg1");
    }

    /* 6. chown(uid, -1) clears both setuid and setgid on regular file */
    {
        write_file("/tmp/cu_chown_uid", "x");
        chmod("/tmp/cu_chown_uid", 06755);
        /* uid participates, gid does not: setuid always cleared for non-dir;
         * setgid cleared because GROUP_EXEC is set (06755 has r-xr-xr-x). */
        if (chown("/tmp/cu_chown_uid", 0, (gid_t)-1) == 0) {
            mode_t mode;
            if (get_mode("/tmp/cu_chown_uid", &mode) == 0 && mode == 0755) {
                PASS("chown(uid,-1) clears setuid/setgid");
            } else {
                FAIL("chown(uid,-1) clears setuid/setgid",
                     "(mode=%o expected 0755)", mode);
            }
        } else {
            FAIL("chown(uid,-1) clears setuid/setgid", "(errno=%d)", errno);
        }
        unlink("/tmp/cu_chown_uid");
    }

    /* 7. chown(-1, gid) clears setuid on regular file (setgid not present) */
    {
        write_file("/tmp/cu_chown_gid", "x");
        chmod("/tmp/cu_chown_gid", 04755);  /* setuid only */
        /* gid participates, uid does not: setuid still cleared (non-dir
         * ATTR_KILL_SUID applies regardless of uid participation); no setgid
         * to clear since file started with 04755. */
        if (chown("/tmp/cu_chown_gid", (uid_t)-1, 0) == 0) {
            mode_t mode;
            if (get_mode("/tmp/cu_chown_gid", &mode) == 0 && mode == 0755) {
                PASS("chown(-1,gid) clears setuid/setgid");
            } else {
                FAIL("chown(-1,gid) clears setuid/setgid",
                     "(mode=%o expected 0755)", mode);
            }
        } else {
            FAIL("chown(-1,gid) clears setuid/setgid", "(errno=%d)", errno);
        }
        unlink("/tmp/cu_chown_gid");
    }

    /* 8. chown clears setuid/setgid on ext4/rootfs path */
    {
        write_file("/root/cu_chown_suid_ext4", "x");
        chmod("/root/cu_chown_suid_ext4", 06755);
        if (chown("/root/cu_chown_suid_ext4", 0, 0) == 0) {
            mode_t mode;
            if (get_mode("/root/cu_chown_suid_ext4", &mode) == 0 && mode == 0755) {
                PASS("chown clears setuid/setgid on ext4");
            } else {
                FAIL("chown clears setuid/setgid on ext4",
                     "(mode=%o expected 0755)", mode);
            }
        } else {
            FAIL("chown clears setuid/setgid on ext4", "(errno=%d)", errno);
        }
        unlink("/root/cu_chown_suid_ext4");
    }

    /* 9. chown(-1,-1) clears setuid/setgid on ext4/rootfs path */
    {
        write_file("/root/cu_chown_neg1_ext4", "x");
        chmod("/root/cu_chown_neg1_ext4", 06755);
        if (chown("/root/cu_chown_neg1_ext4", (uid_t)-1, (gid_t)-1) == 0) {
            mode_t mode;
            if (get_mode("/root/cu_chown_neg1_ext4", &mode) == 0 && mode == 0755) {
                PASS("chown(-1,-1) clears setuid/setgid on ext4");
            } else {
                FAIL("chown(-1,-1) clears setuid/setgid on ext4",
                     "(mode=%o expected 0755)", mode);
            }
        } else {
            FAIL("chown(-1,-1) clears setuid/setgid on ext4", "(errno=%d)", errno);
        }
        unlink("/root/cu_chown_neg1_ext4");
    }
}

/* ==================================================================
 *  Group 5: mkfifo
 * ================================================================== */
static void test_mkfifo(void)
{
    printf("[mkfifo]\n");
    char type[64];
    mode_t mode;

    /* 1. mkfifo creates a fifo */
    {
        unlink("/tmp/cu_fifo");
        int rc = run("mkfifo /tmp/cu_fifo");
        if (rc == 0) {
            if (get_file_type("/tmp/cu_fifo", type, sizeof(type)) == 0 &&
                strcmp(type, "fifo") == 0) {
                PASS("mkfifo basic");
            } else {
                FAIL("mkfifo basic", "(type='%s')", type);
            }
        } else {
            SKIP("mkfifo basic", "(rc=%d)", rc);
        }
        unlink("/tmp/cu_fifo");
    }

    /* 2. mkfifo with mode */
    {
        unlink("/tmp/cu_fifo");
        int rc = run("mkfifo -m 0620 /tmp/cu_fifo");
        if (rc == 0) {
            if (get_mode("/tmp/cu_fifo", &mode) == 0 && mode == 0620) {
                PASS("mkfifo -m 0620");
            } else {
                FAIL("mkfifo -m 0620", "(mode=%o)", mode);
            }
        } else {
            SKIP("mkfifo -m 0620", "(rc=%d)", rc);
        }
        unlink("/tmp/cu_fifo");
    }
}

/* ==================================================================
 *  Group 6: mknod
 * ================================================================== */
static void test_mknod(void)
{
    printf("[mknod]\n");
    char type[64];

    /* 1. mknod char device c 1 3 */
    {
        unlink("/tmp/cu_char");
        int rc = run("mknod /tmp/cu_char c 1 3 2>/dev/null");
        if (rc == 0) {
            if (get_file_type("/tmp/cu_char", type, sizeof(type)) == 0 &&
                strcmp(type, "character special file") == 0) {
                PASS("mknod char c 1 3");
            } else {
                FAIL("mknod char c 1 3", "(type='%s')", type);
            }
            unlink("/tmp/cu_char");
        } else {
            SKIP("mknod char c 1 3", "(rc=%d)", rc);
        }
    }

    /* 2. mknod block device b 7 0 */
    {
        unlink("/tmp/cu_blk");
        int rc = run("mknod /tmp/cu_blk b 7 0 2>/dev/null");
        if (rc == 0) {
            if (get_file_type("/tmp/cu_blk", type, sizeof(type)) == 0 &&
                strcmp(type, "block special file") == 0) {
                PASS("mknod block b 7 0");
            } else {
                FAIL("mknod block b 7 0", "(type='%s')", type);
            }
            unlink("/tmp/cu_blk");
        } else {
            SKIP("mknod block b 7 0", "(rc=%d)", rc);
        }
    }

    /* 3. mknod char device rdev (major:minor) via C mknod() */
    {
        unlink("/tmp/cu_char_rdev");
        /* mknod(path, S_IFCHR | 0600, makedev(1, 3)) */
        if (mknod("/tmp/cu_char_rdev", S_IFCHR | 0600, makedev(1, 3)) == 0) {
            struct stat st;
            if (stat("/tmp/cu_char_rdev", &st) == 0 &&
                S_ISCHR(st.st_mode) &&
                major(st.st_rdev) == 1 && minor(st.st_rdev) == 3) {
                PASS("mknod char rdev major:minor");
            } else {
                FAIL("mknod char rdev major:minor",
                     "(mode=%o rdev=%lu major=%u minor=%u)",
                     st.st_mode, (unsigned long)st.st_rdev,
                     major(st.st_rdev), minor(st.st_rdev));
            }
            unlink("/tmp/cu_char_rdev");
        } else {
            SKIP("mknod char rdev major:minor", "(errno=%d)", errno);
        }
    }

    /* 4. mknod S_IFDIR returns EPERM */
    {
        unlink("/tmp/cu_mknod_dir");
        if (mknod("/tmp/cu_mknod_dir", S_IFDIR | 0755, 0) == 0) {
            FAIL("mknod S_IFDIR EPERM", "(unexpected success)");
            unlink("/tmp/cu_mknod_dir");
        } else if (errno == EPERM) {
            PASS("mknod S_IFDIR EPERM");
        } else {
            FAIL("mknod S_IFDIR EPERM", "(errno=%d, expected EPERM=%d)", errno, EPERM);
        }
    }

    /* 5. mknod char device rdev on ext4/rootfs */
    {
        unlink("/root/cu_char_rdev_ext4");
        if (mknod("/root/cu_char_rdev_ext4", S_IFCHR | 0600, makedev(1, 3)) == 0) {
            struct stat st;
            if (stat("/root/cu_char_rdev_ext4", &st) == 0 &&
                S_ISCHR(st.st_mode) &&
                major(st.st_rdev) == 1 && minor(st.st_rdev) == 3) {
                PASS("mknod char rdev on ext4");
            } else {
                FAIL("mknod char rdev on ext4",
                     "(mode=%o rdev=%lu major=%u minor=%u)",
                     st.st_mode, (unsigned long)st.st_rdev,
                     major(st.st_rdev), minor(st.st_rdev));
            }
            unlink("/root/cu_char_rdev_ext4");
        } else {
            SKIP("mknod char rdev on ext4", "(errno=%d)", errno);
        }
    }

    /* 6. mknod block device rdev on ext4/rootfs */
    {
        unlink("/root/cu_blk_rdev_ext4");
        if (mknod("/root/cu_blk_rdev_ext4", S_IFBLK | 0600, makedev(7, 0)) == 0) {
            struct stat st;
            if (stat("/root/cu_blk_rdev_ext4", &st) == 0 &&
                S_ISBLK(st.st_mode) &&
                major(st.st_rdev) == 7 && minor(st.st_rdev) == 0) {
                PASS("mknod block rdev on ext4");
            } else {
                FAIL("mknod block rdev on ext4",
                     "(mode=%o rdev=%lu major=%u minor=%u)",
                     st.st_mode, (unsigned long)st.st_rdev,
                     major(st.st_rdev), minor(st.st_rdev));
            }
            unlink("/root/cu_blk_rdev_ext4");
        } else {
            SKIP("mknod block rdev on ext4", "(errno=%d)", errno);
        }
    }

    /*
     * 7-10. ext4 rdev old/new encode/decode regression
     *
     * ext4 stores device numbers in i_block[0..1] using two formats:
     *   old (u16): major < 256 && minor < 256
     *   new (u32): otherwise, i_block[0]=0, i_block[1]=new_encode_dev
     *
     * These tests verify Starry reads back correct major:minor after a
     * create-then-stat round-trip on ext4, covering the old/new boundary.
     */
    {
        static const struct { const char *name; int maj, min; int type; } cases[] = {
            /* 7 */ { "ext4 rdev old boundary (255,255)", 255, 255, S_IFCHR },
            /* 8 */ { "ext4 rdev new minor>=256 (1,256)",   1, 256, S_IFCHR },
            /* 9 */ { "ext4 rdev new large minor (1,1040)", 1,1040, S_IFCHR },
            /*10 */ { "ext4 rdev new blk (8,256)",           8, 256, S_IFBLK },
        };
        for (int i = 0; i < (int)(sizeof(cases)/sizeof(cases[0])); i++) {
            char path[128];
            snprintf(path, sizeof(path), "/root/cu_rdev_%d_%d", cases[i].maj, cases[i].min);
            unlink(path);
            if (mknod(path, cases[i].type | 0600, makedev(cases[i].maj, cases[i].min)) == 0) {
                struct stat st;
                if (stat(path, &st) == 0 &&
                    major(st.st_rdev) == (unsigned)cases[i].maj &&
                    minor(st.st_rdev) == (unsigned)cases[i].min) {
                    PASS(cases[i].name);
                } else {
                    FAIL(cases[i].name,
                         "(rdev=%lu got major=%u minor=%u, want %d:%d)",
                         (unsigned long)st.st_rdev,
                         major(st.st_rdev), minor(st.st_rdev),
                         cases[i].maj, cases[i].min);
                }
                unlink(path);
            } else {
                SKIP(cases[i].name, "(errno=%d)", errno);
            }
        }
    }
}

/* ==================================================================
 *  Group 7: ls -la /usr
 * ================================================================== */
static void test_ls(void)
{
    printf("[ls]\n");
    char buf[4096];

    /* 1. ls -la /usr output contains "total" */
    {
        int len = capture_first_line("ls -la /usr", buf, sizeof(buf));
        if (len > 0 && strstr(buf, "total") != NULL) {
            PASS("ls -la /usr header");
        } else {
            FAIL("ls -la /usr header", "(output='%s')", buf);
        }
    }

    /* 2. ls /usr contains known directory entries */
    {
        int rc = run("ls /usr | grep -q -E '^(bin|lib|share|sbin)$'");
        if (rc == 0) {
            PASS("ls /usr entries");
        } else {
            FAIL("ls /usr entries", "(rc=%d)", rc);
        }
    }

    /* 3. ls -la permission format (drwxr-xr-x or similar) */
    {
        int rc = run("ls -la /usr | grep -q '^d'");
        if (rc == 0) {
            PASS("ls -la permission format");
        } else {
            FAIL("ls -la permission format", "(rc=%d)", rc);
        }
    }

    /* 4. ls -la shows owner and group columns ("root" appears) */
    {
        int rc = run("ls -la /usr | grep -q 'root'");
        if (rc == 0) {
            PASS("ls -la owner/group columns");
        } else {
            FAIL("ls -la owner/group columns", "(rc=%d)", rc);
        }
    }
}

/* ==================================================================
 *  Group 8: cp
 * ================================================================== */
static void test_cp(void)
{
    printf("[cp]\n");

    /* 1. cp single file */
    {
        write_file("/tmp/cu_cp_single_src", "single");
        unlink("/tmp/cu_cp_single_dst");
        int rc = run("cp /tmp/cu_cp_single_src /tmp/cu_cp_single_dst");
        if (rc == 0) {
            char buf[64];
            capture_first_line("cat /tmp/cu_cp_single_dst", buf, sizeof(buf));
            if (strcmp(buf, "single") == 0) {
                PASS("cp single file");
            } else {
                FAIL("cp single file", "(content='%s')", buf);
            }
        } else {
            FAIL("cp single file", "(rc=%d)", rc);
        }
        unlink("/tmp/cu_cp_single_src");
        unlink("/tmp/cu_cp_single_dst");
    }

    /* 2. cp -r directory tree */
    {
        run("rm -rf /tmp/cu_cp_src /tmp/cu_cp_dst");
        run("mkdir -p /tmp/cu_cp_src/sub");
        write_file("/tmp/cu_cp_src/a.txt", "aaa\n");
        write_file("/tmp/cu_cp_src/sub/b.txt", "bbb\n");

        int rc = run("cp -r /tmp/cu_cp_src /tmp/cu_cp_dst");
        if (rc == 0) {
            if (access("/tmp/cu_cp_dst/a.txt", F_OK) == 0 &&
                access("/tmp/cu_cp_dst/sub/b.txt", F_OK) == 0) {
                PASS("cp -r directory tree");
            } else {
                FAIL("cp -r directory tree", "(files missing)");
            }
        } else {
            FAIL("cp -r directory tree", "(rc=%d)", rc);
        }

        /* 3. cp -r preserves file content */
        {
            char buf[64];
            int len = capture_first_line("cat /tmp/cu_cp_dst/a.txt", buf, sizeof(buf));
            if (len > 0 && strcmp(buf, "aaa") == 0) {
                PASS("cp -r content preserved");
            } else {
                FAIL("cp -r content preserved", "(output='%s')", buf);
            }
        }

        run("rm -rf /tmp/cu_cp_src /tmp/cu_cp_dst");
    }
}

/* ==================================================================
 *  Group 9: mv
 * ================================================================== */
static void test_mv(void)
{
    printf("[mv]\n");

    /* 1. mv file */
    {
        write_file("/tmp/cu_mv_src", "move_me");
        unlink("/tmp/cu_mv_dst");
        int rc = run("mv /tmp/cu_mv_src /tmp/cu_mv_dst");
        if (rc == 0) {
            if (access("/tmp/cu_mv_src", F_OK) != 0 &&
                access("/tmp/cu_mv_dst", F_OK) == 0) {
                char buf[64];
                capture_first_line("cat /tmp/cu_mv_dst", buf, sizeof(buf));
                if (strcmp(buf, "move_me") == 0) {
                    PASS("mv file");
                } else {
                    FAIL("mv file", "(content='%s')", buf);
                }
            } else {
                FAIL("mv file", "(src exists=%d dst exists=%d)",
                     access("/tmp/cu_mv_src", F_OK) == 0,
                     access("/tmp/cu_mv_dst", F_OK) == 0);
            }
        } else {
            FAIL("mv file", "(rc=%d)", rc);
        }
        unlink("/tmp/cu_mv_src");
        unlink("/tmp/cu_mv_dst");
    }

    /* 2. mv directory */
    {
        run("rm -rf /tmp/cu_mv_dir_src /tmp/cu_mv_dir_dst");
        run("mkdir -p /tmp/cu_mv_dir_src");
        write_file("/tmp/cu_mv_dir_src/f.txt", "dir_move");
        int rc = run("mv /tmp/cu_mv_dir_src /tmp/cu_mv_dir_dst");
        if (rc == 0) {
            if (access("/tmp/cu_mv_dir_src", F_OK) != 0 &&
                access("/tmp/cu_mv_dir_dst/f.txt", F_OK) == 0) {
                PASS("mv directory");
            } else {
                FAIL("mv directory", "(src exists=%d dst_file exists=%d)",
                     access("/tmp/cu_mv_dir_src", F_OK) == 0,
                     access("/tmp/cu_mv_dir_dst/f.txt", F_OK) == 0);
            }
        } else {
            FAIL("mv directory", "(rc=%d)", rc);
        }
        run("rm -rf /tmp/cu_mv_dir_src /tmp/cu_mv_dir_dst");
    }
}

/* ==================================================================
 *  Group 10: rm -rf
 * ================================================================== */
static void test_rm(void)
{
    printf("[rm]\n");

    /* 1. rm -rf directory tree */
    {
        run("mkdir -p /tmp/cu_rm_dir/sub1/sub2");
        write_file("/tmp/cu_rm_dir/a.txt", "x");
        write_file("/tmp/cu_rm_dir/sub1/b.txt", "y");
        int rc = run("rm -rf /tmp/cu_rm_dir");
        if (rc == 0 && access("/tmp/cu_rm_dir", F_OK) != 0) {
            PASS("rm -rf directory tree");
        } else {
            FAIL("rm -rf directory tree", "(rc=%d exists=%d)",
                 rc, access("/tmp/cu_rm_dir", F_OK) == 0);
        }
    }

    /* 2. rm -f single file */
    {
        write_file("/tmp/cu_rm_file", "x");
        int rc = run("rm -f /tmp/cu_rm_file");
        if (rc == 0 && access("/tmp/cu_rm_file", F_OK) != 0) {
            PASS("rm -f single file");
        } else {
            FAIL("rm -f single file", "(rc=%d exists=%d)",
                 rc, access("/tmp/cu_rm_file", F_OK) == 0);
        }
    }

    /* 3. rm -f on non-existent file (should succeed silently) */
    {
        unlink("/tmp/cu_rm_noexist");
        int rc = run("rm -f /tmp/cu_rm_noexist");
        if (rc == 0) {
            PASS("rm -f non-existent");
        } else {
            FAIL("rm -f non-existent", "(rc=%d)", rc);
        }
    }
}

/* ==================================================================
 *  Main
 * ================================================================== */
int main(void)
{
    printf("=== GNU coreutils 9.x test ===\n");

    test_stat();
    test_readlink();
    test_chmod();
    test_chown();
    test_mkfifo();
    test_mknod();
    test_ls();
    test_cp();
    test_mv();
    test_rm();

    printf("=== total: %d passed, %d failed, %d skipped ===\n", pass, fail, skip);

    if (fail > 0) return 1;
    printf("COREUTILS TEST PASSED\n");
    return 0;
}
