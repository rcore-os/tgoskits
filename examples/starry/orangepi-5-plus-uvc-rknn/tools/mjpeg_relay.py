#!/usr/bin/env python3
import argparse
import http.server
import socket
import socketserver
import struct
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


def read_exact(sock, size):
    chunks = []
    remaining = size
    while remaining:
        data = sock.recv(remaining)
        if not data:
            return None
        chunks.append(data)
        remaining -= len(data)
    return b"".join(chunks)


def handle_ingest(conn, addr, store):
    try:
        magic = read_exact(conn, len(b"SRKNMJPG1\n"))
        if magic != b"SRKNMJPG1\n":
            print(f"relay: reject {addr[0]}:{addr[1]} invalid magic", flush=True)
            return
        print(f"relay: ingest connected from {addr[0]}:{addr[1]}", flush=True)
        frames = 0
        started = time.time()
        while True:
            header = read_exact(conn, 4)
            if header is None:
                break
            size = struct.unpack("!I", header)[0]
            if size == 0 or size > 64 * 1024 * 1024:
                print(f"relay: invalid frame size {size}", flush=True)
                break
            jpeg = read_exact(conn, size)
            if jpeg is None:
                break
            store.publish(jpeg)
            frames += 1
            now = time.time()
            if now - started >= 5.0:
                print(f"relay: ingest_fps={frames / (now - started):.2f} latest_bytes={len(jpeg)}", flush=True)
                frames = 0
                started = now
    finally:
        conn.close()
        print(f"relay: ingest disconnected from {addr[0]}:{addr[1]}", flush=True)


def ingest_server(host, port, store):
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind((host, port))
    srv.listen(4)
    print(f"relay: ingest listening on {host}:{port}", flush=True)
    while True:
        conn, addr = srv.accept()
        thread = threading.Thread(target=handle_ingest, args=(conn, addr, store), daemon=True)
        thread.start()


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
