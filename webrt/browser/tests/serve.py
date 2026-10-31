#!/usr/bin/env python3
"""测试用的本地 HTTP 服务：提供 tests/pages/，另有一条故意变慢的路径，用来观察加载进度。

    python3 serve.py 端口 目录

/slow/<n>.svg 延迟 n×0.8 秒再返回一张小图；pages/slow.html 引用了好几张，
页面会加载好几秒，地址栏下的“墨迹进度”可以截图。
"""
import functools
import http.server
import re
import sys
import time

SVG = b'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><circle cx="4" cy="4" r="3" fill="#2a2824" opacity=".3"/></svg>'


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        m = re.fullmatch(r"/slow/(\d+)\.svg", self.path)
        if not m:
            return super().do_GET()
        time.sleep(min(int(m.group(1)), 20) * 0.8)
        self.send_response(200)
        self.send_header("Content-Type", "image/svg+xml")
        self.send_header("Content-Length", str(len(SVG)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(SVG)

    def log_message(self, fmt, *args):
        sys.stderr.write("%s\n" % (fmt % args))


if __name__ == "__main__":
    port, directory = int(sys.argv[1]), sys.argv[2]
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), functools.partial(Handler, directory=directory))
    server.serve_forever()
