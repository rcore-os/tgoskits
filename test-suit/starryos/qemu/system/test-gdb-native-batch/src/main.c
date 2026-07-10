#include <stdio.h>

__attribute__((noinline)) static int native_marker(int value)
{
    return value + 1;
}

int main(void)
{
    int value = native_marker(41);
    printf("gdb-native-batch-target value=%d\n", value);
    return value == 42 ? 0 : 1;
}
