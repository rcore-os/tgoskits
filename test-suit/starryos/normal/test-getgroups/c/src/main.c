/*
 * test_getgroups.c -- getgroups 系统调用测试
 *
 * 测试内容：
 *   1. size=0 时应返回附加组数量，不写入缓冲区
 *   2. 正常获取附加组列表，数量与 size=0 查询一致
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <unistd.h>
#include <stdlib.h>
#include <sys/types.h>

int main(void)
{
    TEST_START("getgroups");

    /* size=0 应返回附加组数量（>= 0），不写入缓冲区 */
    int count = getgroups(0, NULL);
    CHECK(count >= 0, "size=0 返回非负组数量");

    /* 缓冲区根据实际数量分配，数量应与 size=0 查询一致 */
    if (count > 0) {
        gid_t *groups = malloc(count * sizeof(gid_t));
        CHECK(groups != NULL, "分配组缓冲区");
        if (groups) {
            int n = getgroups(count, groups);
            CHECK(n >= 0, "正常获取附加组");
            CHECK(n == count, "实际获取数量与 size=0 查询一致");
            free(groups);
        }
    } else {
        CHECK(count == 0, "无附加组时 count 为 0");
    }

    TEST_DONE();
}
