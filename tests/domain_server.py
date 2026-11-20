#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A local HTTPS server for the tests of the confirmation of a domain (tests/domain_confirm.py).
Usage: domain_server.py <port> <cert.pem> <key.pem> <control-dir>
It reads <control-dir>/mode on every request and answers:
  file            the text of <control-dir>/body.toml
  redirect-same   a redirect to /moved/panpak.toml on the same host (that path serves the body)
  redirect-other  a redirect to another host
  big             a body of 70000 bytes
  slow            nothing for 30 seconds
  notfound        404
Every request path is appended to <control-dir>/requests.log.
"""
import http.server
import ssl
import sys
import time
from pathlib import Path

PORT, CERT, KEY, CTL = int(sys.argv[1]), sys.argv[2], sys.argv[3], Path(sys.argv[4])


class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *a):
        pass

    def reply(self, status, body=b'', headers=()):
        self.send_response(status)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Connection', 'close')
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        with open(CTL / 'requests.log', 'a') as f:
            f.write(self.path + '\n')
        mode = (CTL / 'mode').read_text().strip()
        body = (CTL / 'body.toml').read_bytes()
        if self.path == '/moved/panpak.toml':
            return self.reply(200, body)
        if mode == 'file':
            self.reply(200, body)
        elif mode == 'redirect-same':
            self.reply(301, b'', [('Location', '/moved/panpak.toml')])
        elif mode == 'redirect-other':
            self.reply(301, b'', [('Location', 'https://evil.test/.well-known/panpak.toml')])
        elif mode == 'big':
            self.reply(200, b'#' * 70000)
        elif mode == 'slow':
            time.sleep(30)
        else:
            self.reply(404)


srv = http.server.ThreadingHTTPServer(('127.0.0.1', PORT), H)
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(CERT, KEY)
srv.socket = ctx.wrap_socket(srv.socket, server_side=True)
srv.serve_forever()
