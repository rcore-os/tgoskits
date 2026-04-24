/*
 * grep-sed-awk-test.c -- grep 4.x / sed 4.x / awk 5.x (gawk) 联合验证
 *
 * 关键依赖: regex, mmap, pipe
 * 验收标准:
 *   - grep -r "pattern" /etc
 *   - sed -i 基本替换
 *   - awk 基本字段处理 + pattern action
 *   - 三者管道联合: grep | sed | awk
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/wait.h>
#include <sys/stat.h>
#include <fcntl.h>

static int run(const char *cmd)
{
    int ret = system(cmd);
    if (WIFEXITED(ret))
        return WEXITSTATUS(ret);
    return -1;
}

/* 读文件首行到 buf, 返回长度 */
static int read_first_line(const char *path, char *buf, int bufsz)
{
    FILE *f = fopen(path, "r");
    if (!f) return -1;
    if (!fgets(buf, bufsz, f)) { fclose(f); return -1; }
    int len = (int)strlen(buf);
    while (len > 0 && (buf[len - 1] == '\n' || buf[len - 1] == '\r'))
        buf[--len] = '\0';
    fclose(f);
    return len;
}

/* 写文件 */
static int write_file(const char *path, const char *data)
{
    int fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd < 0) return -1;
    size_t len = strlen(data);
    ssize_t w = write(fd, data, len);
    close(fd);
    return (w == (ssize_t)len) ? 0 : -1;
}

int main(void)
{
    int pass = 0, fail = 0;
    const char *tmp = "/tmp/gsa_test.txt";

    printf("=== grep/sed/awk combined test ===\n");

    /* ================================================================
     *  grep 4.x
     * ================================================================ */

    /* 1. pipe grep: 匹配 → exit 0 */
    {
        int rc = run("echo 'hello world' | grep -q 'hello'");
        if (rc == 0) { printf("  PASS | grep pipe match\n"); pass++; }
        else         { printf("  FAIL | grep pipe match (rc=%d)\n", rc); fail++; }
    }

    /* 2. pipe grep: 不匹配 → exit 1 */
    {
        int rc = run("echo 'foo bar' | grep -q 'baz'");
        if (rc == 1) { printf("  PASS | grep pipe no-match\n"); pass++; }
        else         { printf("  FAIL | grep pipe no-match (rc=%d)\n", rc); fail++; }
    }

    /* 3. grep -r 递归搜索 /etc */
    {
        int rc = run("grep -rq 'root' /etc");
        if (rc == 0) { printf("  PASS | grep -r 'root' /etc\n"); pass++; }
        else         { printf("  FAIL | grep -r 'root' /etc (rc=%d)\n", rc); fail++; }
    }

    /* 4. grep -r 不存在的字符串 → exit 1 */
    {
        int rc = run("grep -rq 'ZZZ_NO_SUCH_12345' /etc 2>/dev/null");
        if (rc == 1) { printf("  PASS | grep -r no-match\n"); pass++; }
        else         { printf("  FAIL | grep -r no-match (rc=%d)\n", rc); fail++; }
    }

    /* 5. grep regex */
    {
        int rc = run("echo 'abc123' | grep -q '[0-9]\\{3\\}'");
        if (rc == 0) { printf("  PASS | grep regex\n"); pass++; }
        else         { printf("  FAIL | grep regex (rc=%d)\n", rc); fail++; }
    }

    /* 6. grep -i 忽略大小写 */
    {
        int rc = run("echo 'Hello' | grep -qi 'hello'");
        if (rc == 0) { printf("  PASS | grep -i\n"); pass++; }
        else         { printf("  FAIL | grep -i (rc=%d)\n", rc); fail++; }
    }

    /* 7. grep -v 反向匹配 */
    {
        int rc = run("echo -e 'aaa\\nbbb' | grep -v 'aaa' | grep -q 'bbb'");
        if (rc == 0) { printf("  PASS | grep -v\n"); pass++; }
        else         { printf("  FAIL | grep -v (rc=%d)\n", rc); fail++; }
    }

    /* ================================================================
     *  sed 4.x
     * ================================================================ */

    /* 8. pipe sed s/// 替换 */
    {
        int rc = run("echo 'hello world' | sed 's/hello/hi/' | grep -q '^hi world$'");
        if (rc == 0) { printf("  PASS | sed pipe s///\n"); pass++; }
        else         { printf("  FAIL | sed pipe s/// (rc=%d)\n", rc); fail++; }
    }

    /* 9. pipe sed s///g 全局替换 */
    {
        int rc = run("echo 'aaa bbb aaa' | sed 's/aaa/xxx/g' | grep -q '^xxx bbb xxx$'");
        if (rc == 0) { printf("  PASS | sed s///g\n"); pass++; }
        else         { printf("  FAIL | sed s///g (rc=%d)\n", rc); fail++; }
    }

    /* 10. sed -i 基本替换 (验收标准) */
    {
        write_file(tmp, "foo bar baz\n");
        int rc = run("sed -i 's/bar/replaced/' /tmp/gsa_test.txt");
        if (rc == 0) {
            char buf[256] = {0};
            read_first_line(tmp, buf, sizeof(buf));
            if (strcmp(buf, "foo replaced baz") == 0) {
                printf("  PASS | sed -i s///\n"); pass++;
            } else {
                printf("  FAIL | sed -i s/// (got '%s')\n", buf); fail++;
            }
        } else {
            printf("  FAIL | sed -i s/// (rc=%d)\n", rc); fail++;
        }
        unlink(tmp);
    }

    /* 11. sed -i 删除行 */
    {
        write_file(tmp, "keep\ndelete\nkeep2\n");
        int rc = run("sed -i '/delete/d' /tmp/gsa_test.txt");
        if (rc == 0) {
            rc = run("grep -q 'delete' /tmp/gsa_test.txt");
            if (rc != 0) {
                rc = run("grep -q 'keep' /tmp/gsa_test.txt");
                if (rc == 0) { printf("  PASS | sed -i /d\n"); pass++; }
                else         { printf("  FAIL | sed -i /d (keep lines lost)\n"); fail++; }
            } else {
                printf("  FAIL | sed -i /d (line not deleted)\n"); fail++;
            }
        } else {
            printf("  FAIL | sed -i /d (rc=%d)\n", rc); fail++;
        }
        unlink(tmp);
    }

    /* 12. sed -i -e 多次替换 */
    {
        write_file(tmp, "one two three\n");
        int rc = run("sed -i -e 's/one/1/' -e 's/two/2/' -e 's/three/3/' /tmp/gsa_test.txt");
        if (rc == 0) {
            char buf[256] = {0};
            read_first_line(tmp, buf, sizeof(buf));
            if (strcmp(buf, "1 2 3") == 0) {
                printf("  PASS | sed -i -e multi\n"); pass++;
            } else {
                printf("  FAIL | sed -i -e multi (got '%s')\n", buf); fail++;
            }
        } else {
            printf("  FAIL | sed -i -e multi (rc=%d)\n", rc); fail++;
        }
        unlink(tmp);
    }

    /* 13. sed -n '2p' 打印指定行 */
    {
        int rc = run("echo -e 'line1\\nline2\\nline3' | sed -n '2p' | grep -q '^line2$'");
        if (rc == 0) { printf("  PASS | sed -n 2p\n"); pass++; }
        else         { printf("  FAIL | sed -n 2p (rc=%d)\n", rc); fail++; }
    }

    /* ================================================================
     *  awk 5.x (gawk)
     * ================================================================ */

    /* 14. awk 基本字段 $1 $2 */
    {
        int rc = run("echo 'hello world' | awk '{print $2}' | grep -q '^world$'");
        if (rc == 0) { printf("  PASS | awk field $2\n"); pass++; }
        else         { printf("  FAIL | awk field $2 (rc=%d)\n", rc); fail++; }
    }

    /* 15. awk NR 行号 */
    {
        int rc = run("echo -e 'aa\\nbb\\ncc' | awk 'END{print NR}' | grep -q '^3$'");
        if (rc == 0) { printf("  PASS | awk NR\n"); pass++; }
        else         { printf("  FAIL | awk NR (rc=%d)\n", rc); fail++; }
    }

    /* 16. awk BEGIN/END */
    {
        int rc = run("echo 'data' | awk 'BEGIN{print \"START\"} {print} END{print \"END\"}' "
                      "| grep -qc 'START'");
        if (rc == 0) { printf("  PASS | awk BEGIN/END\n"); pass++; }
        else         { printf("  FAIL | awk BEGIN/END (rc=%d)\n", rc); fail++; }
    }

    /* 17. awk 条件匹配 */
    {
        int rc = run("echo -e '1\\n2\\n3\\n4\\n5' | awk '$1>3{print}' "
                      "| grep -c '^[4-5]$' | grep -q '^2$'");
        if (rc == 0) { printf("  PASS | awk condition\n"); pass++; }
        else         { printf("  FAIL | awk condition (rc=%d)\n", rc); fail++; }
    }

    /* 18. awk 算术 + printf */
    {
        int rc = run("echo '10 20' | awk '{printf \"%d\\n\", $1+$2}' | grep -q '^30$'");
        if (rc == 0) { printf("  PASS | awk arithmetic\n"); pass++; }
        else         { printf("  FAIL | awk arithmetic (rc=%d)\n", rc); fail++; }
    }

    /* 19. awk -F 自定义分隔符 */
    {
        int rc = run("echo 'a:b:c' | awk -F: '{print $2}' | grep -q '^b$'");
        if (rc == 0) { printf("  PASS | awk -F separator\n"); pass++; }
        else         { printf("  FAIL | awk -F separator (rc=%d)\n", rc); fail++; }
    }

    /* ================================================================
     *  grep | sed | awk 管道联合测试
     * ================================================================ */

    /* 20. grep | sed 管道 */
    {
        int rc = run("echo -e 'apple\\nbanana\\ncherry' | grep 'a' | sed 's/banana/BANANA/' "
                      "| grep -q 'BANANA'");
        if (rc == 0) { printf("  PASS | grep|sed pipe\n"); pass++; }
        else         { printf("  FAIL | grep|sed pipe (rc=%d)\n", rc); fail++; }
    }

    /* 21. grep | awk 管道: 提取 /etc/passwd 中 root 行的 shell */
    {
        int rc = run("grep 'root' /etc/passwd | awk -F: '{print $NF}' "
                      "| grep -q '/bin/sh'");
        if (rc == 0) { printf("  PASS | grep|awk pipe\n"); pass++; }
        else         { printf("  FAIL | grep|awk pipe (rc=%d)\n", rc); fail++; }
    }

    /* 22. sed | awk 管道 */
    {
        int rc = run("echo 'price:100' | sed 's/price://' | awk '{printf \"USD_%s\", $1}' "
                      "| grep -q '^USD_100$'");
        if (rc == 0) { printf("  PASS | sed|awk pipe\n"); pass++; }
        else         { printf("  FAIL | sed|awk pipe (rc=%d)\n", rc); fail++; }
    }

    /* 23. grep | sed | awk 三级管道 */
    {
        int rc = run("echo -e 'error:404\\ninfo:200\\nerror:500' "
                      "| grep 'error' "
                      "| sed 's/error://g' "
                      "| awk '{s+=$1} END{print s}' "
                      "| grep -q '^904$'");
        if (rc == 0) { printf("  PASS | grep|sed|awk pipe\n"); pass++; }
        else         { printf("  FAIL | grep|sed|awk pipe (rc=%d)\n", rc); fail++; }
    }

    /* 24. 端到端: 生成文件 → grep -r → sed -i → awk 统计 */
    {
        mkdir("/tmp/gsa_dir", 0755);
        write_file("/tmp/gsa_dir/file1.txt", "apple 10\nbanana 20\n");
        write_file("/tmp/gsa_dir/file2.txt", "apple 30\ncherry 40\n");

        /* grep -r 找 apple 行, awk 统计总量 */
        int rc = run("grep -rh 'apple' /tmp/gsa_dir | awk '{s+=$2} END{print s}' "
                      "| grep -q '^40$'");
        if (rc == 0) { printf("  PASS | grep -r|awk end-to-end\n"); pass++; }
        else         { printf("  FAIL | grep -r|awk end-to-end (rc=%d)\n", rc); fail++; }

        /* sed -i 替换后 awk 统计 */
        run("sed -i 's/apple/fruit/g' /tmp/gsa_dir/file1.txt");
        rc = run("awk '/fruit/{s+=$2} END{print s}' /tmp/gsa_dir/file1.txt "
                  "| grep -q '^10$'");
        if (rc == 0) { printf("  PASS | sed -i|awk end-to-end\n"); pass++; }
        else         { printf("  FAIL | sed -i|awk end-to-end (rc=%d)\n", rc); fail++; }

        /* 清理 */
        unlink("/tmp/gsa_dir/file1.txt");
        unlink("/tmp/gsa_dir/file2.txt");
        rmdir("/tmp/gsa_dir");
    }

    printf("=== total: %d passed, %d failed ===\n", pass, fail);

    if (fail > 0) return 1;
    printf("GREP SED AWK TEST PASSED\n");
    return 0;
}
