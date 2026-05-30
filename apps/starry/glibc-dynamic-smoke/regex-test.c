#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <regex.h>

int main(void) {
    regex_t regex;
    int ret;

    ret = regcomp(&regex, "^hello.*world$", REG_EXTENDED);
    if (ret != 0) {
        printf("regcomp failed\n");
        return 1;
    }

    ret = regexec(&regex, "hello beautiful world", 0, NULL, 0);
    if (ret == 0) {
        printf("regex match: OK\n");
    } else {
        printf("regex match: FAIL\n");
    }

    regfree(&regex);
    printf("regex test OK\n");
    return 0;
}
