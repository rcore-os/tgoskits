/* kcov-spec §1: Opening /dev/kcov */
#include "test_framework.h"
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    TEST_START("KCOV §1: open /dev/kcov");
    int fd = open("/dev/kcov", O_RDWR);
    CHECK(fd >= 0, "/dev/kcov opens with O_RDWR");
    if (fd >= 0)
        close(fd);
    struct stat st;
    CHECK_RET(stat("/dev/kcov", &st), 0, "/dev/kcov exists");
    CHECK(S_ISCHR(st.st_mode), "/dev/kcov is a character device");
    TEST_DONE();
}
