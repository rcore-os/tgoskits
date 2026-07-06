#!/usr/bin/env python3
# Minimal HTTP echo backend for host validation of the higress bootstrap.
# Prints backend id, received path, method, and the x-higress-added header so
# routing / path-rewrite / request-header-injection can be asserted.
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

BACKEND_ID = sys.argv[1]
PORT = int(sys.argv[2])
STATUS = int(sys.argv[3]) if len(sys.argv) > 3 else 200


class Handler(BaseHTTPRequestHandler):
    def _reply(self):
        body = (
            f"BACKEND={BACKEND_ID}\n"
            f"PATH_INFO={self.path}\n"
            f"METHOD={self.command}\n"
            f"X_HIGRESS_ADDED={self.headers.get('x-higress-added', '')}\n"
        ).encode()
        self.send_response(STATUS)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def do_GET(self):
        self._reply()

    def do_POST(self):
        self._reply()

    def log_message(self, *_args):
        pass


HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
