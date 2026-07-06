#!/usr/bin/env python3
# Minimal HTTPS echo backend (upstream-TLS target) for host validation.
import ssl
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(sys.argv[1])
CERT = sys.argv[2]
KEY = sys.argv[3]


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"BACKEND=backend_tls\nUPSTREAM_TLS=ok\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


httpd = HTTPServer(("127.0.0.1", PORT), Handler)
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(CERT, KEY)
httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)
httpd.serve_forever()
