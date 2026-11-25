/* SPDX-License-Identifier: Apache-2.0 */

#include "starry_axivc.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

static int starry_wait_fd(int fd, short events, int32_t timeout_ms) {
  struct pollfd pfd = {
      .fd = fd,
      .events = events,
  };
  int result;

  do {
    result = poll(&pfd, 1, timeout_ms);
  } while (result < 0 && errno == EINTR);
  if (result == 0) {
    return -ETIMEDOUT;
  }
  if (result < 0) {
    return -errno;
  }
  if ((pfd.revents & POLLNVAL) != 0) {
    return -EBADF;
  }
  if ((pfd.revents & POLLERR) != 0) {
    return -EIO;
  }
  if ((pfd.revents & POLLHUP) != 0 && (pfd.revents & events) == 0) {
    return -EPIPE;
  }
  return 0;
}

static int starry_deadline_remaining_us(uint64_t deadline) {
  uint64_t now = starry_now_us();

  if (now >= deadline) {
    return 0;
  }
  if (deadline - now > (uint64_t)INT32_MAX * 1000ULL) {
    return INT32_MAX;
  }
  return (int)((deadline - now + 999ULL) / 1000ULL);
}

int starry_axivc_open(struct starry_axivc *ivc, size_t minimum_capacity) {
  struct starry_axivc_payload_info info;
  long page_size;
  int fd;

  if (ivc == NULL || minimum_capacity == 0U) {
    return -EINVAL;
  }
  memset(ivc, 0, sizeof(*ivc));
  ivc->fd = -1;

  fd = open(TGOS_STARRY_AXIVC_DEVICE, O_RDWR | O_CLOEXEC);
  if (fd < 0) {
    return -errno;
  }
  memset(&info, 0, sizeof(info));
  /*
   * musl declares the request parameter as int while Linux consumes the
   * unsigned 32-bit ioctl encoding. Preserve those low 32 request bits.
   */
  if (ioctl(fd, (int)(uint32_t)TGOS_STARRY_AXIVC_PAYLOAD_INFO, &info) != 0) {
    int error = errno;

    close(fd);
    return -error;
  }
  page_size = sysconf(_SC_PAGESIZE);
  if (info.version != TGOS_STARRY_AXIVC_PAYLOAD_INFO_VERSION ||
      info.payload_size == 0U || info.payload_size > (uint64_t)SIZE_MAX ||
      info.local_offset > (uint64_t)INT64_MAX ||
      info.peer_offset > (uint64_t)INT64_MAX ||
      info.local_offset == info.peer_offset || page_size <= 0 ||
      info.local_offset % (uint64_t)page_size != 0U ||
      info.peer_offset % (uint64_t)page_size != 0U) {
    close(fd);
    return -EPROTO;
  }
  ivc->info = info;
  ivc->capacity = (size_t)info.payload_size;
  if (ivc->capacity < minimum_capacity) {
    close(fd);
    return -ENOSPC;
  }

  /*
   * payload_size is a byte count and may be unaligned. POSIX mmap only
   * constrains the file offset, which was checked above, so map the returned
   * capacity verbatim instead of rounding or consulting file metadata.
   */
  ivc->local_payload = mmap(NULL, ivc->capacity, PROT_READ | PROT_WRITE,
                            MAP_SHARED, fd, (off_t)info.local_offset);
  if (ivc->local_payload == MAP_FAILED) {
    int error = errno;

    ivc->local_payload = NULL;
    close(fd);
    return -error;
  }
  ivc->peer_payload = mmap(NULL, ivc->capacity, PROT_READ, MAP_SHARED, fd,
                           (off_t)info.peer_offset);
  if (ivc->peer_payload == MAP_FAILED) {
    int error = errno;

    ivc->peer_payload = NULL;
    munmap(ivc->local_payload, ivc->capacity);
    ivc->local_payload = NULL;
    close(fd);
    return -error;
  }
  ivc->fd = fd;
  return 0;
}

void starry_axivc_close(struct starry_axivc *ivc) {
  if (ivc == NULL) {
    return;
  }
  if (ivc->peer_payload != NULL) {
    munmap((void *)ivc->peer_payload, ivc->capacity);
  }
  if (ivc->local_payload != NULL) {
    munmap(ivc->local_payload, ivc->capacity);
  }
  if (ivc->fd >= 0) {
    close(ivc->fd);
  }
  memset(ivc, 0, sizeof(*ivc));
  ivc->fd = -1;
}

int starry_axivc_send_frame(struct starry_axivc *ivc,
                            const struct tgos_ivc_frame *frame,
                            int32_t timeout_ms) {
  uint64_t deadline;

  if (ivc == NULL || frame == NULL || ivc->fd < 0 || timeout_ms < 0) {
    return -EINVAL;
  }
  deadline = starry_now_us() + (uint64_t)timeout_ms * 1000ULL;

  for (;;) {
    ssize_t written = write(ivc->fd, frame, sizeof(*frame));
    int remaining;

    if (written == (ssize_t)sizeof(*frame)) {
      return 0;
    }
    if (written >= 0) {
      fprintf(stderr, "AXIVC Starry short frame write bytes=%zd expected=%zu\n",
              written, sizeof(*frame));
      return -EMSGSIZE;
    }
    if (errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR) {
      return -errno;
    }
    remaining = starry_deadline_remaining_us(deadline);
    if (remaining == 0) {
      return -ETIMEDOUT;
    }
    int error = starry_wait_fd(ivc->fd, POLLOUT, remaining);

    if (error != 0) {
      return error;
    }
  }
}

int starry_axivc_recv_frame(struct starry_axivc *ivc,
                            struct tgos_ivc_frame *frame, int32_t timeout_ms) {
  uint64_t deadline;

  if (ivc == NULL || frame == NULL || ivc->fd < 0 || timeout_ms < 0) {
    return -EINVAL;
  }
  deadline = starry_now_us() + (uint64_t)timeout_ms * 1000ULL;

  for (;;) {
    ssize_t received = read(ivc->fd, frame, sizeof(*frame));
    int remaining;

    if (received == (ssize_t)sizeof(*frame)) {
      return 0;
    }
    if (received >= 0) {
      fprintf(stderr, "AXIVC Starry short frame read bytes=%zd expected=%zu\n",
              received, sizeof(*frame));
      return received == 0 ? -EPIPE : -EMSGSIZE;
    }
    if (errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR) {
      return -errno;
    }
    remaining = starry_deadline_remaining_us(deadline);
    if (remaining == 0) {
      return -ETIMEDOUT;
    }
    int error = starry_wait_fd(ivc->fd, POLLIN, remaining);

    if (error != 0) {
      return error;
    }
  }
}

uint64_t starry_now_us(void) {
  struct timespec time;

  if (clock_gettime(CLOCK_MONOTONIC, &time) != 0) {
    return 0U;
  }
  return (uint64_t)time.tv_sec * 1000000ULL + (uint64_t)time.tv_nsec / 1000ULL;
}

const char *starry_error_name(int error) {
  if (error >= 0) {
    return "unknown";
  }
  switch (-error) {
  case EAGAIN:
    return "would-block";
  case EMSGSIZE:
    return "short-frame";
  case ETIMEDOUT:
    return "timeout";
  case EPIPE:
    return "peer-closed";
  case ENOSPC:
    return "payload-too-small";
  case ENODEV:
    return "device-unavailable";
  case EPROTO:
    return "protocol";
  case EOVERFLOW:
    return "overflow";
  default:
    return strerror(-error);
  }
}
