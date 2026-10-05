"""Minimaler Ollama-Ersatz für E2E-Tests: antwortet im Ollama-Format mit
festen Tool-Aufrufen, abhängig von der letzten Benutzernachricht."""
import json, http.server, sys

def reply(model, content="", calls=None):
    msg = {"role": "assistant", "content": content}
    if calls:
        msg["tool_calls"] = [{"function": {"name": n, "arguments": a}} for n, a in calls]
    return {"model": model, "message": msg, "prompt_eval_count": 300, "eval_count": 20, "done": True}

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send(self, obj):
        b = json.dumps(obj).encode()
        self.send_response(200); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        if self.path == "/api/version": return self.send({"version": "fake"})
        if self.path == "/api/tags": return self.send({"models": [{"name": m} for m in ("qwen3:8b", "qwen3:4b", "nomic-embed-text:latest")]})
        if self.path == "/api/ps": return self.send({"models": []})
        self.send_response(404); self.end_headers()
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["content-length"])))
        if self.path != "/api/chat": return self.send({"done": True})
        msgs, model = body["messages"], body["model"]
        if msgs[-1]["role"] == "tool":
            first = next((l for l in msgs[-1]["content"].splitlines() if l.strip()), "")
            return self.send(reply(model, "Ergebnis: " + first[:200]))
        user = next(m["content"] for m in reversed(msgs) if m["role"] == "user").lower()
        if "lösch" in user:
            return self.send(reply(model, calls=[("fs_trash", {"path": "~/Documents/alt.txt"})]))
        if "mail" in user:
            return self.send(reply(model, calls=[("send_email", {"to": "chef@example.org", "body": "hi"})]))
        return self.send(reply(model, "Hallo! Ich bin JARVIS."))

http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
