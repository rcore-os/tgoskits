#!/usr/bin/env python3
import argparse
import base64
import http.server
import os
import re
import socketserver
import subprocess
import sys
import threading
import time


class FrameStore:
    def __init__(self):
        self.cond = threading.Condition()
        self.jpeg = None
        self.frame_id = 0
        self.source_frame = 0
        self.last_update = 0.0

    def publish(self, jpeg, source_frame):
        with self.cond:
            self.jpeg = jpeg
            self.frame_id += 1
            self.source_frame = source_frame
            self.last_update = time.time()
            self.cond.notify_all()

    def snapshot(self):
        with self.cond:
            return self.jpeg, self.frame_id, self.source_frame, self.last_update

    def wait_next(self, last_frame, timeout):
        deadline = time.time() + timeout
        with self.cond:
            while self.frame_id == last_frame:
                remaining = deadline - time.time()
                if remaining <= 0:
                    return None, last_frame, self.source_frame
                self.cond.wait(remaining)
            return self.jpeg, self.frame_id, self.source_frame


class RelayHandler(http.server.BaseHTTPRequestHandler):
    server_version = "StarrySerialMjpegRelay/1.0"

    def log_message(self, fmt, *args):
        return

    def do_GET(self):
        if self.path in ("/", "/stream.mjpg"):
            self.handle_stream()
        elif self.path == "/snapshot.jpg":
            self.handle_snapshot()
        else:
            self.send_error(404)

    def handle_snapshot(self):
        jpeg, frame_id, source_frame, updated = self.server.store.snapshot()
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
        self.send_header("X-Source-Frame", str(source_frame))
        self.send_header("X-Frame-Age-Ms", str(int((time.time() - updated) * 1000)))
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
            jpeg, frame_id, source_frame = self.server.store.wait_next(last_frame, 10.0)
            if jpeg is None:
                continue
            last_frame = frame_id
            part = (
                b"--frame\r\n"
                b"Content-Type: image/jpeg\r\n"
                + f"Content-Length: {len(jpeg)}\r\n".encode("ascii")
                + f"X-Frame-Id: {frame_id}\r\n".encode("ascii")
                + f"X-Source-Frame: {source_frame}\r\n\r\n".encode("ascii")
                + jpeg
                + b"\r\n"
            )
            try:
                self.wfile.write(part)
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                break


class ThreadingHttpServer(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


def read_starry_output(proc, store, log_path):
    begin_re = re.compile(r"STARRY_JPEG_BEGIN frame=(\d+) bytes=(\d+)")
    end_re = re.compile(r"STARRY_JPEG_END frame=(\d+)")
    capture = False
    capture_frame = 0
    expected_bytes = 0
    chunks = []
    frames = 0
    started = time.time()
    log_file = open(log_path, "a", buffering=1) if log_path else None
    try:
        for raw in proc.stdout:
            line = raw.rstrip("\r\n")
            if log_file:
                log_file.write(raw)
            if not capture:
                match = begin_re.search(line)
                if match:
                    capture = True
                    capture_frame = int(match.group(1))
                    expected_bytes = int(match.group(2))
                    chunks = []
                else:
                    print(line, flush=True)
                continue

            match = end_re.search(line)
            if match:
                try:
                    jpeg = base64.b64decode("".join(chunks), validate=True)
                except Exception as exc:
                    print(f"relay: base64 decode failed for source frame {capture_frame}: {exc}", flush=True)
                else:
                    if (
                        len(jpeg) == expected_bytes
                        and jpeg.startswith(b"\xff\xd8")
                        and jpeg.endswith(b"\xff\xd9")
                    ):
                        store.publish(jpeg, capture_frame)
                        frames += 1
                    else:
                        print(
                            f"relay: invalid JPEG source_frame={capture_frame} expected={expected_bytes} got={len(jpeg)}",
                            flush=True,
                        )
                capture = False
                now = time.time()
                if now - started >= 5.0:
                    fps = frames / (now - started)
                    _, frame_id, source_frame, updated = store.snapshot()
                    age_ms = int((now - updated) * 1000) if updated else -1
                    print(
                        f"relay: serial_fps={fps:.2f} relay_frame={frame_id} source_frame={source_frame} age_ms={age_ms}",
                        flush=True,
                    )
                    frames = 0
                    started = now
                continue

            chunks.append(line.strip())
    finally:
        if log_file:
            log_file.close()


def main():
    parser = argparse.ArgumentParser(description="Run Starry and serve serial JPEG frames as MJPEG over HTTP.")
    parser.add_argument("--http-host", default="0.0.0.0")
    parser.add_argument("--http-port", type=int, default=18081)
    parser.add_argument("--log", default="/tmp/starry-serial-mjpeg.log")
    parser.add_argument("starry_cmd", nargs=argparse.REMAINDER)
    args = parser.parse_args()

    if not args.starry_cmd:
        raise SystemExit("missing command after --")
    cmd = args.starry_cmd
    if cmd and cmd[0] == "--":
        cmd = cmd[1:]

    store = FrameStore()
    httpd = ThreadingHttpServer((args.http_host, args.http_port), RelayHandler)
    httpd.store = store
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    print(f"relay: open http://127.0.0.1:{args.http_port}/stream.mjpg", flush=True)
    print(f"relay: running {' '.join(cmd)}", flush=True)

    proc = subprocess.Popen(
        cmd,
        cwd=os.getcwd(),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    try:
        read_starry_output(proc, store, args.log)
    except KeyboardInterrupt:
        pass
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


if __name__ == "__main__":
    main()
