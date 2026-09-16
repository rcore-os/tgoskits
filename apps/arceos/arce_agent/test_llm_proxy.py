"""Real TCP request-admission tests; no API key or external service required.

Run: python3 -m unittest discover -s apps/arceos/arce_agent -p 'test_llm_proxy.py' -v
"""

import http.client
import http.server
import queue
import socket
import threading
import unittest
from unittest import mock

import llm_proxy


class ProxyTests(unittest.TestCase):
    def setUp(self):
        self.forwarded = queue.Queue()
        forwarded = self.forwarded

        class Upstream(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
                forwarded.put((self.path, self.headers['Authorization'], body))
                self.send_response(200)
                self.send_header('Content-Length', '2')
                self.end_headers()
                self.wfile.write(b'OK')

            def log_message(self, *args):
                pass

        self.upstream = self.start_server(http.server.ThreadingHTTPServer, Upstream)
        self.entered = queue.Queue()
        entered = self.entered

        class Handler(llm_proxy.ProxyHandler):
            def setup(self):
                super().setup()
                entered.put(self.client_address)

            def log_message(self, *args):
                pass

        patcher = mock.patch.multiple(
            llm_proxy, TARGET_BASE='http://127.0.0.1:%d' % self.upstream.server_port,
            API_KEY='test-key', PROXY_TOKEN='local-token', MAX_BODY_BYTES=1024, READ_TIMEOUT=0.5,
            REQUEST_TIMEOUT=1.5, MAX_CONNECTIONS=2, create=True,
        )
        patcher.start()
        self.addCleanup(patcher.stop)
        self.finished = queue.Queue()
        finished = self.finished

        # Keep the same tests runnable against the pre-fix single-threaded server.
        class Server(getattr(llm_proxy, 'ProxyServer', http.server.HTTPServer)):
            def process_request_thread(self, request, client_address):
                try:
                    super().process_request_thread(request, client_address)
                finally:
                    finished.put(client_address)

        self.proxy = self.start_server(Server, Handler)

    def start_server(self, server_type, handler):
        server = server_type(('127.0.0.1', 0), handler)
        thread = threading.Thread(target=server.serve_forever, kwargs={'poll_interval': 0.01}, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(thread.join, 3)
        self.addCleanup(server.shutdown)
        return server

    def connect(self, wait=True):
        client = socket.create_connection(self.proxy.server_address, timeout=3)
        self.addCleanup(client.close)
        if wait:
            self.entered.get(timeout=3)
        return client

    def response(self, client):
        response = http.client.HTTPResponse(client)
        response.begin()
        body = response.read()
        return response.status, body

    def test_body_admission_and_forwarding(self):
        # Reject before reading any body, including ambiguous framing.
        for headers, status in [
            (b'Content-Length: 1025', 413),
            (b'Content-Length: ' + b'9' * 5000, 413),
            (b'Content-Length: -1', 400),
            (b'Content-Length: nope', 400),
            (b'Content-Length: 1\r\nContent-Length: 2', 400),
            (b'Transfer-Encoding: chunked', 400),
        ]:
            with self.subTest(headers=headers):
                client = self.connect()
                client.sendall(b'POST /v1/chat/completions HTTP/1.0\r\nAuthorization: Bearer local-token\r\n' + headers + b'\r\n\r\n')
                try:
                    self.assertEqual(self.response(client)[0], status)
                    self.assertTrue(self.forwarded.empty())
                finally:
                    client.shutdown(socket.SHUT_RDWR)
                    client.close()
        client = self.connect()
        client.sendall(b'POST / HTTP/1.0\r\nAuthorization: Bearer local-token\r\n'
                       b'Content-Length: 10\r\n\r\nx')
        client.shutdown(socket.SHUT_WR)
        self.assertEqual(self.response(client)[0], 400)
        self.assertTrue(self.forwarded.empty())
        for body in (b'', b'x' * 1024):
            client = self.connect()
            client.sendall(b'POST /v1/chat/completions HTTP/1.0\r\nAuthorization: Bearer local-token\r\nContent-Length: '
                           + str(len(body)).encode() + b'\r\n\r\n' + body)
            self.assertEqual(self.response(client), (200, b'OK'))
            self.assertEqual(self.forwarded.get(timeout=3), ('/chat/completions', 'Bearer test-key', body))

    @mock.patch.multiple(llm_proxy, READ_TIMEOUT=2, REQUEST_TIMEOUT=4, create=True)
    def test_slow_body_does_not_block_other_requests(self):
        slow = self.connect()
        slow.sendall(b'POST / HTTP/1.0\r\nAuthorization: Bearer local-token\r\nContent-Length: 10\r\n\r\nx')
        fast = self.connect(wait=False)
        fast.settimeout(1)
        fast.sendall(b'POST /v1/ready HTTP/1.0\r\nAuthorization: Bearer local-token\r\nContent-Length: 0\r\n\r\n')
        self.assertEqual(self.response(fast), (200, b'OK'))
        self.assertEqual(self.response(slow)[0], 408)
        self.assertEqual(self.forwarded.get(timeout=3)[0], '/ready')
        self.assertTrue(self.forwarded.empty())

    def test_total_deadline_stops_trickling_body_and_headers(self):
        for initial in (b'POST / HTTP/1.0\r\nAuthorization: Bearer local-token\r\nContent-Length: 100\r\n\r\n',
                        b'POST / HTTP/1.0\r\nAuthorization: Bearer local-token\r\nX-Slow: '):
            with self.subTest(initial=initial):
                client = self.connect()
                client.sendall(initial)
                stop = threading.Event()

                def trickle():
                    while not stop.wait(0.1):
                        try:
                            client.sendall(b'x')
                        except OSError:
                            break

                sender = threading.Thread(target=trickle)
                sender.start()
                try:
                    # A socket idle timeout alone cannot terminate this stream.
                    data = client.recv(4096)
                    if b'Content-Length' in initial:
                        self.assertIn(b'408', data.split(b'\r\n')[0])
                    self.assertTrue(self.forwarded.empty())
                finally:
                    stop.set()
                    sender.join()
                    client.close()

    def test_connection_capacity_is_released(self):
        clients = [self.connect(), self.connect()]
        overflow = self.connect(wait=False)
        self.assertEqual(overflow.recv(1), b'')
        for client in clients:
            self.assertEqual(client.recv(1), b'')
        for _ in clients:
            self.finished.get(timeout=3)
        # The timed-out handlers must release their admission slots.
        client = self.connect()
        client.sendall(b'POST / HTTP/1.0\r\nAuthorization: Bearer local-token\r\nContent-Length: 0\r\n\r\n')
        self.assertEqual(self.response(client), (200, b'OK'))


if __name__ == '__main__':
    unittest.main()
