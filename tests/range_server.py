#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Test HTTP server for `panpak update`: static with Range support.
Usage: range_server.py <dir> <port> [normal|corrupt=<off>|truncate=<off>
    |norange|badrange|loop|drip|huge]
  normal: serves files with correct partial 206s (only if Range present,
    else whole 200).
  corrupt=<off>: flips one byte in every response covering <off>.
  truncate=<off>: cuts every body at <off> bytes (Content-Range stays
    as promised -> the client must fail the check).
  norange: always ignores Range, answers whole 200.
  badrange: incoherent Content-Range (end different from requested).
  loop: 302 to itself, forever (the client must stop at max 3).
  drip: sends 1 byte every 2 seconds (slowloris: the client must
    time out).
  huge: for any Range declares 10 GiB then sends data forever
    (the client must stop at take without allocating)."""
import http.server
import os
import re
import sys
import json

ROOT = None
PORT = None
MODE = "normal"


class H(http.server.SimpleHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def __init__(self, *a, **kw):
        super().__init__(*a, directory=ROOT, **kw)

    def log_message(self, *a):
        pass

    def do_GET(self):
        if MODE == "loop":
            self.send_response(302)
            self.send_header("Location", self.path)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        path = self.translate_path(self.path)
        if not os.path.isfile(path):
            self.send_error(404)
            return
        size = os.path.getsize(path)
        if MODE == "huge":
            # Declares 10 GiB and sends forever: the client must stop.
            self.send_response(206)
            self.send_header("Content-Range", "bytes 0-79/10737418240")
            self.send_header("Content-Length", str(10 * 1024**3))
            self.end_headers()
            try:
                chunk = b"\x00" * 65536
                while True:
                    self.wfile.write(chunk)
            except (BrokenPipeError, ConnectionResetError):
                pass
            return
        if MODE == "drip":
            self.send_response(206)
            self.send_header("Content-Range", f"bytes {size-80}-{size-1}/{size}")
            self.send_header("Content-Length", "80")
            self.end_headers()
            try:
                import time
                with open(path, "rb") as f:
                    data = f.read(80)
                for i in range(len(data)):
                    self.wfile.write(data[i : i + 1])
                    self.wfile.flush()
                    time.sleep(2)
            except (BrokenPipeError, ConnectionResetError):
                pass
            return
        rng = self.headers.get("Range")
        if MODE == "norange" or not rng:
            self.send_response(200)
            self.send_header("Content-Length", str(size))
            self.end_headers()
            with open(path, "rb") as f:
                self.wfile.write(f.read())
            return
        m = re.match(r"bytes=(\d*)-(\d*)$", rng.strip())
        if not m:
            self.send_error(416)
            return
        s, e = m.groups()
        if s == "":
            # Suffix-range: last int(e) bytes.
            n = int(e)
            s = max(0, size - n)
            e = size - 1
        else:
            s = int(s)
            e = int(e) if e else size - 1
        if s >= size:
            self.send_error(416)
            return
        e = min(e, size - 1)
        with open(path, "rb") as f:
            f.seek(s)
            body = f.read(e - s + 1)
        if MODE == "badrange":
            # end shifted by +100 vs the requested one.
            e = e + 100
        if MODE.startswith("corrupt="):
            off = int(MODE.split("=")[1])
            if s <= off <= e:
                body = body[: off - s] + bytes([body[off - s] ^ 0xFF]) + body[off - s + 1 :]
        if MODE.startswith("truncate="):
            off = int(MODE.split("=")[1])
            body = body[: max(0, off - s)]
        self.send_response(206)
        self.send_header("Content-Range", f"bytes {s}-{e}/{size}")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        if os.environ.get("PANPAK_RANGE_LOG"):
            with open(os.environ["PANPAK_RANGE_LOG"], "a") as log:
                log.write(json.dumps({"start": s, "end": e, "bytes": len(body)}) + "\n")


def main():
    global ROOT, PORT, MODE
    ROOT = sys.argv[1]
    PORT = int(sys.argv[2])
    MODE = sys.argv[3] if len(sys.argv) > 3 else "normal"
    http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()


if __name__ == "__main__":
    main()
