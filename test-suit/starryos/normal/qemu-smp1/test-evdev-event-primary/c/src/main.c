/*
 * test-evdev-event-primary
 *
 * Verifies that input devices register under /dev/input/event<N> as
 * libinput expects. Older code keyed device classification on a single
 * byte of the keys bitmap and registered keyboards/mice both as
 * /dev/input/mice, which collided when virtio-keyboard reported a key
 * in the same byte and one device disappeared.
 */

#include "test_framework.h"
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void)
{
    TEST_START("/dev/input/event0 exists and is openable");

    struct stat st;
    int ev0 = stat("/dev/input/event0", &st);
    CHECK_RET(ev0, 0, "stat /dev/input/event0");
    if (ev0 == 0) {
        CHECK((st.st_mode & S_IFMT) == S_IFCHR,
              "/dev/input/event0 is a character device");
    }

    int fd = open("/dev/input/event0", O_RDONLY | O_NONBLOCK);
    CHECK(fd >= 0, "open /dev/input/event0 O_RDONLY|O_NONBLOCK");
    if (fd >= 0) {
        close(fd);
    }

    TEST_DONE();
}
