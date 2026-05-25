#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define M_DIR     OF_MOD("create")
#define M_NEW     M_DIR "/new"
#define M_EXIST   M_DIR "/exist"
#define M_SUBDIR  M_DIR "/sub"
#define M_SYM     M_DIR "/sym"

/* O_CREAT 全行为。
 *
 * man 2 open §"O_CREAT" 原文（节选）：
 *   "O_CREAT — If pathname does not exist, create it as a regular file."
 *   "The mode argument specifies the file mode bits to be applied when a
 *    new file is created. If neither O_CREAT nor O_TMPFILE is specified,
 *    then mode is ignored. The effective mode is modified by the process's
 *    umask in the usual way: in the absence of a default ACL, the mode of
 *    the created file is (mode & ~umask)."
 *
 * man 2 open §"ENOENT" 原文：
 *   "ENOENT — A directory component in pathname does not exist or is a
 *    dangling symbolic link."
 *
 * man 2 open §"EISDIR" 原文：
 *   "EISDIR — pathname refers to a directory and the access requested
 *    involved writing (that is, O_WRONLY or O_RDWR is set)."
 *
 * 7 子矩阵：absent / present-no-EXCL / no-mode-arg-no-CREAT / through-symlink /
 *           missing-parent → ENOENT / dir-target+WRONLY → EISDIR / no-TRUNC. */
static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                      "create setup: mkdir");
    CHECK(ensure_dir(M_SUBDIR) == 0,                                   "create setup: subdir");
    CHECK(write_file(M_EXIST, "old", 3, 0644) == 0,                    "create setup: existing file");
    CHECK(symlink(M_EXIST, M_SYM) == 0,                                "create setup: symlink");
    umask(0);
}

static void create_when_absent(void)
{
    unlink(M_NEW);
    /* 怎么测：O_CREAT 不存在的路径
     * 期望：fd>=0；文件创建出来；初始内容为空；mode == 0644 */
    int fd = open(M_NEW, O_CREAT | O_WRONLY, 0644);
    CHECK(fd >= 0,                                                     "create absent: open(O_CREAT|O_WRONLY) ok");
    if (fd >= 0) close(fd);
    CHECK(is_regular_file(M_NEW),                                      "create absent: file now exists");
    CHECK(get_file_size(M_NEW) == 0,                                   "create absent: size==0");
    CHECK((get_file_mode(M_NEW) & 0777) == 0644,                       "create absent: mode==0644");
    unlink(M_NEW);
}

static void create_when_present_no_excl(void)
{
    /* 怎么测：O_CREAT 已存在文件，无 O_EXCL
     * 期望：fd>=0；旧内容保留；mode 不变
     * 为什么：O_CREAT 在文件已存在时退化为打开 */
    int fd = open(M_EXIST, O_CREAT | O_RDWR, 0600);
    CHECK(fd >= 0,                                                     "create present: open(O_CREAT|O_RDWR) ok");
    if (fd >= 0) close(fd);
    char buf[8] = {0};
    ssize_t n = read_file(M_EXIST, buf, sizeof(buf));
    CHECK(n == 3 && memcmp(buf, "old", 3) == 0,                        "create present: old content preserved");
    /* mode 不应被 0600 覆盖（仅创建时使用）*/
    CHECK((get_file_mode(M_EXIST) & 0777) == 0644,                     "create present: mode unchanged");
}

static void create_no_mode_arg_when_no_o_creat(void)
{
    /* 怎么测：不带 O_CREAT 的 open，省略 mode 参数
     * 期望：fd>=0
     * 为什么：man「If neither O_CREAT nor O_TMPFILE is specified, then mode is
     *         ignored」—— 不传 mode 也不影响 */
    int fd = open(M_EXIST, O_RDONLY);
    CHECK(fd >= 0,                                                     "create no-mode arg: open RDONLY ok");
    if (fd >= 0) close(fd);
}

static void create_through_symlink(void)
{
    /* 怎么测：O_CREAT 一个指向已存在文件的 symlink（无 O_EXCL）
     * 期望：fd>=0；fd 指向 symlink 的目标，原文件内容不变
     * 为什么：O_CREAT 不阻止跟随 symlink；O_EXCL+O_CREAT 才阻止 */
    int fd = open(M_SYM, O_CREAT | O_RDONLY, 0644);
    CHECK(fd >= 0,                                                     "create via symlink: O_CREAT|O_RDONLY ok");
    if (fd >= 0) close(fd);
    CHECK(is_regular_file(M_EXIST),                                    "create via symlink: target still file");
}

static void create_in_directory_path_segment_fail(void)
{
    /* 怎么测：O_CREAT，路径中间组件不存在
     * 期望：-1, ENOENT
     * 为什么：man ENOENT「pathname 中某个目录组件不存在」 */
    errno = 0;
    int fd = open(M_DIR "/no_such_dir/file", O_CREAT | O_WRONLY, 0644);
    CHECK(fd == -1 && errno == ENOENT,                                 "create with missing parent -> ENOENT");
    if (fd >= 0) close(fd);
}

static void create_at_directory_target(void)
{
    /* 怎么测：O_CREAT 已存在的目录路径，写模式
     * 期望：-1, EISDIR
     * 为什么：路径解析到目录后，open 写打开会触发 EISDIR */
    errno = 0;
    int fd = open(M_SUBDIR, O_CREAT | O_WRONLY, 0644);
    CHECK(fd == -1 && errno == EISDIR,                                 "create at dir + O_WRONLY -> EISDIR");
    if (fd >= 0) close(fd);
}

static void create_does_not_truncate_existing(void)
{
    /* 怎么测：O_CREAT 已有内容文件，无 O_TRUNC
     * 期望：内容保留
     * 为什么：O_CREAT 与 O_TRUNC 是独立 flag */
    write_file(M_EXIST, "keepme", 6, 0644);
    int fd = open(M_EXIST, O_CREAT | O_RDWR, 0644);
    CHECK(fd >= 0,                                                     "create no-trunc: open ok");
    if (fd >= 0) close(fd);
    CHECK(get_file_size(M_EXIST) == 6,                                 "create no-trunc: size unchanged");
}

int open_create_run(void)
{
    printf("\n----- open_create -----\n");
    mod_setup();
    create_when_absent();
    create_when_present_no_excl();
    create_no_mode_arg_when_no_o_creat();
    create_through_symlink();
    create_in_directory_path_segment_fail();
    create_at_directory_target();
    create_does_not_truncate_existing();
    cleanup_tree(M_DIR);
    printf("  ----- open_create: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
