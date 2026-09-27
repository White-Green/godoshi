import json
import urllib.request
from http.server import BaseHTTPRequestHandler, HTTPServer


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        outbound_response = None
        if self.path == "/outbound":
            try:
                req = urllib.request.Request("http://outbound.internal/test")
                with urllib.request.urlopen(req, timeout=5) as resp:
                    outbound_response = resp.read().decode("utf-8")
            except Exception as e:
                outbound_response = f"Error: {e}"

        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        response = {
            "status": "ok",
            "message": "Hello from Cloudflare Container!",
            "path": self.path,
            "outbound_response": outbound_response,
        }
        self.wfile.write(json.dumps(response).encode("utf-8"))

    def log_message(self, format, *args):
        print(f"[Container] {self.address_string()} - {format % args}")


if __name__ == "__main__":
    server = HTTPServer(("0.0.0.0", 8080), Handler)
    print("Container listening on port 8080...")
    server.serve_forever()
