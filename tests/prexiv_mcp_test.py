#!/usr/bin/env python3
"""Small live smoke test for the PreXiv MCP bridge.

This intentionally exercises only read tools and the no-token write boundary.
It does not create users or manuscripts; account verification belongs to the
Rust app and is covered by the Rust test suite.
"""

import json
import os
import subprocess
import sys
import time

BASE = os.environ.get("BASE", "http://localhost:3000/api/v1")
MCP_CWD = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "mcp"))


class MCPClient:
    def __init__(self):
        env = {**os.environ, "PREXIV_API_URL": BASE}
        env.pop("PREXIV_TOKEN", None)
        self.proc = subprocess.Popen(
            ["node", "server.js"],
            cwd=MCP_CWD,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.next_id = 1
        self._init()

    def _send(self, msg):
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def _recv_until(self, want_id, timeout=8.0):
        start = time.time()
        while time.time() - start < timeout:
            line = self.proc.stdout.readline()
            if not line:
                break
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            if msg.get("id") == want_id:
                return msg
        stderr = ""
        if self.proc.poll() is not None:
            stderr = self.proc.stderr.read()
        raise TimeoutError(f"no MCP response for id={want_id}; stderr={stderr!r}")

    def _init(self):
        self._send(
            {
                "jsonrpc": "2.0",
                "id": self.next_id,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "prexiv-test", "version": "0.1"},
                },
            }
        )
        self._recv_until(self.next_id)
        self.next_id += 1
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def list_tools(self):
        i = self.next_id
        self.next_id += 1
        self._send({"jsonrpc": "2.0", "id": i, "method": "tools/list"})
        return self._recv_until(i)["result"]["tools"]

    def call(self, name, args=None):
        i = self.next_id
        self.next_id += 1
        self._send(
            {
                "jsonrpc": "2.0",
                "id": i,
                "method": "tools/call",
                "params": {"name": name, "arguments": args or {}},
            }
        )
        return self._recv_until(i)

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        self.proc.terminate()
        try:
            self.proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            self.proc.kill()


passed = []
failed = []


def check(name, cond, detail=""):
    if cond:
        passed.append(name)
        print(f"PASS {name}")
    else:
        failed.append((name, detail))
        print(f"FAIL {name} {detail}")


def result_json(msg):
    content = msg.get("result", {}).get("content", [])
    if not content:
        return None
    text = content[0].get("text", "")
    return json.loads(text) if text else None


def main():
    c = MCPClient()
    try:
        tools = c.list_tools()
        names = {t["name"] for t in tools}
        check("tool list includes browse", "prexiv_browse" in names)
        check("tool list includes submit", "prexiv_submit" in names)

        categories_msg = c.call("prexiv_list_categories")
        categories = result_json(categories_msg)
        check(
            "categories read through MCP",
            isinstance(categories, list) and len(categories) >= 10,
            f"got {type(categories).__name__}",
        )

        browse_msg = c.call("prexiv_browse", {"mode": "new", "per": 1})
        browse = result_json(browse_msg)
        check(
            "browse read through MCP",
            isinstance(browse, dict) and isinstance(browse.get("items"), list),
            f"got {browse!r}",
        )

        submit_msg = c.call(
            "prexiv_submit",
            {
                "title": "No-token write should fail",
                "abstract": "This body is intentionally long enough for the schema but should never be submitted because the MCP bridge has no PREXIV_TOKEN.",
                "authors": "PreXiv test",
                "category": "cs.AI",
                "conductor_type": "ai-agent",
                "conductor_ai_model": "test model",
                "source_base64": "XFxkb2N1bWVudGNsYXNze2FydGljbGV9XFxjb250ZW50",
                "source_filename": "main.tex",
            },
        )
        err_text = submit_msg.get("result", {}).get("content", [{}])[0].get("text", "")
        check(
            "write tool requires token",
            submit_msg.get("result", {}).get("isError") is True
            and "PREXIV_TOKEN is not set" in err_text,
            err_text,
        )
    finally:
        c.close()

    if failed:
        print(f"\nFAILED {len(failed)} checks")
        for name, detail in failed:
            print(f" - {name}: {detail}")
        sys.exit(1)
    print(f"\nOK: {len(passed)} checks passed")


if __name__ == "__main__":
    main()
