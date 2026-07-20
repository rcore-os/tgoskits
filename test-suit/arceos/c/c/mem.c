#include "test.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

int arceos_c_test_mem(char *reason, size_t reason_len)
{
    enum { COUNT = 16 };
    uintptr_t *blocks[COUNT];
    uintptr_t *zero = (uintptr_t *)malloc(0);
    uintptr_t *items = (uintptr_t *)calloc(COUNT, sizeof(uintptr_t));

    printf("mem: malloc(0)=%p\n", zero);
    CHECK_TRUE(items != NULL);
    for (int i = 0; i < COUNT; i++) {
        CHECK_RET(items[i], 0);
    }

    for (int i = 0; i < COUNT; i++) {
        blocks[i] = (uintptr_t *)malloc(sizeof(uintptr_t));
        CHECK_TRUE(blocks[i] != NULL);
        *blocks[i] = (uintptr_t)(0x23300000U + (unsigned)i);
    }
    for (int i = 0; i < COUNT; i++) {
        CHECK_RET(*blocks[i], (uintptr_t)(0x23300000U + (unsigned)i));
    }
    for (int i = 0; i < COUNT; i++) {
        free(blocks[i]);
    }
    free(items);
    free(zero);
    puts("mem: allocation APIs OK");
    return 0;
}
