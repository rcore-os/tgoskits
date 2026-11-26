/* SPDX-License-Identifier: Apache-2.0 */

#include "starry_axivc.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#define TGOS_STARRY_AXIVC_PAYLOAD_INFO_VERSION 1ULL

struct starry_axivc_payload_info {
  uint64_t version;
  uint64_t generation;
  uint64_t local_offset;
  uint64_t peer_offset;
  uint64_t payload_size;
};

#define TGOS_STARRY_AXIVC_PAYLOAD_INFO                                         \
  _IOR(0x49, 0x06, struct starry_axivc_payload_info)

_Static_assert(sizeof(struct starry_axivc_payload_info) == 0x28,
               "payload info ABI must remain 40 bytes");
_Static_assert((uint32_t)TGOS_STARRY_AXIVC_PAYLOAD_INFO == 0x80284906U,
               "payload info ioctl must remain _IOR(0x49, 6, payload_info)");

struct ivc {
  int fd;
  int failed;
  size_t capacity;
  struct starry_axivc_payload_info info;
  uint8_t *local_payload;
  const uint8_t *peer_payload;
};

static int starry_error_from_errno(struct ivc *connection, int error) {
  if (error == EAGAIN || error == EWOULDBLOCK) {
    return -EAGAIN;
  }
  if (error == EIO || error == EPIPE || error == ENODEV) {
    connection->failed = 1;
    return -EPIPE;
  }
  return -error;
}

static int starry_wait_fd(struct ivc *connection, short events,
                          int32_t timeout_ms) {
  struct pollfd pfd = {
      .fd = connection->fd,
      .events = events,
  };
  int result = poll(&pfd, 1, timeout_ms);

  if (result == 0) {
    return -EAGAIN;
  }
  if (result < 0) {
    return starry_error_from_errno(connection, errno);
  }
  if ((pfd.revents & POLLNVAL) != 0) {
    return -EBADF;
  }
  if ((pfd.revents & (POLLERR | POLLHUP)) != 0) {
    connection->failed = 1;
    return -EPIPE;
  }
  if ((pfd.revents & events) == 0) {
    return -EAGAIN;
  }
  return 0;
}

static int32_t starry_deadline_remaining_ms(uint64_t deadline) {
  uint64_t now = starry_now_us();

  if (now >= deadline) {
    return 0;
  }
  if (deadline - now > (uint64_t)INT32_MAX * 1000ULL) {
    return INT32_MAX;
  }
  return (int32_t)((deadline - now + 999ULL) / 1000ULL);
}

static int starry_timeout_valid(int32_t timeout_ms) {
  return timeout_ms >= 0 && timeout_ms <= IVC_MAX_TIMEOUT_MS;
}

int ivc_open(struct ivc **out) {
  struct ivc *connection;
  int fd;

  if (out == NULL) {
    return -EINVAL;
  }
  *out = NULL;

  fd = open(TGOS_STARRY_AXIVC_DEVICE, O_RDWR | O_CLOEXEC | O_NONBLOCK);
  if (fd < 0) {
    return -errno;
  }
  connection = calloc(1U, sizeof(*connection));
  if (connection == NULL) {
    close(fd);
    return -ENOMEM;
  }
  connection->fd = fd;
  *out = connection;
  return 0;
}

int starry_axivc_payload_get(struct ivc *connection,
                             size_t minimum_capacity,
                             struct starry_axivc_payload_view *view) {
  struct starry_axivc_payload_info info;
  long page_size;

  if (connection == NULL || view == NULL || minimum_capacity == 0U) {
    return -EINVAL;
  }
  memset(view, 0, sizeof(*view));
  if (connection->failed) {
    return -EPIPE;
  }
  if (connection->capacity != 0U) {
    if (connection->capacity < minimum_capacity) {
      return -ENOSPC;
    }
    view->generation = connection->info.generation;
    view->local_offset = connection->info.local_offset;
    view->peer_offset = connection->info.peer_offset;
    view->capacity = connection->capacity;
    view->local_payload = connection->local_payload;
    view->peer_payload = connection->peer_payload;
    return 0;
  }

  memset(&info, 0, sizeof(info));
  /*
   * musl declares the request parameter as int while Linux consumes the
   * unsigned 32-bit ioctl encoding. Preserve those low 32 request bits.
   */
  if (ioctl(connection->fd, (int)(uint32_t)TGOS_STARRY_AXIVC_PAYLOAD_INFO,
            &info) != 0) {
    return starry_error_from_errno(connection, errno);
  }
  page_size = sysconf(_SC_PAGESIZE);
  if (info.version != TGOS_STARRY_AXIVC_PAYLOAD_INFO_VERSION ||
      info.payload_size == 0U || info.payload_size > (uint64_t)SIZE_MAX ||
      info.local_offset > (uint64_t)INT64_MAX ||
      info.peer_offset > (uint64_t)INT64_MAX ||
      info.local_offset == info.peer_offset || page_size <= 0 ||
      info.local_offset % (uint64_t)page_size != 0U ||
      info.peer_offset % (uint64_t)page_size != 0U) {
    return -EPROTO;
  }
  if ((size_t)info.payload_size < minimum_capacity) {
    return -ENOSPC;
  }

  /*
   * payload_size is a byte count and may be unaligned. POSIX mmap only
   * constrains the file offset, which was checked above, so map the returned
   * capacity verbatim instead of rounding or consulting file metadata.
   */
  connection->local_payload =
      mmap(NULL, (size_t)info.payload_size, PROT_READ | PROT_WRITE, MAP_SHARED,
           connection->fd, (off_t)info.local_offset);
  if (connection->local_payload == MAP_FAILED) {
    connection->local_payload = NULL;
    return -errno;
  }
  connection->peer_payload =
      mmap(NULL, (size_t)info.payload_size, PROT_READ, MAP_SHARED,
           connection->fd, (off_t)info.peer_offset);
  if (connection->peer_payload == MAP_FAILED) {
    int error = errno;

    connection->peer_payload = NULL;
    munmap(connection->local_payload, (size_t)info.payload_size);
    connection->local_payload = NULL;
    return -error;
  }
  connection->info = info;
  connection->capacity = (size_t)info.payload_size;
  view->generation = info.generation;
  view->local_offset = info.local_offset;
  view->peer_offset = info.peer_offset;
  view->capacity = connection->capacity;
  view->local_payload = connection->local_payload;
  view->peer_payload = connection->peer_payload;
  return 0;
}

int ivc_close(struct ivc *connection) {
  int error = 0;

  if (connection == NULL) {
    return -EINVAL;
  }
  if (connection->peer_payload != NULL &&
      munmap((void *)connection->peer_payload, connection->capacity) != 0) {
    error = -errno;
  }
  if (connection->local_payload != NULL &&
      munmap(connection->local_payload, connection->capacity) != 0 &&
      error == 0) {
    error = -errno;
  }
  if (connection->fd >= 0 && close(connection->fd) != 0 && error == 0) {
    error = -errno;
  }
  free(connection);
  return error;
}

int ivc_send(struct ivc *connection, const void *data, size_t len,
             int32_t timeout_ms) {
  uint64_t deadline;

  if (connection == NULL || data == NULL || len == 0U) {
    return -EINVAL;
  }
  if (len > IVC_MAX_RECORD_BYTES) {
    return -EMSGSIZE;
  }
  if (!starry_timeout_valid(timeout_ms)) {
    return -EINVAL;
  }
  if (connection->failed) {
    return -EPIPE;
  }
  deadline = starry_now_us() + (uint64_t)timeout_ms * 1000ULL;

  for (;;) {
    ssize_t written = write(connection->fd, data, len);
    int32_t remaining;
    int error;

    if (written == (ssize_t)len) {
      return 0;
    }
    if (written > 0) {
      return -EIO;
    }
    if (written == 0) {
      if (timeout_ms == 0) {
        return -EAGAIN;
      }
    } else {
      if (errno == EINTR) {
        if (timeout_ms == 0) {
          return -EINTR;
        }
        remaining = starry_deadline_remaining_ms(deadline);
        if (remaining == 0) {
          return -EAGAIN;
        }
        continue;
      }
      if (errno != EAGAIN && errno != EWOULDBLOCK) {
        return starry_error_from_errno(connection, errno);
      }
      if (timeout_ms == 0) {
        return -EAGAIN;
      }
    }
    remaining = starry_deadline_remaining_ms(deadline);
    if (remaining == 0) {
      return -EAGAIN;
    }
    error = starry_wait_fd(connection, POLLOUT, remaining);
    if (error == -EINTR) {
      remaining = starry_deadline_remaining_ms(deadline);
      if (remaining == 0) {
        return -EAGAIN;
      }
      continue;
    }
    if (error != 0) {
      return error;
    }
  }
}

int ivc_recv(struct ivc *connection, void *buffer, size_t cap,
             int32_t timeout_ms) {
  uint64_t deadline;

  if (connection == NULL || buffer == NULL || cap == 0U) {
    return -EINVAL;
  }
  if (!starry_timeout_valid(timeout_ms)) {
    return -EINVAL;
  }
  if (connection->failed) {
    return -EPIPE;
  }
  deadline = starry_now_us() + (uint64_t)timeout_ms * 1000ULL;

  for (;;) {
    int32_t remaining;
    int error;
    ssize_t received;

    received = read(connection->fd, buffer, cap);

    if (received > 0) {
      if ((size_t)received > IVC_MAX_RECORD_BYTES) {
        return -EMSGSIZE;
      }
      return (int)received;
    }
    if (received == 0) {
      connection->failed = 1;
      return -EPIPE;
    }
    if (errno == EINVAL) {
      /*
       * Starry rejects a legal packet that is larger than the caller's buffer
       * without consuming it. At the maximum record size, EINVAL cannot mean a
       * legal short-buffer failure and is therefore a wire protocol anomaly.
       */
      if (cap >= IVC_MAX_RECORD_BYTES) {
        return -EMSGSIZE;
      }
      return -ENOSPC;
    }
    if (errno == EINTR) {
      if (timeout_ms == 0) {
        return -EINTR;
      }
      remaining = starry_deadline_remaining_ms(deadline);
      if (remaining == 0) {
        return -EAGAIN;
      }
      continue;
    }
    if (errno != EAGAIN && errno != EWOULDBLOCK) {
      return starry_error_from_errno(connection, errno);
    }
    if (timeout_ms == 0) {
      return -EAGAIN;
    }
    remaining = starry_deadline_remaining_ms(deadline);
    if (remaining == 0) {
      return -EAGAIN;
    }
    error = starry_wait_fd(connection, POLLIN, remaining);
    if (error == -EINTR) {
      remaining = starry_deadline_remaining_ms(deadline);
      if (remaining == 0) {
        return -EAGAIN;
      }
      continue;
    }
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
  case EINVAL:
    return "invalid-argument";
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
