#!/usr/bin/env python3
import argparse
import http.server
import socket
import socketserver
import threading
import time


class FrameStore:
    def __init__(self):
        self.cond = threading.Condition()
        self.jpeg = None
        self.frame_id = 0
        self.last_update = 0.0

    def publish(self, jpeg):
        with self.cond:
            self.jpeg = jpeg
            self.frame_id += 1
            self.last_update = time.time()
            self.cond.notify_all()

    def wait_next(self, last_frame, timeout):
        deadline = time.time() + timeout
        with self.cond:
            while self.frame_id == last_frame:
                remaining = deadline - time.time()
                if remaining <= 0:
                    return None, last_frame
                self.cond.wait(remaining)
            return self.jpeg, self.frame_id


def be16(data, offset):
    return (data[offset] << 8) | data[offset + 1]


def be32(data, offset):
    return (
        (data[offset] << 24)
        | (data[offset + 1] << 16)
        | (data[offset + 2] << 8)
        | data[offset + 3]
    )


class UdpReassembler:
    def __init__(self):
        self.frames = {}
        self.last_cleanup = time.time()

    def push(self, packet):
        if len(packet) < 20 or packet[:4] != b"SRKU":
            return None
        frame_id = be32(packet, 4)
        total_len = be32(packet, 8)
        chunk_index = be16(packet, 12)
        chunk_count = be16(packet, 14)
        chunk_len = be16(packet, 16)
        payload = packet[20:]
        if (
            total_len == 0
            or total_len > 256 * 1024
            or chunk_count == 0
            or chunk_count > 256
            or chunk_index >= chunk_count
            or chunk_len != len(payload)
        ):
            return None

        now = time.time()
        if now - self.last_cleanup >= 2.0:
            self.frames = {
                key: value for key, value in self.frames.items()
                if now - value["updated"] < 2.0
            }
            self.last_cleanup = now

        frame = self.frames.get(frame_id)
        if frame is None:
            frame = {
                "total_len": total_len,
                "chunk_count": chunk_count,
                "chunks": {},
                "updated": now,
            }
            self.frames[frame_id] = frame
        if frame["total_len"] != total_len or frame["chunk_count"] != chunk_count:
            self.frames.pop(frame_id, None)
            return None
        frame["chunks"][chunk_index] = payload
        frame["updated"] = now
        if len(frame["chunks"]) != chunk_count:
            return None

        jpeg = b"".join(frame["chunks"].get(i, b"") for i in range(chunk_count))
        self.frames.pop(frame_id, None)
        if len(jpeg) != total_len or not (jpeg.startswith(b"\xff\xd8") and jpeg.endswith(b"\xff\xd9")):
            return None
        return jpeg


def ingest_server(host, port, store):
    srv = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind((host, port))
    print(f"relay: chunked UDP ingest listening on {host}:{port}", flush=True)
    reassembler = UdpReassembler()
    frames = 0
    started = time.time()
    while True:
        packet, addr = srv.recvfrom(2048)
        jpeg = reassembler.push(packet)
        if jpeg is None:
            continue
        store.publish(jpeg)
        frames += 1
        now = time.time()
        if now - started >= 5.0:
            print(f"relay: ingest_fps={frames / (now - started):.2f} latest_bytes={len(jpeg)}", flush=True)
            frames = 0
            started = now


class RelayHandler(http.server.BaseHTTPRequestHandler):
    server_version = "StarryMjpegRelay/1.0"

    def log_message(self, fmt, *args):
        print(f"http: {self.address_string()} - {fmt % args}", flush=True)

    def do_GET(self):
        if self.path in ("/", "/stream.mjpg"):
            self.handle_stream()
        elif self.path == "/snapshot.jpg":
            self.handle_snapshot()
        else:
            self.send_error(404)

    def handle_snapshot(self):
        jpeg, frame_id = self.server.store.wait_next(0, 0.001)
        if jpeg is None:
            self.send_response(503)
            self.send_header("Content-Type", "text/plain")
            self.end_headers()
            self.wfile.write(b"no frame available\n")
            return
        self.send_response(200)
        self.send_header("Content-Type", "image/jpeg")
        self.send_header("Content-Length", str(len(jpeg)))
        self.send_header("Cache-Control", "no-cache")
        self.send_header("X-Frame-Id", str(frame_id))
        self.end_headers()
        self.wfile.write(jpeg)

    def handle_stream(self):
        self.send_response(200)
        self.send_header("Content-Type", "multipart/x-mixed-replace; boundary=frame")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Pragma", "no-cache")
        self.end_headers()
        last_frame = 0
        while True:
            jpeg, frame_id = self.server.store.wait_next(last_frame, 10.0)
            if jpeg is None:
                continue
            last_frame = frame_id
            part = (
                b"--frame\r\n"
                b"Content-Type: image/jpeg\r\n" +
                f"Content-Length: {len(jpeg)}\r\n".encode("ascii") +
                f"X-Frame-Id: {frame_id}\r\n\r\n".encode("ascii") +
                jpeg +
                b"\r\n"
            )
            try:
                self.wfile.write(part)
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                break


class ThreadingHttpServer(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


def main():
    parser = argparse.ArgumentParser(description="Receive Starry JPEG push frames and serve them as MJPEG over HTTP.")
    parser.add_argument("--ingest-host", default="0.0.0.0")
    parser.add_argument("--ingest-port", type=int, default=18080)
    parser.add_argument("--http-host", default="0.0.0.0")
    parser.add_argument("--http-port", type=int, default=18081)
    args = parser.parse_args()

    store = FrameStore()
    threading.Thread(target=ingest_server, args=(args.ingest_host, args.ingest_port, store), daemon=True).start()
    httpd = ThreadingHttpServer((args.http_host, args.http_port), RelayHandler)
    httpd.store = store
    print(f"relay: open http://127.0.0.1:{args.http_port}/stream.mjpg", flush=True)
    httpd.serve_forever()


if __name__ == "__main__":
    main()
