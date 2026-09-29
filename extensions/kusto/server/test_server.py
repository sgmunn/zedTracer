import json
import os
import subprocess
import sys
import unittest
from pathlib import Path


DEFAULT_SERVER = Path(__file__).parent / "bin" / "Debug" / "net8.0" / "kusto-lsp.dll"
SERVER = Path(os.environ.get("KUSTO_LSP_BINARY", DEFAULT_SERVER))
SERVER_COMMAND = ["dotnet", str(SERVER)] if SERVER.suffix == ".dll" else [str(SERVER)]
URI = "file:///offline-test.kql"


class LanguageServerTest(unittest.TestCase):
    def setUp(self):
        self.process = subprocess.Popen(
            SERVER_COMMAND,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.send(1, "initialize", {"processId": None, "rootUri": None, "capabilities": {}})
        self.assertIn("capabilities", self.receive()["result"])

    def tearDown(self):
        self.process.terminate()
        try:
            self.process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.communicate()

    def send(self, request_id, method, parameters):
        message = {"jsonrpc": "2.0", "method": method, "params": parameters}
        if request_id is not None:
            message["id"] = request_id
        body = json.dumps(message).encode("utf-8")
        self.process.stdin.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
        self.process.stdin.flush()

    def receive(self):
        headers = {}
        while line := self.process.stdout.readline():
            if line == b"\r\n":
                break
            name, value = line.decode("ascii").split(":", 1)
            headers[name.lower()] = value.strip()
        self.assertIn("content-length", headers)
        return json.loads(self.process.stdout.read(int(headers["content-length"])))

    def open_document(self, text):
        self.send(
            None,
            "textDocument/didOpen",
            {"textDocument": {"uri": URI, "languageId": "kusto", "version": 1, "text": text}},
        )
        return self.receive()["params"]["diagnostics"]

    def complete(self, line, character):
        self.send(
            2,
            "textDocument/completion",
            {"textDocument": {"uri": URI}, "position": {"line": line, "character": character}},
        )
        return self.receive()["result"]

    def test_local_name_and_function_completion_replace_prefix(self):
        diagnostics = self.open_document("let threshold = 5;\nStormEvents | where th")
        self.assertIsInstance(diagnostics, list)

        completions = self.complete(1, 22)
        threshold = next(item for item in completions if item["label"] == "threshold")
        self.assertEqual(threshold["textEdit"]["range"], {
            "start": {"line": 1, "character": 20},
            "end": {"line": 1, "character": 22},
        })
        self.assertTrue(threshold["textEdit"]["newText"].startswith("threshold"))

        self.send(
            None,
            "textDocument/didChange",
            {"textDocument": {"uri": URI, "version": 2},
             "contentChanges": [{"text": "StormEvents | extend Length = str"}]},
        )
        self.receive()
        functions = self.complete(0, 33)
        self.assertIn("strlen(string)", [item["label"] for item in functions])

    def test_syntax_diagnostics_and_close(self):
        diagnostics = self.open_document("let = 5;")
        self.assertTrue(diagnostics)
        self.assertEqual(diagnostics[0]["source"], "Kusto")

        self.send(None, "textDocument/didClose", {"textDocument": {"uri": URI}})
        self.assertEqual(self.receive()["params"]["diagnostics"], [])

    def test_hover_on_builtin_function(self):
        self.open_document("print value = strlen('abc')")
        self.send(
            3,
            "textDocument/hover",
            {"textDocument": {"uri": URI}, "position": {"line": 0, "character": 16}},
        )
        hover = self.receive()["result"]
        self.assertIn("strlen", hover["contents"]["value"])

    def test_utf16_positions(self):
        self.open_document("// 😀\nlet threshold = 5;\nStormEvents | where th")
        threshold = next(item for item in self.complete(2, 22) if item["label"] == "threshold")
        self.assertEqual(threshold["textEdit"]["range"]["start"],
                         {"line": 2, "character": 20})


if __name__ == "__main__":
    if not SERVER.exists():
        sys.exit("Build KustoLanguageServer.csproj before running these tests")
    unittest.main()
