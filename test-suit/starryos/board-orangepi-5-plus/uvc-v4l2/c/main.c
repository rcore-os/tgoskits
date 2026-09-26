#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/videodev2.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

struct mapped_buffer { void *ptr; size_t length; };

static void die(const char *stage) {
    fprintf(stderr, "STARRY_UVC_V4L2_FAILED stage=%s errno=%d (%s)\n", stage, errno, strerror(errno));
    exit(1);
}

static void check_ioctl(int fd, unsigned long request, void *arg, const char *stage) {
    if (ioctl(fd, request, arg) < 0) die(stage);
    printf("STARRY_UVC_V4L2_STAGE %s\n", stage);
    fflush(stdout);
}

static void expect_einval(int fd, unsigned long request, void *arg, const char *stage) {
    errno = 0;
    if (ioctl(fd, request, arg) != -1 || errno != EINVAL) die(stage);
    printf("STARRY_UVC_V4L2_STAGE %s rejected\n", stage);
    fflush(stdout);
}

int main(void) {
    const char *path = "/dev/video0";
    alarm(45);
    int fd = open(path, O_RDWR | O_NONBLOCK);
    if (fd < 0) die("open");
    printf("STARRY_UVC_V4L2_STAGE open fd=%d\n", fd);
    fflush(stdout);

    struct v4l2_capability cap = {0};
    check_ioctl(fd, VIDIOC_QUERYCAP, &cap, "QUERYCAP");
    printf("STARRY_UVC_V4L2_CAP driver=%s card=%s bus=%s caps=%#x device_caps=%#x\n",
           cap.driver, cap.card, cap.bus_info, cap.capabilities, cap.device_caps);
    if (!(cap.device_caps & V4L2_CAP_VIDEO_CAPTURE) ||
        !(cap.device_caps & V4L2_CAP_STREAMING)) die("capabilities");

    struct v4l2_fmtdesc desc = {.type = V4L2_BUF_TYPE_VIDEO_CAPTURE};
    check_ioctl(fd, VIDIOC_ENUM_FMT, &desc, "ENUM_FMT");
    printf("STARRY_UVC_V4L2_FMT fourcc=%c%c%c%c\n", desc.pixelformat & 255,
           (desc.pixelformat >> 8) & 255, (desc.pixelformat >> 16) & 255,
           (desc.pixelformat >> 24) & 255);

    struct v4l2_format trial = {.type = V4L2_BUF_TYPE_VIDEO_CAPTURE};
    trial.fmt.pix.width = 639;
    trial.fmt.pix.height = 479;
    trial.fmt.pix.pixelformat = V4L2_PIX_FMT_MJPEG;
    check_ioctl(fd, VIDIOC_TRY_FMT, &trial, "TRY_FMT");
    if (trial.fmt.pix.pixelformat != V4L2_PIX_FMT_MJPEG ||
        trial.fmt.pix.width != 640 || trial.fmt.pix.height != 480 ||
        !trial.fmt.pix.sizeimage) die("TRY_FMT_result");

    struct v4l2_format format = {.type = V4L2_BUF_TYPE_VIDEO_CAPTURE};
    format.fmt.pix.width = 640;
    format.fmt.pix.height = 480;
    format.fmt.pix.pixelformat = V4L2_PIX_FMT_MJPEG;
    format.fmt.pix.field = V4L2_FIELD_ANY;
    check_ioctl(fd, VIDIOC_S_FMT, &format, "S_FMT");
    printf("STARRY_UVC_V4L2_SET_FMT width=%u height=%u fourcc=%#x sizeimage=%u\n",
           format.fmt.pix.width, format.fmt.pix.height,
           format.fmt.pix.pixelformat, format.fmt.pix.sizeimage);
    if (format.fmt.pix.pixelformat != V4L2_PIX_FMT_MJPEG ||
        format.fmt.pix.width != 640 || format.fmt.pix.height != 480 ||
        !format.fmt.pix.sizeimage) die("S_FMT_result");

    struct v4l2_streamparm parm = {.type = V4L2_BUF_TYPE_VIDEO_CAPTURE};
    parm.parm.capture.timeperframe.numerator = 1;
    parm.parm.capture.timeperframe.denominator = 30;
    check_ioctl(fd, VIDIOC_S_PARM, &parm, "S_PARM");
    printf("STARRY_UVC_V4L2_PARM %u/%u\n",
           parm.parm.capture.timeperframe.numerator,
           parm.parm.capture.timeperframe.denominator);

    struct v4l2_requestbuffers req = {.count = 4,
        .type = V4L2_BUF_TYPE_VIDEO_CAPTURE, .memory = V4L2_MEMORY_MMAP};
    check_ioctl(fd, VIDIOC_REQBUFS, &req, "REQBUFS");
    if (req.count < 2 || req.count > 16) die("REQBUFS_count");
    struct v4l2_buffer invalid = {.index = 0, .type = 0xffffffffu,
        .memory = V4L2_MEMORY_MMAP};
    expect_einval(fd, VIDIOC_QUERYBUF, &invalid, "QUERYBUF_type");
    invalid.type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    invalid.memory = 0xffffffffu;
    expect_einval(fd, VIDIOC_QBUF, &invalid, "QBUF_memory");
    struct mapped_buffer *mapped = calloc(req.count, sizeof(*mapped));
    if (!mapped) die("calloc");
    for (unsigned i = 0; i < req.count; i++) {
        struct v4l2_buffer buf = {.index = i, .type = V4L2_BUF_TYPE_VIDEO_CAPTURE,
            .memory = V4L2_MEMORY_MMAP};
        check_ioctl(fd, VIDIOC_QUERYBUF, &buf, "QUERYBUF");
        mapped[i].length = buf.length;
        mapped[i].ptr = mmap(NULL, buf.length, PROT_READ | PROT_WRITE, MAP_SHARED,
                             fd, buf.m.offset);
        if (mapped[i].ptr == MAP_FAILED) die("mmap");
        printf("STARRY_UVC_V4L2_STAGE mmap index=%u length=%zu offset=%u\n",
               i, mapped[i].length, buf.m.offset);
    }

    /* Queue out of index order to verify the capture queue is FIFO. */
    for (unsigned n = 0; n < req.count; n++) {
        unsigned index = (n + req.count - 1) % req.count;
        struct v4l2_buffer buf = {.index = index, .type = V4L2_BUF_TYPE_VIDEO_CAPTURE,
            .memory = V4L2_MEMORY_MMAP};
        check_ioctl(fd, VIDIOC_QBUF, &buf, "QBUF");
    }

    enum v4l2_buf_type type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    check_ioctl(fd, VIDIOC_STREAMON, &type, "STREAMON");
    invalid.type = 0xffffffffu;
    invalid.memory = V4L2_MEMORY_MMAP;
    expect_einval(fd, VIDIOC_DQBUF, &invalid, "DQBUF_type");
    unsigned frames = 0;
    unsigned long bytes = 0;
    for (unsigned attempt = 0; attempt < 20 && frames < 5; attempt++) {
        struct pollfd pfd = {.fd = fd, .events = POLLIN | POLLPRI};
        int ready = poll(&pfd, 1, 5000);
        if (ready < 0) die("poll");
        if (!ready) die("poll_timeout");
        struct v4l2_buffer buf = {.type = V4L2_BUF_TYPE_VIDEO_CAPTURE,
            .memory = V4L2_MEMORY_MMAP};
        if (ioctl(fd, VIDIOC_DQBUF, &buf) < 0) {
            if (errno == EAGAIN) continue;
            die("DQBUF");
        }
        if (buf.index >= req.count || buf.bytesused < 4 ||
            buf.bytesused > mapped[buf.index].length) die("frame_bounds");
        if (frames == 0 && buf.index != req.count - 1) die("qbuf_order");
        const unsigned char *data = mapped[buf.index].ptr;
        if (data[0] != 0xff || data[1] != 0xd8) die("jpeg_soi");
        int eoi = 0;
        for (unsigned j = 2; j < buf.bytesused; j++)
            if (data[j - 1] == 0xff && data[j] == 0xd9) eoi = 1;
        if (!eoi) die("jpeg_eoi");
        frames++;
        bytes += buf.bytesused;
        printf("STARRY_UVC_V4L2_FRAME index=%u bytes=%u sequence=%u\n",
               buf.index, buf.bytesused, buf.sequence);
        check_ioctl(fd, VIDIOC_QBUF, &buf, "QBUF_REQUEUE");
    }
    if (frames != 5 || !bytes) die("frame_count");
    check_ioctl(fd, VIDIOC_STREAMOFF, &type, "STREAMOFF");
    for (unsigned i = 0; i < req.count; i++)
        if (munmap(mapped[i].ptr, mapped[i].length) < 0) die("munmap");
    free(mapped);
    close(fd);
    printf("STARRY_UVC_V4L2_OK frames=%u bytes=%lu\n", frames, bytes);
    return 0;
}
