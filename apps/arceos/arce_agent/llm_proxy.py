#!/usr/bin/env python3
"""
Simple HTTP-to-HTTPS reverse proxy for ArceAgent testing.

Listens on 127.0.0.1:8080 (plain HTTP) and forwards requests to the
Tsinghua AI Platform HTTPS API. This allows ArceOS (which lacks TLS)
to reach the API via QEMU user-mode networking (10.0.2.2:8080).

The proxy also injects the Authorization header, so the API key does NOT
need to be stored in the Rust binary or transmitted over the unencrypted
QEMU virtual network link.

Usage:
    # Set TARGET_BASE and API_KEY below, then generate a separate proxy token:
    export ARCE_AGENT_PROXY_TOKEN="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"
    python3 llm_proxy.py
    # Start the host ArceAgent with the same environment variable.
    # QEMU clients use http://10.0.2.2:8080/v1/chat/completions with
    # Authorization: Bearer <ARCE_AGENT_PROXY_TOKEN> (never the upstream key).

Keep this proxy local; remote clients need an authenticated encrypted tunnel.
The token authorizes use of the configured upstream account, so configure
spending limits with the provider when sharing access with trusted clients.
Upstream redirects are rejected; configure the final API endpoint directly.
"""

import http.server
import hmac
import os
import urllib.request
import ssl
import json
import sys
import socket
import threading
import time

TARGET_BASE = "" # OpenAI-capable api url, for example, "https://lab.cs.tsinghua.edu.cn/ai-platform/api/v1"
LISTEN_PORT = 8080
LISTEN_HOST = "127.0.0.1"
PROXY_TOKEN = os.environ.get("ARCE_AGENT_PROXY_TOKEN", "")

API_KEY = "" # Paste your API key here

# Bounds apply to incoming headers/body, independently of the upstream timeout.
MAX_BODY_BYTES = 1024 * 1024
READ_TIMEOUT = 5
REQUEST_TIMEOUT = 15
MAX_CONNECTIONS = 16


class ProxyServer(http.server.ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, *args, **kwargs):
        self._slots = threading.BoundedSemaphore(MAX_CONNECTIONS)
        super().__init__(*args, **kwargs)

    def process_request(self, request, client_address):
        # Never block the accept loop or create unbounded worker threads.
        if not self._slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except BaseException:
            self._slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self._slots.release()


class RejectRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        # Neither the upstream key nor the client's proxy token may follow redirects.
        raise urllib.error.URLError("Upstream redirects are disabled")


class ProxyHandler(http.server.BaseHTTPRequestHandler):
    def handle(self):
        self.connection.settimeout(READ_TIMEOUT)
        self._deadline = time.monotonic() + REQUEST_TIMEOUT
        # Interrupt even a trickling request line/header inside stdlib parsing.
        self._timer = threading.Timer(REQUEST_TIMEOUT, self._expire_input)
        self._timer.daemon = True
        self._timer.start()
        try:
            super().handle()
        except (TimeoutError, ConnectionError):
            pass
        finally:
            self._stop_input_timer()

    def _expire_input(self):
        try:
            self.connection.shutdown(socket.SHUT_RD)
        except OSError:
            pass

    def _stop_input_timer(self):
        self._timer.cancel()
        # Join before the socket can be closed or reused by another request.
        self._timer.join()

    def _read_body(self):
        if self.headers.get_all("Transfer-Encoding"):
            self.send_error(400, "Transfer-Encoding is not supported")
            return None
        lengths = self.headers.get_all("Content-Length", [])
        value = lengths[0].strip() if lengths else "0"
        if len(lengths) > 1 or not value or not value.isascii() or not value.isdecimal():
            self.send_error(400, "Invalid Content-Length")
            return None
        # Compare decimal strings before conversion, including very long values.
        value = value.lstrip("0") or "0"
        limit = str(MAX_BODY_BYTES)
        if len(value) > len(limit) or (len(value) == len(limit) and value > limit):
            self.send_error(413, "Request body too large")
            return None
        remaining = int(value)
        body = bytearray()
        try:
            while remaining:
                if time.monotonic() >= self._deadline:
                    raise TimeoutError
                chunk = self.rfile.read1(min(remaining, 64 * 1024))
                if not chunk:
                    if time.monotonic() >= self._deadline:
                        raise TimeoutError
                    self.send_error(400, "Incomplete request body")
                    return None
                body.extend(chunk)
                remaining -= len(chunk)
            if time.monotonic() >= self._deadline:
                raise TimeoutError
        except TimeoutError:
            self.send_error(408, "Request input timed out")
            return None
        return bytes(body)

    def do_POST(self):
        self._proxy()

    def do_GET(self):
        self._proxy()

    def _proxy(self):
        # Authenticate before reading a body or constructing a credentialed request.
        authorization = self.headers.get_all("Authorization", [])
        if (
            not PROXY_TOKEN
            or len(authorization) != 1
            or not hmac.compare_digest(
                authorization[0].encode(), f"Bearer {PROXY_TOKEN}".encode()
            )
        ):
            self.send_response(401)
            self.send_header("WWW-Authenticate", 'Bearer realm="arce-agent-proxy"')
            self.send_header("Content-Length", "0")
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True
            return

        # Close after one request, including when rejecting an unread body.
        self.close_connection = True
        body = self._read_body()
        self._stop_input_timer()
        if body is None:
            return
        body = body or None

        # Build target URL: /v1/chat/completions -> TARGET_BASE/chat/completions
        # Strip /v1 prefix if present since TARGET_BASE already includes /v1
        path = self.path
        if path.startswith("/v1"):
            path = path[3:]  # Remove /v1 prefix
        target_url = TARGET_BASE + path

        # Forward selected headers from the client
        headers = {}
        for key in ("Content-Type", "Accept"):
            val = self.headers.get(key)
            if val:
                headers[key] = val

        # Always inject the API key (overrides any client-provided key)
        headers["Authorization"] = f"Bearer {API_KEY}"

        try:
            req = urllib.request.Request(
                target_url,
                data=body,
                headers=headers,
                method=self.command,
            )
            # Create SSL context that validates certs
            ctx = ssl.create_default_context()
            opener = urllib.request.build_opener(
                urllib.request.HTTPSHandler(context=ctx), RejectRedirects()
            )
            with opener.open(req, timeout=120) as resp:
                resp_body = resp.read()
                self.send_response(resp.status)
                for key, val in resp.getheaders():
                    if key.lower() not in ("transfer-encoding", "connection"):
                        self.send_header(key, val)
                self.send_header("Content-Length", str(len(resp_body)))
                self.end_headers()
                self.wfile.write(resp_body)
        except urllib.error.HTTPError as e:
            error_body = e.read()
            self.send_response(e.code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(error_body)))
            self.end_headers()
            self.wfile.write(error_body)
        except Exception as e:
            error_msg = json.dumps({"error": str(e)}).encode()
            self.send_response(502)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(error_msg)))
            self.end_headers()
            self.wfile.write(error_msg)

    def log_message(self, format, *args):
        print(f"[proxy] {args[0]}", flush=True)


if __name__ == "__main__":
    if not PROXY_TOKEN or any(not 33 <= ord(char) <= 126 for char in PROXY_TOKEN):
        sys.exit("Set ARCE_AGENT_PROXY_TOKEN to a nonempty printable ASCII token without spaces")
    if PROXY_TOKEN == API_KEY:
        sys.exit("ARCE_AGENT_PROXY_TOKEN must differ from the upstream API key")
    server = ProxyServer((LISTEN_HOST, LISTEN_PORT), ProxyHandler)
    print(f"LLM proxy listening on {LISTEN_HOST}:{LISTEN_PORT}", flush=True)
    print(f"Forwarding to {TARGET_BASE}", flush=True)
    print(f"API key injected by proxy (not sent from client)", flush=True)
    print(f"ArceAgent should connect to http://10.0.2.2:{LISTEN_PORT}/v1/...", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nProxy stopped.")
        server.server_close()
