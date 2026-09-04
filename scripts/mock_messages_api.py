import http.server, json, sys, os, time
MODE = sys.argv[2] if len(sys.argv) > 2 else 'ok'
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        n = int(self.headers.get('content-length', 0)); body = json.loads(self.rfile.read(n))
        mode = open(os.environ.get('MOCK_MODE_FILE', '/dev/null')).read().strip() if os.environ.get('MOCK_MODE_FILE') else MODE
        with open(os.environ.get('MOCK_LOG', '/dev/null'), 'a') as f:
            f.write(json.dumps({'key': self.headers.get('x-api-key'), 'model': body.get('model'), 'n_msgs': len(body['messages']), 'system': bool(body.get('system')), 'fmt': body.get('output_config', {}).get('format', {}).get('type'), 'effort': body.get('output_config', {}).get('effort'), 'user_head': body['messages'][-1]['content'][:80]}) + '\n')
        if mode == '500':
            self.send_response(500); self.send_header('content-type','application/json'); self.end_headers()
            self.wfile.write(b'{"type":"error","error":{"type":"api_error","message":"boom"}}'); return
        if mode == '401':
            self.send_response(401); self.send_header('content-type','application/json'); self.end_headers()
            self.wfile.write(b'{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}'); return
        fmt = body.get('output_config', {}).get('format')
        if fmt:
            text = json.dumps({"label": "Mock cloud task", "project": "sbx", "description": "from the mock"}) if 'description' in json.dumps(fmt['schema']) else json.dumps({"state": "mock state", "next_steps": "mock next"})
        else:
            text = "Mock cloud answer: worked on the sandbox task." if 'Question:' in body['messages'][-1]['content'] else "Mock cloud description of the task."
        self.send_response(200); self.send_header('content-type','text/event-stream'); self.end_headers()
        def ev(name, d): self.wfile.write(f"event: {name}\ndata: {json.dumps(d)}\n\n".encode()); self.wfile.flush()
        ev('message_start', {"type":"message_start","message":{"usage":{"input_tokens":1500,"cache_read_input_tokens":0}}})
        for i in range(0, len(text), 7):
            ev('content_block_delta', {"type":"content_block_delta","delta":{"type":"text_delta","text":text[i:i+7]}}); time.sleep(0.01)
        ev('message_delta', {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":40}})
        ev('message_stop', {"type":"message_stop"})
http.server.ThreadingHTTPServer(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
