#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <errno.h>
#include <string.h>

int main(void) {
    // Test 1: mkdir("") should return ENOENT (not EEXIST)
    errno = 0;
    if (mkdir("", 0755) == -1) {
        if (errno == ENOENT) {
            printf("Test 1 PASSED: mkdir(\"\") correctly returns ENOENT\n");
        } else {
            printf("Test 1 FAILED: mkdir(\"\") returned errno %d (%s), expected ENOENT (%d)\n", 
                   errno, strerror(errno), ENOENT);
            return 1;
        }
    } else {
        printf("Test 1 FAILED: mkdir(\"\") succeeded, expected ENOENT\n");
        return 1;
    }

    // Test 2: mkdir("/") should return EEXIST (root exists)
    errno = 0;
    if (mkdir("/", 0755) == -1) {
        if (errno == EEXIST) {
            printf("Test 2 PASSED: mkdir(\"/\") correctly returns EEXIST\n");
        } else {
            printf("Test 2 FAILED: mkdir(\"/\") returned errno %d (%s), expected EEXIST (%d)\n", 
                   errno, strerror(errno), EEXIST);
            return 1;
        }
    } else {
        printf("Test 2 FAILED: mkdir(\"/\") succeeded, expected EEXIST\n");
        return 1;
    }

    // Test 3: mkdir(".") should return EEXIST (current dir exists)
    errno = 0;
    if (mkdir(".", 0755) == -1) {
        if (errno == EEXIST) {
            printf("Test 3 PASSED: mkdir(\".\") correctly returns EEXIST\n");
        } else {
            printf("Test 3 FAILED: mkdir(\".\") returned errno %d (%s), expected EEXIST (%d)\n", 
                   errno, strerror(errno), EEXIST);
            return 1;
        }
    } else {
        printf("Test 3 FAILED: mkdir(\".\") succeeded, expected EEXIST\n");
        return 1;
    }

    printf("TEST PASSED\n");
    return 0;
}
