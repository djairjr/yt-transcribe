#!/usr/bin/env python3
"""Static file server with Range support, for testing the network route.

`python3 -m http.server` ignores the `Range` header: it answers 200 with the
whole body. An mp4 keeps its `moov` atom at the end, so a player seeks there
first -- without Range, FFmpeg reads the file forward instead of asking for a
206, and the seek path the Internet actually uses stays untested.

Usage:  python3 http_range_server.py [port] [directory]
"""

import functools
import os
import re
import sys
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

RANGE_RE = re.compile(r"bytes=(\d*)-(\d*)")


class RangeHandler(SimpleHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def send_head(self):
        path = self.translate_path(self.path)
        if os.path.isdir(path):
            self.remaining = None
            return super().send_head()
        try:
            handle = open(path, "rb")
        except OSError:
            self.send_error(404, "File not found")
            return None

        size = os.fstat(handle.fileno()).st_size
        header = self.headers.get("Range")
        match = RANGE_RE.fullmatch(header.strip()) if header else None

        if not match:
            self.remaining = None
            self.send_response(200)
            self.send_header("Content-Type", self.guess_type(path))
            self.send_header("Content-Length", str(size))
            self.send_header("Accept-Ranges", "bytes")
            self.end_headers()
            return handle

        start_field, end_field = match.group(1), match.group(2)
        if start_field == "":  # suffix range: bytes=-N
            length = min(int(end_field), size)
            start, end = size - length, size - 1
        else:
            start = int(start_field)
            end = min(int(end_field), size - 1) if end_field else size - 1

        if start > end or start >= size:
            handle.close()
            # A 416 has no body; without Content-Length the persistent connection
            # never gets delimited and the client loses where the next response
            # begins. FFmpeg probes with `bytes=<size>-` on purpose and expects a
            # clean 416 (it recovers and asks again).
            self.send_response(416)
            self.send_header("Content-Range", "bytes */%d" % size)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return None

        handle.seek(start)
        self.remaining = end - start + 1
        self.send_response(206)
        self.send_header("Content-Type", self.guess_type(path))
        self.send_header("Content-Range", "bytes %d-%d/%d" % (start, end, size))
        self.send_header("Content-Length", str(self.remaining))
        self.send_header("Accept-Ranges", "bytes")
        self.end_headers()
        return handle

    def copyfile(self, source, outputfile):
        # A bounded range must stop at the end of the range, not at EOF.
        remaining = getattr(self, "remaining", None)
        if remaining is None:
            return super().copyfile(source, outputfile)
        while remaining > 0:
            chunk = source.read(min(64 * 1024, remaining))
            if not chunk:
                break
            outputfile.write(chunk)
            remaining -= len(chunk)

    def log_message(self, format, *args):
        # Range and User-Agent are logged: they are the evidence that an AVOption
        # set by the client reached the wire, and of exactly which byte range the
        # client asked for when a 416 comes back.
        agent = self.headers.get("User-Agent", "-")
        rng = self.headers.get("Range", "-")
        sys.stderr.write(
            "%s - %s [Range: %s] [UA: %s]\n" % (self.address_string(), format % args, rng, agent)
        )


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8080
    directory = sys.argv[2] if len(sys.argv) > 2 else "."
    handler = functools.partial(RangeHandler, directory=directory)
    # ThreadingHTTPServer, not HTTPServer: FFmpeg keeps one connection open while
    # it opens another (a seek can arrive on a second connection). A
    # single-threaded server blocks writing to the first one -- whose reader has
    # paused -- and never accepts the second, so both sides wait forever.
    with ThreadingHTTPServer(("0.0.0.0", port), handler) as httpd:
        print("serving %s on 0.0.0.0:%d" % (directory, port), flush=True)
        httpd.serve_forever()


if __name__ == "__main__":
    main()
