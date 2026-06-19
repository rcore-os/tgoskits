#!/usr/bin/env python3
import argparse
import http.server
import os
import re
import sys
import tempfile
import urllib.parse
from pathlib import Path


SAFE_NAME = re.compile(r"^[A-Za-z0-9._+-]+$")


class UploadHandler(http.server.BaseHTTPRequestHandler):
    server_version = "StarryArtifactUpload/1.0"

    def log_message(self, fmt, *args):
        sys.stderr.write("%s - - [%s] %s\n" % (self.client_address[0], self.log_date_time_string(), fmt % args))

    def do_GET(self):
        parsed = urllib.parse.urlparse(self.path)
        if parsed.path == "/health":
            self._send(200, b"ok\n")
            return
        self._send(404, b"not found\n")

    def do_PUT(self):
        self._handle_upload()

    def do_POST(self):
        self._handle_upload()

    def _handle_upload(self):
        parsed = urllib.parse.urlparse(self.path)
        query = urllib.parse.parse_qs(parsed.query)
        token = query.get("token", [""])[0]
        expected_token = self.server.upload_token
        if expected_token and token != expected_token:
            self._send(403, b"bad token\n")
            return

        prefix = "/upload/"
        if not parsed.path.startswith(prefix):
            self._send(404, b"not found\n")
            return

        name = urllib.parse.unquote(parsed.path[len(prefix):])
        if not name or "/" in name or not SAFE_NAME.match(name):
            self._send(400, b"bad artifact name\n")
            return

        length_header = self.headers.get("Content-Length")
        if not length_header:
            self._send(411, b"content length required\n")
            return

        try:
            remaining = int(length_header)
        except ValueError:
            self._send(400, b"bad content length\n")
            return

        out_dir = self.server.output_dir
        final_path = out_dir / name
        fd, tmp_name = tempfile.mkstemp(prefix=f".{name}.", suffix=".tmp", dir=out_dir)
        written = 0
        try:
            with os.fdopen(fd, "wb") as out:
                while remaining:
                    chunk = self.rfile.read(min(1024 * 1024, remaining))
                    if not chunk:
                        raise OSError("short upload")
                    out.write(chunk)
                    written += len(chunk)
                    remaining -= len(chunk)
            os.replace(tmp_name, final_path)
        except Exception as err:
            try:
                os.unlink(tmp_name)
            except OSError:
                pass
            self._send(500, f"upload failed: {err}\n".encode())
            return

        sys.stderr.write(f"uploaded {final_path} ({written} bytes)\n")
        self._send(201, b"uploaded\n")

    def _send(self, status, body):
        self.send_response(status)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bind", default="127.0.0.1")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--dir", required=True)
    parser.add_argument("--token", default="")
    args = parser.parse_args()

    out_dir = Path(args.dir).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)

    class Server(http.server.ThreadingHTTPServer):
        daemon_threads = True

    server = Server((args.bind, args.port), UploadHandler)
    server.output_dir = out_dir
    server.upload_token = args.token
    print(f"artifact upload server listening on {args.bind}:{args.port}, dir={out_dir}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
