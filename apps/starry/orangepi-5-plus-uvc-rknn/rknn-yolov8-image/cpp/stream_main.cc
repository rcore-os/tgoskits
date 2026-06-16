// Copyright (c) 2023 by Rockchip Electronics Co., Ltd. All Rights Reserved.
//
// Licensed under the Apache License, Version 2.0.

#include <errno.h>
#include <fcntl.h>
#include <arpa/inet.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <mutex>
#include <string>
#include <vector>

#include "turbojpeg.h"
#include "uvc_capture.h"
#include "image_drawing.h"
#include "yolov8.h"

struct Options {
    int device = 0;
    int width = 320;
    int height = 240;
    int fps = 10;
    int duration_sec = 0;
    int infer_every = 3;
    int max_inferences = 0;
    int http_port = 8080;
    int http_fps = 8;
    int jpeg_quality = 75;
    int publish_width = 0;
    int publish_height = 0;
    const char *push_host = NULL;
    int push_port = 18080;
    int push_fps = 3;
    int serial_fps = 0;
    int log_every = 10;
    int min_confidence = 55;
    bool draw_result = true;
    const char *model_path = "model/yolov8.rknn";
    const char *label_path = "model/coco_80_labels_list.txt";
};

struct ResultState {
    std::mutex mutex;
    object_detect_result_list results;
    uint64_t frame_id = 0;
};

struct HttpFrameState {
    std::mutex mutex;
    std::condition_variable updated;
    std::vector<unsigned char> jpeg;
    uint64_t frame_id = 0;
    int width = 0;
    int height = 0;
    int detections = 0;
    bool running = false;
};

struct HttpServer {
    int port = 0;
    int listen_fd = -1;
    int target_fps = 15;
    int max_clients = 4;
    std::atomic<int> active_clients{0};
    pthread_t thread = 0;
    HttpFrameState *frames = NULL;
};

struct HttpClient {
    int fd = -1;
    int target_fps = 15;
    HttpFrameState *frames = NULL;
    std::atomic<int> *active_clients = NULL;
};

struct PushClient {
    std::string host;
    int port = 0;
    int target_fps = 15;
    pthread_t thread = 0;
    HttpFrameState *frames = NULL;
    uint64_t sent = 0;
    uint64_t sent_packets = 0;
    uint64_t send_errors = 0;
};

struct SerialPublisher {
    pthread_t thread = 0;
    HttpFrameState *frames = NULL;
    int target_fps = 1;
};

struct DisplayPublisher {
    pthread_t thread = 0;
    SharedState *capture = NULL;
    ResultState *results = NULL;
    HttpFrameState *frames = NULL;
    int target_fps = 10;
    int jpeg_quality = 75;
    int publish_width = 0;
    int publish_height = 0;
    bool draw_result = true;
};

static volatile sig_atomic_t g_running = 1;

static void signal_handler(int)
{
    g_running = 0;
}

static double monotonic_sec()
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1000000000.0;
}

static int clamp_int(int value, int low, int high)
{
    return std::max(low, std::min(high, value));
}

static object_detect_result_list make_display_results(const object_detect_result_list *results)
{
    return *results;
}

static void scale_display_results(object_detect_result_list *results, int src_width, int src_height, int dst_width, int dst_height)
{
    if (results == NULL || src_width <= 0 || src_height <= 0 || dst_width <= 0 || dst_height <= 0) {
        return;
    }
    for (int i = 0; i < results->count; ++i) {
        image_rect_t *box = &results->results[i].box;
        box->left = clamp_int(box->left * dst_width / src_width, 0, dst_width - 1);
        box->right = clamp_int(box->right * dst_width / src_width, 0, dst_width - 1);
        box->top = clamp_int(box->top * dst_height / src_height, 0, dst_height - 1);
        box->bottom = clamp_int(box->bottom * dst_height / src_height, 0, dst_height - 1);
    }
}

static int resize_rgb_nearest(image_buffer_t *image, int dst_width, int dst_height)
{
    if (image == NULL || image->virt_addr == NULL || image->format != IMAGE_FORMAT_RGB888 ||
        dst_width <= 0 || dst_height <= 0) {
        return -1;
    }
    if (image->width == dst_width && image->height == dst_height) {
        return 0;
    }

    int dst_size = dst_width * dst_height * 3;
    unsigned char *dst = reinterpret_cast<unsigned char *>(malloc(dst_size));
    if (dst == NULL) {
        printf("display: malloc resize buffer failed: size=%d\n", dst_size);
        return -1;
    }

    int src_stride = image->width_stride > 0 ? image->width_stride : image->width * 3;
    for (int y = 0; y < dst_height; ++y) {
        int src_y = y * image->height / dst_height;
        const unsigned char *src_row = image->virt_addr + src_y * src_stride;
        unsigned char *dst_row = dst + y * dst_width * 3;
        for (int x = 0; x < dst_width; ++x) {
            int src_x = x * image->width / dst_width;
            const unsigned char *src = src_row + src_x * 3;
            unsigned char *out = dst_row + x * 3;
            out[0] = src[0];
            out[1] = src[1];
            out[2] = src[2];
        }
    }

    free(image->virt_addr);
    image->width = dst_width;
    image->height = dst_height;
    image->width_stride = dst_width * 3;
    image->height_stride = dst_height;
    image->virt_addr = dst;
    image->size = dst_size;
    return 0;
}

static void draw_detection_results(image_buffer_t *image, const object_detect_result_list *results)
{
    for (int i = 0; i < results->count; ++i) {
        const object_detect_result *det = &results->results[i];
        int left = clamp_int(det->box.left, 0, image->width - 1);
        int top = clamp_int(det->box.top, 0, image->height - 1);
        int right = clamp_int(det->box.right, 0, image->width - 1);
        int bottom = clamp_int(det->box.bottom, 0, image->height - 1);
        if (right <= left || bottom <= top) {
            continue;
        }

        int line_width = image->width <= 120 ? 1 : 2;
        draw_rectangle(image, left, top, right - left, bottom - top, COLOR_GREEN, line_width);
        if (image->width <= 120 || image->height <= 90) {
            continue;
        }

        char label[96];
        snprintf(label, sizeof(label), "%s %.0f%%", coco_cls_to_name(det->cls_id), det->prop * 100.0f);
        int text_y = top > 18 ? top - 18 : top + 4;
        draw_text(image, label, left, text_y, COLOR_YELLOW, 10);
    }
}

static int encode_jpeg(const image_buffer_t *image, int quality, std::vector<unsigned char> *jpeg)
{
    tjhandle handle = tjInitCompress();
    if (handle == NULL) {
        printf("tjInitCompress failed\n");
        return -1;
    }

    unsigned char *jpeg_buf = NULL;
    unsigned long jpeg_size = 0;
    int ret = tjCompress2(
        handle,
        image->virt_addr,
        image->width,
        0,
        image->height,
        TJPF_RGB,
        &jpeg_buf,
        &jpeg_size,
        TJSAMP_420,
        quality,
        TJFLAG_FASTDCT);
    if (ret != 0) {
        printf("tjCompress2 failed: %s\n", tjGetErrorStr());
        tjDestroy(handle);
        return -1;
    }

    jpeg->assign(jpeg_buf, jpeg_buf + jpeg_size);
    tjFree(jpeg_buf);
    tjDestroy(handle);
    return 0;
}

static void publish_http_frame(HttpFrameState *state, uint64_t frame_id, int width, int height, int detections, const std::vector<unsigned char> &jpeg)
{
    std::lock_guard<std::mutex> guard(state->mutex);
    state->jpeg = jpeg;
    state->frame_id = frame_id;
    state->width = width;
    state->height = height;
    state->detections = detections;
    state->updated.notify_all();
}

static void *display_publisher_thread(void *arg)
{
    DisplayPublisher *publisher = reinterpret_cast<DisplayPublisher *>(arg);
    uint64_t last_frame = 0;
    int sleep_us = publisher->target_fps > 0 ? 1000000 / publisher->target_fps : 100000;
    while (g_running && publisher->frames->running) {
        LatestFrame frame;
        if (!snapshot_latest_capture(publisher->capture, &frame) || frame.id == last_frame) {
            usleep(10000);
            continue;
        }
        last_frame = frame.id;

        image_buffer_t image;
        memset(&image, 0, sizeof(image));
        if (frame_to_image(frame, &image) != 0) {
            continue;
        }

        object_detect_result_list results;
        memset(&results, 0, sizeof(results));
        {
            std::lock_guard<std::mutex> guard(publisher->results->mutex);
            results = publisher->results->results;
        }
        object_detect_result_list display_results = make_display_results(&results);
        int source_width = image.width;
        int source_height = image.height;
        if (publisher->publish_width > 0 && publisher->publish_height > 0 &&
            (publisher->publish_width != image.width || publisher->publish_height != image.height)) {
            if (resize_rgb_nearest(&image, publisher->publish_width, publisher->publish_height) == 0) {
                scale_display_results(&display_results, source_width, source_height, image.width, image.height);
            }
        }
        if (publisher->draw_result) {
            draw_detection_results(&image, &display_results);
        }

        std::vector<unsigned char> jpeg;
        if (encode_jpeg(&image, publisher->jpeg_quality, &jpeg) == 0) {
            publish_http_frame(publisher->frames, frame.id, image.width, image.height, display_results.count, jpeg);
        }
        free(image.virt_addr);
        if (sleep_us > 0) {
            usleep(sleep_us);
        }
    }
    return NULL;
}

static bool start_display_publisher(DisplayPublisher *publisher, SharedState *capture, ResultState *results,
                                    HttpFrameState *frames, int target_fps, int jpeg_quality,
                                    int publish_width, int publish_height, bool draw_result)
{
    publisher->capture = capture;
    publisher->results = results;
    publisher->frames = frames;
    publisher->target_fps = target_fps;
    publisher->jpeg_quality = jpeg_quality;
    publisher->publish_width = publish_width;
    publisher->publish_height = publish_height;
    publisher->draw_result = draw_result;
    if (pthread_create(&publisher->thread, NULL, display_publisher_thread, publisher) != 0) {
        printf("display: pthread_create failed\n");
        publisher->thread = 0;
        return false;
    }
    printf("stream-rknn: display publisher target_fps=%d publish_size=%dx%d\n",
           target_fps, publish_width, publish_height);
    return true;
}

static void stop_display_publisher(DisplayPublisher *publisher)
{
    if (publisher->thread != 0) {
        pthread_join(publisher->thread, NULL);
        publisher->thread = 0;
    }
}

static bool send_all(int fd, const void *data, size_t len)
{
    const unsigned char *ptr = reinterpret_cast<const unsigned char *>(data);
    int retry_count = 0;
    while (len > 0) {
        ssize_t written = send(fd, ptr, len, MSG_NOSIGNAL);
        if (written < 0) {
            if (errno == EINTR) {
                continue;
            }
            if ((errno == EAGAIN || errno == EWOULDBLOCK) && retry_count++ < 20) {
                fd_set writefds;
                FD_ZERO(&writefds);
                FD_SET(fd, &writefds);
                struct timeval timeout;
                timeout.tv_sec = 0;
                timeout.tv_usec = 100000;
                int ready = select(fd + 1, NULL, &writefds, NULL, &timeout);
                if (ready > 0) {
                    continue;
                }
                if (ready < 0 && errno == EINTR) {
                    continue;
                }
                continue;
            }
            return false;
        }
        if (written == 0) {
            return false;
        }
        retry_count = 0;
        ptr += written;
        len -= (size_t)written;
    }
    return true;
}

static bool send_text(int fd, const char *text)
{
    return send_all(fd, text, strlen(text));
}

static bool send_u32_be(int fd, uint32_t value)
{
    uint32_t net_value = htonl(value);
    return send_all(fd, &net_value, sizeof(net_value));
}

static void put_be16(unsigned char *dst, uint16_t value)
{
    dst[0] = (unsigned char)((value >> 8) & 0xff);
    dst[1] = (unsigned char)(value & 0xff);
}

static void put_be32(unsigned char *dst, uint32_t value)
{
    dst[0] = (unsigned char)((value >> 24) & 0xff);
    dst[1] = (unsigned char)((value >> 16) & 0xff);
    dst[2] = (unsigned char)((value >> 8) & 0xff);
    dst[3] = (unsigned char)(value & 0xff);
}

static void close_client(int fd)
{
    if (fd >= 0) {
        close(fd);
    }
}

static void configure_stream_socket(int fd)
{
    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
    struct timeval timeout;
    timeout.tv_sec = 1;
    timeout.tv_usec = 0;
    setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    int flags = fcntl(fd, F_GETFL, 0);
    if (flags >= 0) {
        fcntl(fd, F_SETFL, flags | O_NONBLOCK);
    }
}

static int connect_tcp_ipv4(const char *host, int port)
{
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        return -1;
    }

    sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons((uint16_t)port);
    if (inet_pton(AF_INET, host, &addr.sin_addr) != 1) {
        close(fd);
        errno = EINVAL;
        return -1;
    }

    int flags = fcntl(fd, F_GETFL, 0);
    if (flags < 0) {
        close(fd);
        return -1;
    }
    if (fcntl(fd, F_SETFL, flags | O_NONBLOCK) != 0) {
        close(fd);
        return -1;
    }
    int conn_ret = connect(fd, reinterpret_cast<sockaddr *>(&addr), sizeof(addr));
    if (conn_ret != 0 && errno != EINPROGRESS) {
        close(fd);
        return -1;
    }
    if (conn_ret != 0) {
        fd_set writefds;
        FD_ZERO(&writefds);
        FD_SET(fd, &writefds);
        struct timeval connect_timeout;
        connect_timeout.tv_sec = 2;
        connect_timeout.tv_usec = 0;
        int ready = select(fd + 1, NULL, &writefds, NULL, &connect_timeout);
        if (ready <= 0) {
            close(fd);
            errno = ETIMEDOUT;
            return -1;
        }
        int err = 0;
        socklen_t err_len = sizeof(err);
        if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &err, &err_len) != 0 || err != 0) {
            close(fd);
            errno = err != 0 ? err : errno;
            return -1;
        }
    }
    if (fcntl(fd, F_SETFL, flags) != 0) {
        close(fd);
        return -1;
    }
    struct timeval timeout;
    timeout.tv_sec = 2;
    timeout.tv_usec = 0;
    setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    return fd;
}

static void sleep_interruptible_ms(int ms)
{
    const int step_ms = 100;
    int slept = 0;
    while (g_running && slept < ms) {
        int chunk = std::min(step_ms, ms - slept);
        usleep((useconds_t)chunk * 1000);
        slept += chunk;
    }
}

static bool read_http_request(int fd, char *buf, size_t size)
{
    size_t used = 0;
    while (used + 1 < size) {
        ssize_t got = recv(fd, buf + used, size - used - 1, 0);
        if (got < 0) {
            if (errno == EINTR) {
                continue;
            }
            if (errno == EAGAIN || errno == EWOULDBLOCK) {
                fd_set readfds;
                FD_ZERO(&readfds);
                FD_SET(fd, &readfds);
                struct timeval timeout;
                timeout.tv_sec = 1;
                timeout.tv_usec = 0;
                int ready = select(fd + 1, &readfds, NULL, NULL, &timeout);
                if (ready > 0) {
                    continue;
                }
                if (ready < 0 && errno == EINTR) {
                    continue;
                }
            }
            return false;
        }
        if (got == 0) {
            break;
        }
        used += (size_t)got;
        buf[used] = '\0';
        if (strstr(buf, "\r\n\r\n") != NULL || strstr(buf, "\n\n") != NULL) {
            return true;
        }
    }
    buf[used] = '\0';
    return used > 0;
}

static bool snapshot_frame(HttpFrameState *state, std::vector<unsigned char> *jpeg, uint64_t *frame_id, int *width, int *height, int *detections)
{
    std::lock_guard<std::mutex> guard(state->mutex);
    if (state->jpeg.empty()) {
        return false;
    }
    *jpeg = state->jpeg;
    *frame_id = state->frame_id;
    *width = state->width;
    *height = state->height;
    *detections = state->detections;
    return true;
}

static void handle_snapshot_client(int fd, HttpFrameState *state)
{
    std::vector<unsigned char> jpeg;
    uint64_t frame_id = 0;
    int width = 0;
    int height = 0;
    int detections = 0;
    if (!snapshot_frame(state, &jpeg, &frame_id, &width, &height, &detections)) {
        send_text(fd, "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nno frame available\n");
        close_client(fd);
        return;
    }

    char header[512];
    snprintf(header, sizeof(header),
             "HTTP/1.1 200 OK\r\n"
             "Content-Type: image/jpeg\r\n"
             "Content-Length: %zu\r\n"
             "Cache-Control: no-cache\r\n"
             "X-Frame-Id: %llu\r\n"
             "X-Detections: %d\r\n"
             "Connection: close\r\n\r\n",
             jpeg.size(),
             (unsigned long long)frame_id,
             detections);
    send_text(fd, header);
    send_all(fd, jpeg.data(), jpeg.size());
    close_client(fd);
}

static void handle_stream_client(int fd, HttpFrameState *state, int target_fps)
{
    if (!send_text(fd,
                   "HTTP/1.1 200 OK\r\n"
                   "Content-Type: multipart/x-mixed-replace; boundary=frame\r\n"
                   "Cache-Control: no-cache\r\n"
                   "Pragma: no-cache\r\n"
                   "Connection: close\r\n\r\n")) {
        close_client(fd);
        return;
    }

    uint64_t last_frame = 0;
    const int sleep_us = target_fps > 0 ? 1000000 / target_fps : 100000;
    while (g_running && state->running) {
        std::vector<unsigned char> jpeg;
        uint64_t frame_id = 0;
        int width = 0;
        int height = 0;
        int detections = 0;
        {
            std::unique_lock<std::mutex> lock(state->mutex);
            state->updated.wait_for(lock, std::chrono::milliseconds(100), [&] {
                return !g_running || !state->running || (!state->jpeg.empty() && state->frame_id != last_frame);
            });
            if (!g_running || !state->running) {
                break;
            }
            if (state->jpeg.empty() || state->frame_id == last_frame) {
                continue;
            }
            jpeg = state->jpeg;
            frame_id = state->frame_id;
            width = state->width;
            height = state->height;
            detections = state->detections;
        }
        last_frame = frame_id;

        char part_header[512];
        snprintf(part_header, sizeof(part_header),
                 "--frame\r\n"
                 "Content-Type: image/jpeg\r\n"
                 "Content-Length: %zu\r\n"
                 "X-Frame-Id: %llu\r\n"
                 "X-Size: %dx%d\r\n"
                 "X-Detections: %d\r\n\r\n",
                 jpeg.size(),
                 (unsigned long long)frame_id,
                 width,
                 height,
                 detections);
        if (!send_text(fd, part_header) ||
            !send_all(fd, jpeg.data(), jpeg.size()) ||
            !send_text(fd, "\r\n")) {
            break;
        }
        if (sleep_us > 0) {
            usleep(sleep_us);
        }
    }
    close_client(fd);
}

static void *http_client_thread(void *arg)
{
    HttpClient *client = reinterpret_cast<HttpClient *>(arg);
    int fd = client->fd;
    HttpFrameState *frames = client->frames;
    int target_fps = client->target_fps;
    std::atomic<int> *active_clients = client->active_clients;
    delete client;

    char request[2048];
    if (!read_http_request(fd, request, sizeof(request))) {
        close_client(fd);
        if (active_clients != NULL) {
            active_clients->fetch_sub(1);
        }
        return NULL;
    }
    char method[16] = {0};
    char path[256] = {0};
    if (sscanf(request, "%15s %255s", method, path) != 2) {
        close_client(fd);
        if (active_clients != NULL) {
            active_clients->fetch_sub(1);
        }
        return NULL;
    }
    char *query = strchr(path, '?');
    if (query != NULL) {
        *query = '\0';
    }

    if (strcmp(method, "GET") == 0 && strcmp(path, "/stream.mjpg") == 0) {
        handle_stream_client(fd, frames, target_fps);
    } else if (strcmp(method, "GET") == 0 && strcmp(path, "/snapshot.jpg") == 0) {
        handle_snapshot_client(fd, frames);
    } else {
        send_text(fd,
                  "HTTP/1.1 200 OK\r\n"
                  "Content-Type: text/html\r\n"
                  "Connection: close\r\n\r\n"
                  "<!doctype html><html><head><title>RKNN YOLOv8 Stream</title></head>"
                  "<body style=\"margin:0;background:#111;color:#eee;font-family:sans-serif;overflow:hidden\">"
                  "<img src=\"/stream.mjpg\" style=\"width:100vw;height:100vh;object-fit:contain;display:block\">"
                  "</body></html>\n");
        close_client(fd);
    }
    if (active_clients != NULL) {
        active_clients->fetch_sub(1);
    }
    return NULL;
}

static void *http_server_thread(void *arg)
{
    HttpServer *server = reinterpret_cast<HttpServer *>(arg);
    const int listen_fd = server->listen_fd;
    while (g_running && server->frames->running) {
        fd_set readfds;
        FD_ZERO(&readfds);
        FD_SET(listen_fd, &readfds);
        struct timeval timeout;
        timeout.tv_sec = 0;
        timeout.tv_usec = 200000;
        int ready = select(listen_fd + 1, &readfds, NULL, NULL, &timeout);
        if (ready < 0) {
            if (errno == EINTR) {
                continue;
            }
            printf("http: select failed: %s\n", strerror(errno));
            break;
        }
        if (ready == 0) {
            sched_yield();
            continue;
        }

        int fd = accept(listen_fd, NULL, NULL);
        if (fd < 0) {
            if (!g_running || !server->frames->running) {
                break;
            }
            if (errno == EINTR) {
                continue;
            }
            printf("http: accept failed: %s\n", strerror(errno));
            continue;
        }
        configure_stream_socket(fd);
        sched_yield();

        int active = server->active_clients.load();
        if (active >= server->max_clients) {
            send_text(fd,
                      "HTTP/1.1 503 Service Unavailable\r\n"
                      "Content-Type: text/plain\r\n"
                      "Connection: close\r\n\r\n"
                      "too many clients\n");
            close_client(fd);
            continue;
        }
        server->active_clients.fetch_add(1);

        HttpClient *client = new HttpClient;
        if (client == NULL) {
            server->active_clients.fetch_sub(1);
            close_client(fd);
            continue;
        }
        client->fd = fd;
        client->target_fps = server->target_fps;
        client->frames = server->frames;
        client->active_clients = &server->active_clients;
        pthread_t thread;
        if (pthread_create(&thread, NULL, http_client_thread, client) != 0) {
            delete client;
            server->active_clients.fetch_sub(1);
            close_client(fd);
            continue;
        }
        pthread_detach(thread);
    }
    return NULL;
}

static bool start_http_server(HttpServer *server, HttpFrameState *frames, int port, int target_fps)
{
    server->port = port;
    server->target_fps = target_fps;
    server->frames = frames;
    server->active_clients.store(0);
    server->listen_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (server->listen_fd < 0) {
        printf("http: socket failed: %s\n", strerror(errno));
        return false;
    }

    int one = 1;
    setsockopt(server->listen_fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

    sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_ANY);
    addr.sin_port = htons((uint16_t)port);
    if (bind(server->listen_fd, reinterpret_cast<sockaddr *>(&addr), sizeof(addr)) != 0) {
        printf("http: bind 0.0.0.0:%d failed: %s\n", port, strerror(errno));
        close(server->listen_fd);
        server->listen_fd = -1;
        return false;
    }
    if (listen(server->listen_fd, 64) != 0) {
        printf("http: listen failed: %s\n", strerror(errno));
        close(server->listen_fd);
        server->listen_fd = -1;
        return false;
    }

    frames->running = true;
    if (pthread_create(&server->thread, NULL, http_server_thread, server) != 0) {
        printf("http: pthread_create failed\n");
        close(server->listen_fd);
        server->listen_fd = -1;
        return false;
    }

    printf("stream-rknn: HTTP MJPEG server listening on 0.0.0.0:%d\n", port);
    printf("stream-rknn: open http://<board-ip>:%d/stream.mjpg or /snapshot.jpg\n", port);
    return true;
}

static void stop_http_server(HttpServer *server, HttpFrameState *frames)
{
    {
        std::lock_guard<std::mutex> guard(frames->mutex);
        frames->running = false;
        frames->updated.notify_all();
    }
    if (server->listen_fd >= 0) {
        shutdown(server->listen_fd, SHUT_RDWR);
        close(server->listen_fd);
    }
    if (server->thread != 0) {
        pthread_join(server->thread, NULL);
        server->thread = 0;
    }
    server->listen_fd = -1;
}

static bool wait_next_frame(HttpFrameState *state, uint64_t last_frame, std::vector<unsigned char> *jpeg, uint64_t *frame_id)
{
    std::unique_lock<std::mutex> lock(state->mutex);
    state->updated.wait_for(lock, std::chrono::milliseconds(100), [&] {
        return !g_running || !state->running || (!state->jpeg.empty() && state->frame_id != last_frame);
    });
    if (!g_running || !state->running || state->jpeg.empty() || state->frame_id == last_frame) {
        return false;
    }
    *jpeg = state->jpeg;
    *frame_id = state->frame_id;
    return true;
}

static void *push_client_thread(void *arg)
{
    PushClient *client = reinterpret_cast<PushClient *>(arg);
    HttpFrameState *frames = client->frames;
    const int sleep_us = client->target_fps > 0 ? 1000000 / client->target_fps : 100000;
    double last_error_log = 0.0;

    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) {
        printf("push: udp socket failed: %s\n", strerror(errno));
        fflush(stdout);
        return NULL;
    }

    sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons((uint16_t)client->port);
    if (inet_pton(AF_INET, client->host.c_str(), &addr.sin_addr) != 1) {
        printf("push: invalid udp host %s\n", client->host.c_str());
        fflush(stdout);
        close(fd);
        return NULL;
    }

    printf("push: sending chunked UDP JPEG frames to relay %s:%d\n", client->host.c_str(), client->port);
    fflush(stdout);

    const size_t header_size = 20;
    const size_t payload_limit = 1200;
    std::vector<unsigned char> packet(header_size + payload_limit);
    memcpy(packet.data(), "SRKU", 4);

    uint64_t last_frame = 0;
    while (g_running && frames->running) {
        std::vector<unsigned char> jpeg;
        uint64_t frame_id = 0;
        if (!wait_next_frame(frames, last_frame, &jpeg, &frame_id)) {
            continue;
        }
        last_frame = frame_id;
        if (jpeg.size() > 256 * 1024) {
            client->send_errors++;
            continue;
        }

        const size_t chunk_count = (jpeg.size() + payload_limit - 1) / payload_limit;
        if (chunk_count == 0 || chunk_count > 256) {
            client->send_errors++;
            continue;
        }

        bool ok = true;
        uint32_t frame32 = (uint32_t)frame_id;
        uint32_t total_len = (uint32_t)jpeg.size();
        for (size_t chunk_index = 0; chunk_index < chunk_count; ++chunk_index) {
            size_t offset = chunk_index * payload_limit;
            size_t chunk_len = std::min(payload_limit, jpeg.size() - offset);
            put_be32(packet.data() + 4, frame32);
            put_be32(packet.data() + 8, total_len);
            put_be16(packet.data() + 12, (uint16_t)chunk_index);
            put_be16(packet.data() + 14, (uint16_t)chunk_count);
            put_be16(packet.data() + 16, (uint16_t)chunk_len);
            put_be16(packet.data() + 18, 0);
            memcpy(packet.data() + header_size, jpeg.data() + offset, chunk_len);
            ssize_t sent = sendto(fd, packet.data(), header_size + chunk_len, 0,
                                  reinterpret_cast<sockaddr *>(&addr), sizeof(addr));
            if (sent < 0) {
                ok = false;
                client->send_errors++;
                double now = monotonic_sec();
                if (now - last_error_log >= 5.0) {
                    printf("push: udp sendto %s:%d failed: %s\n", client->host.c_str(), client->port, strerror(errno));
                    fflush(stdout);
                    last_error_log = now;
                }
                break;
            }
            client->sent_packets++;
        }

        if (ok) {
            client->sent++;
        } else {
            double now = monotonic_sec();
            if (now - last_error_log >= 5.0) {
                printf("push: frame send incomplete frame=%llu chunks=%zu sent_frames=%llu sent_packets=%llu errors=%llu\n",
                       (unsigned long long)frame_id,
                       chunk_count,
                       (unsigned long long)client->sent,
                       (unsigned long long)client->sent_packets,
                       (unsigned long long)client->send_errors);
                fflush(stdout);
                last_error_log = now;
            }
        }
        if (sleep_us > 0) {
            usleep(sleep_us);
        }
    }
    close(fd);
    return NULL;
}

static bool start_push_client(PushClient *client, HttpFrameState *frames, const char *host, int port, int target_fps)
{
    client->host = host;
    client->port = port;
    client->target_fps = target_fps;
    client->frames = frames;
    if (pthread_create(&client->thread, NULL, push_client_thread, client) != 0) {
        printf("push: pthread_create failed\n");
        client->thread = 0;
        return false;
    }
    printf("stream-rknn: pushing MJPEG frames to %s:%d\n", host, port);
    return true;
}

static void stop_push_client(PushClient *client, HttpFrameState *frames)
{
    {
        std::lock_guard<std::mutex> guard(frames->mutex);
        frames->running = false;
        frames->updated.notify_all();
    }
    if (client->thread != 0) {
        pthread_join(client->thread, NULL);
        client->thread = 0;
    }
}

static bool wait_next_frame(HttpFrameState *state, uint64_t last_frame, std::vector<unsigned char> *jpeg, uint64_t *frame_id);

static void print_serial_jpeg_frame(uint64_t frame_id, const std::vector<unsigned char> &jpeg)
{
    static const char table[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    printf("STARRY_JPEG_BEGIN frame=%llu bytes=%zu\n", (unsigned long long)frame_id, jpeg.size());
    for (size_t i = 0; i < jpeg.size(); i += 3) {
        unsigned int b0 = jpeg[i];
        unsigned int b1 = (i + 1 < jpeg.size()) ? jpeg[i + 1] : 0;
        unsigned int b2 = (i + 2 < jpeg.size()) ? jpeg[i + 2] : 0;
        putchar(table[(b0 >> 2) & 0x3f]);
        putchar(table[((b0 & 0x03) << 4) | ((b1 >> 4) & 0x0f)]);
        putchar(i + 1 < jpeg.size() ? table[((b1 & 0x0f) << 2) | ((b2 >> 6) & 0x03)] : '=');
        putchar(i + 2 < jpeg.size() ? table[b2 & 0x3f] : '=');
        if (((i / 3) + 1) % 19 == 0) {
            putchar('\n');
        }
    }
    putchar('\n');
    printf("STARRY_JPEG_END frame=%llu\n", (unsigned long long)frame_id);
    fflush(stdout);
}

static void *serial_publisher_thread(void *arg)
{
    SerialPublisher *publisher = reinterpret_cast<SerialPublisher *>(arg);
    HttpFrameState *frames = publisher->frames;
    const int sleep_us = publisher->target_fps > 0 ? 1000000 / publisher->target_fps : 1000000;
    uint64_t last_frame = 0;
    while (g_running && frames->running) {
        std::vector<unsigned char> jpeg;
        uint64_t frame_id = 0;
        if (!wait_next_frame(frames, last_frame, &jpeg, &frame_id)) {
            continue;
        }
        last_frame = frame_id;
        print_serial_jpeg_frame(frame_id, jpeg);
        if (sleep_us > 0) {
            usleep(sleep_us);
        }
    }
    return NULL;
}

static bool start_serial_publisher(SerialPublisher *publisher, HttpFrameState *frames, int target_fps)
{
    publisher->frames = frames;
    publisher->target_fps = target_fps;
    if (pthread_create(&publisher->thread, NULL, serial_publisher_thread, publisher) != 0) {
        printf("serial: pthread_create failed\n");
        publisher->thread = 0;
        return false;
    }
    printf("stream-rknn: serial JPEG publisher target_fps=%d\n", target_fps);
    return true;
}

static void stop_serial_publisher(SerialPublisher *publisher, HttpFrameState *frames)
{
    {
        std::lock_guard<std::mutex> guard(frames->mutex);
        frames->running = false;
        frames->updated.notify_all();
    }
    if (publisher->thread != 0) {
        pthread_join(publisher->thread, NULL);
        publisher->thread = 0;
    }
}

static void print_result(int inference_index, uint64_t frame_id, uint32_t sequence, double latency_ms, const object_detect_result_list *results)
{
    printf("YOLO_INFER index=%d frame=%llu sequence=%u latency_ms=%.2f detections=%d\n",
           inference_index,
           (unsigned long long)frame_id,
           sequence,
           latency_ms,
           results->count);

    if (results->count == 0) {
        printf("YOLO_RESULT index=%d frame=%llu detections=0\n",
               inference_index,
               (unsigned long long)frame_id);
        fflush(stdout);
        return;
    }

    for (int i = 0; i < results->count; ++i) {
        const object_detect_result *det = &results->results[i];
        int box_width = det->box.right - det->box.left;
        int box_height = det->box.bottom - det->box.top;
        int center_x = det->box.left + box_width / 2;
        int center_y = det->box.top + box_height / 2;
        printf("YOLO_RESULT index=%d det=%d class=%s confidence=%.1f%% box=(%d,%d,%d,%d) center=(%d,%d) size=%dx%d\n",
               inference_index,
               i,
               coco_cls_to_name(det->cls_id),
               det->prop * 100.0f,
               det->box.left,
               det->box.top,
               det->box.right,
               det->box.bottom,
               center_x,
               center_y,
               box_width,
               box_height);
    }
    fflush(stdout);
}

static void print_usage(const char *argv0)
{
    printf("Usage: %s [OPTIONS]\n", argv0);
    printf("  --model <PATH>          RKNN model [default: model/yolov8.rknn]\n");
    printf("  --label <PATH>          label file [default: model/coco_80_labels_list.txt]\n");
    printf("  --device <INDEX>        UVC device index [default: 0]\n");
    printf("  --width <PIXELS>        frame width [default: 320]\n");
    printf("  --height <PIXELS>       frame height [default: 240]\n");
    printf("  --fps <FPS>             camera FPS [default: 10]\n");
    printf("  --duration-sec <SECS>   run duration, 0 means forever [default: 0]\n");
    printf("  --infer-every <N>       infer every Nth captured frame [default: 3]\n");
    printf("  --max-inferences <N>    stop after N successful inferences\n");
    printf("  --http-port <PORT>      serve annotated MJPEG stream, 0 disables [default: 8080]\n");
    printf("  --http-fps <FPS>        max MJPEG response FPS [default: 8]\n");
    printf("  --publish-width <PX>    resize published MJPEG width, 0 keeps camera width [default: 0]\n");
    printf("  --publish-height <PX>   resize published MJPEG height, 0 keeps camera height [default: 0]\n");
    printf("  --push-host <IP>        push annotated JPEG frames to relay host\n");
    printf("  --push-port <PORT>      relay ingest UDP port [default: 18080]\n");
    printf("  --push-fps <FPS>        max relay push FPS [default: 3]\n");
    printf("  --serial-fps <FPS>      print annotated JPEG frames as base64 on stdout [default: 0]\n");
    printf("  --log-every <N>         print YOLO_RESULT every N inferences [default: 10]\n");
    printf("  --min-confidence <PCT>  detection threshold percentage [default: 55]\n");
    printf("  --jpeg-quality <1-100>  MJPEG JPEG quality [default: 75]\n");
    printf("  --no-draw-result        publish raw frames without result boxes/text\n");
}

static bool parse_int_arg(const char *name, const char *value, int *out)
{
    if (value[0] == '\0') {
        printf("invalid value for %s: %s\n", name, value);
        return false;
    }
    int parsed = 0;
    for (const char *p = value; *p != '\0'; ++p) {
        if (*p < '0' || *p > '9') {
            printf("invalid value for %s: %s\n", name, value);
            return false;
        }
        parsed = parsed * 10 + (*p - '0');
        if (parsed > 1000000) {
            printf("invalid value for %s: %s\n", name, value);
            return false;
        }
    }
    if (parsed <= 0) {
        printf("invalid value for %s: %s\n", name, value);
        return false;
    }
    *out = parsed;
    return true;
}

static bool parse_nonnegative_int_arg(const char *name, const char *value, int *out)
{
    if (value[0] == '\0') {
        printf("invalid value for %s: %s\n", name, value);
        return false;
    }
    int parsed = 0;
    for (const char *p = value; *p != '\0'; ++p) {
        if (*p < '0' || *p > '9') {
            printf("invalid value for %s: %s\n", name, value);
            return false;
        }
        parsed = parsed * 10 + (*p - '0');
        if (parsed > 1000000) {
            printf("invalid value for %s: %s\n", name, value);
            return false;
        }
    }
    *out = parsed;
    return true;
}

static bool parse_args(int argc, char **argv, Options *options)
{
    for (int i = 1; i < argc; ++i) {
        const char *arg = argv[i];
        const char *value = NULL;
        if (strcmp(arg, "-h") == 0 || strcmp(arg, "--help") == 0) {
            print_usage(argv[0]);
            exit(0);
        }
        if (i + 1 < argc) {
            value = argv[i + 1];
        }

        if (strcmp(arg, "--model") == 0 && value != NULL) {
            options->model_path = value;
            ++i;
        } else if (strcmp(arg, "--label") == 0 && value != NULL) {
            options->label_path = value;
            ++i;
        } else if (strcmp(arg, "--device") == 0 && value != NULL) {
            if (!parse_nonnegative_int_arg(arg, value, &options->device)) return false;
            ++i;
        } else if (strcmp(arg, "--width") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->width)) return false;
            ++i;
        } else if (strcmp(arg, "--height") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->height)) return false;
            ++i;
        } else if (strcmp(arg, "--fps") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->fps)) return false;
            ++i;
        } else if (strcmp(arg, "--duration-sec") == 0 && value != NULL) {
            if (!parse_nonnegative_int_arg(arg, value, &options->duration_sec)) return false;
            ++i;
        } else if (strcmp(arg, "--infer-every") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->infer_every)) return false;
            ++i;
        } else if (strcmp(arg, "--max-inferences") == 0 && value != NULL) {
            if (!parse_nonnegative_int_arg(arg, value, &options->max_inferences)) return false;
            ++i;
        } else if (strcmp(arg, "--http-port") == 0 && value != NULL) {
            if (!parse_nonnegative_int_arg(arg, value, &options->http_port)) return false;
            if (options->http_port > 65535) {
                printf("invalid value for %s: %s\n", arg, value);
                return false;
            }
            ++i;
        } else if (strcmp(arg, "--http-fps") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->http_fps)) return false;
            ++i;
        } else if (strcmp(arg, "--publish-width") == 0 && value != NULL) {
            if (!parse_nonnegative_int_arg(arg, value, &options->publish_width)) return false;
            ++i;
        } else if (strcmp(arg, "--publish-height") == 0 && value != NULL) {
            if (!parse_nonnegative_int_arg(arg, value, &options->publish_height)) return false;
            ++i;
        } else if (strcmp(arg, "--push-host") == 0 && value != NULL) {
            options->push_host = value;
            ++i;
        } else if (strcmp(arg, "--push-port") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->push_port)) return false;
            if (options->push_port > 65535) {
                printf("invalid value for %s: %s\n", arg, value);
                return false;
            }
            ++i;
        } else if (strcmp(arg, "--push-fps") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->push_fps)) return false;
            ++i;
        } else if (strcmp(arg, "--serial-fps") == 0 && value != NULL) {
            if (!parse_nonnegative_int_arg(arg, value, &options->serial_fps)) return false;
            ++i;
        } else if (strcmp(arg, "--log-every") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->log_every)) return false;
            ++i;
        } else if (strcmp(arg, "--min-confidence") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->min_confidence)) return false;
            if (options->min_confidence < 1 || options->min_confidence > 99) {
                printf("invalid value for %s: %s\n", arg, value);
                return false;
            }
            ++i;
        } else if (strcmp(arg, "--jpeg-quality") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->jpeg_quality)) return false;
            if (options->jpeg_quality < 1 || options->jpeg_quality > 100) {
                printf("invalid value for %s: %s\n", arg, value);
                return false;
            }
            ++i;
        } else if (strcmp(arg, "--no-draw-result") == 0) {
            options->draw_result = false;
        } else {
            printf("unknown or incomplete argument: %s\n", arg);
            return false;
        }
    }
    return true;
}

int main(int argc, char **argv)
{
    Options options;
    if (!parse_args(argc, argv, &options)) {
        print_usage(argv[0]);
        return 2;
    }

    signal(SIGINT, signal_handler);
    signal(SIGTERM, signal_handler);
    signal(SIGPIPE, SIG_IGN);

    printf("YOLOv8 UVC Streaming Detection\n");
    printf("===============================\n");
    printf("model=%s label=%s device=%d size=%dx%d fps=%d duration=%d infer_every=%d max_inferences=%d http_port=%d http_fps=%d publish_size=%dx%d push_host=%s push_port=%d push_fps=%d serial_fps=%d log_every=%d min_confidence=%d jpeg_quality=%d draw_result=%d\n",
           options.model_path,
           options.label_path,
           options.device,
           options.width,
           options.height,
           options.fps,
           options.duration_sec,
           options.infer_every,
           options.max_inferences,
           options.http_port,
           options.http_fps,
           options.publish_width,
           options.publish_height,
           options.push_host != NULL ? options.push_host : "",
           options.push_port,
           options.push_fps,
           options.serial_fps,
           options.log_every,
           options.min_confidence,
           options.jpeg_quality,
           options.draw_result ? 1 : 0);

    int ret = init_post_process(options.label_path);
    if (ret != 0) {
        printf("init_post_process fail! ret=%d label_path=%s\n", ret, options.label_path);
        return 1;
    }

    rknn_app_context_t app_ctx;
    memset(&app_ctx, 0, sizeof(app_ctx));
    ret = init_yolov8_model(options.model_path, &app_ctx);
    if (ret != 0) {
        printf("init_yolov8_model fail! ret=%d model_path=%s\n", ret, options.model_path);
        deinit_post_process();
        return 1;
    }
    printf("stream-rknn: init_yolov8_model success width=%d height=%d channel=%d\n",
           app_ctx.model_width,
           app_ctx.model_height,
           app_ctx.model_channel);

    UvcCaptureSession capture;
    UvcCaptureOptions capture_options;
    capture_options.device = options.device;
    capture_options.width = options.width;
    capture_options.height = options.height;
    capture_options.fps = options.fps;
    capture_options.log_prefix = "stream-rknn";
    if (!start_uvc_capture(&capture, &capture_options)) {
        release_yolov8_model(&app_ctx);
        deinit_post_process();
        return 1;
    }
    SharedState *state = &capture.state;

    HttpFrameState http_frames;
    ResultState result_state;
    memset(&result_state.results, 0, sizeof(result_state.results));
    HttpServer http_server;
    bool http_started = false;
    PushClient push_client;
    bool push_started = false;
    SerialPublisher serial_publisher;
    bool serial_started = false;
    DisplayPublisher display_publisher;
    bool display_started = false;
    if (options.http_port > 0 || options.push_host != NULL || options.serial_fps > 0) {
        std::lock_guard<std::mutex> guard(http_frames.mutex);
        http_frames.running = true;
    }
    if (options.http_port > 0) {
        http_started = start_http_server(&http_server, &http_frames, options.http_port, options.http_fps);
        if (!http_started) {
            stop_uvc_capture(&capture);
            release_yolov8_model(&app_ctx);
            deinit_post_process();
            return 1;
        }
    }
    if (options.push_host != NULL) {
        push_started = start_push_client(&push_client, &http_frames, options.push_host, options.push_port, options.push_fps);
        if (!push_started) {
            if (http_started) {
                stop_http_server(&http_server, &http_frames);
            }
            stop_uvc_capture(&capture);
            release_yolov8_model(&app_ctx);
            deinit_post_process();
            return 1;
        }
    }
    if (options.serial_fps > 0) {
        serial_started = start_serial_publisher(&serial_publisher, &http_frames, options.serial_fps);
        if (!serial_started) {
            if (push_started) {
                stop_push_client(&push_client, &http_frames);
            }
            if (http_started) {
                stop_http_server(&http_server, &http_frames);
            }
            stop_uvc_capture(&capture);
            release_yolov8_model(&app_ctx);
            deinit_post_process();
            return 1;
        }
    }
    if (http_started || push_started || serial_started) {
        int display_fps = options.serial_fps > 0 ? options.serial_fps : (options.push_host != NULL ? options.push_fps : options.http_fps);
        display_started = start_display_publisher(&display_publisher, state, &result_state, &http_frames,
                                                  display_fps, options.jpeg_quality,
                                                  options.publish_width, options.publish_height,
                                                  options.draw_result);
        if (!display_started) {
            if (push_started) {
                stop_push_client(&push_client, &http_frames);
            }
            if (serial_started) {
                stop_serial_publisher(&serial_publisher, &http_frames);
            }
            if (http_started) {
                stop_http_server(&http_server, &http_frames);
            }
            stop_uvc_capture(&capture);
            release_yolov8_model(&app_ctx);
            deinit_post_process();
            return 1;
        }
    }

    const double start = monotonic_sec();
    double last_report = start;
    uint64_t last_report_captured = 0;
    uint64_t last_inferred_frame = 0;
    const uint64_t infer_interval = (uint64_t)options.infer_every;
    int inferences = 0;
    int decode_errors = 0;
    int inference_errors = 0;

    while (g_running && (options.duration_sec == 0 || monotonic_sec() - start < options.duration_sec)) {
        LatestFrame frame;
        {
            std::lock_guard<std::mutex> guard(state->mutex);
            if (state->latest.id != 0 && state->latest.id != last_inferred_frame &&
                state->latest.id >= last_inferred_frame + infer_interval) {
                frame = state->latest;
            }
        }

        if (frame.id == 0) {
            usleep(10000);
        } else {
            image_buffer_t image;
            memset(&image, 0, sizeof(image));
            double decode_start = monotonic_sec();
            if (frame_to_image(frame, &image) != 0) {
                decode_errors++;
                last_inferred_frame = frame.id;
                continue;
            }
            object_detect_result_list results;
            memset(&results, 0, sizeof(results));
            double infer_start = monotonic_sec();
            ret = inference_yolov8_model_with_thresholds(
                &app_ctx,
                &image,
                &results,
                (float)options.min_confidence / 100.0f,
                NMS_THRESH);
            double infer_end = monotonic_sec();
            last_inferred_frame = frame.id;
            if (ret != 0) {
                printf("inference_yolov8_model fail! ret=%d frame=%llu\n", ret, (unsigned long long)frame.id);
                inference_errors++;
            } else {
                inferences++;
                {
                    std::lock_guard<std::mutex> guard(result_state.mutex);
                    result_state.results = results;
                    result_state.frame_id = frame.id;
                }
                if (options.log_every > 0 && (inferences == 1 || inferences % options.log_every == 0)) {
                    printf("stream-rknn: decode_ms=%.2f ", (infer_start - decode_start) * 1000.0);
                    print_result(inferences, frame.id, frame.sequence, (infer_end - infer_start) * 1000.0, &results);
                }
            }
            free(image.virt_addr);
            sched_yield();
            usleep(20000);
            if (options.max_inferences > 0 && inferences >= options.max_inferences) {
                break;
            }
        }

        double now = monotonic_sec();
        if (now - last_report >= 1.0) {
            uint64_t captured = 0;
            uint64_t bytes = 0;
            uint64_t dropped = 0;
            {
                UvcCaptureCounters counters = capture_counters(state);
                captured = counters.captured;
                bytes = counters.bytes;
                dropped = counters.dropped;
            }
            double interval = now - last_report;
            uint64_t delta = captured - last_report_captured;
            printf("stream-rknn: capture_fps=%.2f captured=%llu inferred=%d dropped_latest=%llu mib_s=%.2f elapsed=%.1f\n",
                   (double)delta / interval,
                   (unsigned long long)captured,
                   inferences,
                   (unsigned long long)dropped,
                   (double)bytes / (now - start) / 1024.0 / 1024.0,
                   now - start);
            last_report = now;
            last_report_captured = captured;
        }
    }

    if (push_started) {
        stop_push_client(&push_client, &http_frames);
    } else if (serial_started) {
        stop_serial_publisher(&serial_publisher, &http_frames);
    } else if (http_started) {
        std::lock_guard<std::mutex> guard(http_frames.mutex);
        http_frames.running = false;
        http_frames.updated.notify_all();
    }
    if (display_started) {
        stop_display_publisher(&display_publisher);
    }
    if (http_started) {
        stop_http_server(&http_server, &http_frames);
    }

    stop_uvc_capture(&capture);

    double elapsed = monotonic_sec() - start;
    uint64_t captured = 0;
    uint64_t bytes = 0;
    uint64_t dropped = 0;
    {
        UvcCaptureCounters counters = capture_counters(state);
        captured = counters.captured;
        bytes = counters.bytes;
        dropped = counters.dropped;
    }

    int release_ret = release_yolov8_model(&app_ctx);
    deinit_post_process();

    printf("stream-rknn: done duration_sec=%.1f captured=%llu capture_fps=%.2f inferences=%d infer_fps=%.2f dropped_latest=%llu decode_errors=%d inference_errors=%d bytes=%llu\n",
           elapsed,
           (unsigned long long)captured,
           (double)captured / std::max(elapsed, 0.001),
           inferences,
           (double)inferences / std::max(elapsed, 0.001),
           (unsigned long long)dropped,
           decode_errors,
           inference_errors,
           (unsigned long long)bytes);

    if (release_ret == 0 && inferences > 0 && inference_errors == 0) {
        printf("UVC_RKNN_STREAM_DONE\n");
        return 0;
    }
    return 1;
}
