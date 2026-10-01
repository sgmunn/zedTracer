import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


DEFAULT_SERVER = Path(__file__).parent / "bin" / "Debug" / "net8.0" / "kusto-lsp.dll"
SERVER = Path(os.environ.get("KUSTO_LSP_BINARY", DEFAULT_SERVER))
SERVER_COMMAND = ["dotnet", str(SERVER)] if SERVER.suffix == ".dll" else [str(SERVER)]
URI = "file:///offline-test.kql"


class LanguageServerTest(unittest.TestCase):
    def setUp(self):
        self.start_server()

    def start_server(self, root_uri=None, initialization_options=None, environment=None):
        self.process = subprocess.Popen(
            SERVER_COMMAND,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={**os.environ, **(environment or {})},
        )
        parameters = {"processId": None, "rootUri": root_uri, "capabilities": {}}
        if initialization_options is not None:
            parameters["initializationOptions"] = initialization_options
        self.send(1, "initialize", parameters)
        self.assertIn("capabilities", self.receive()["result"])

    def tearDown(self):
        self.stop_server()

    def stop_server(self):
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

    def test_a_completion_with_closing_text_is_a_snippet_that_keeps_the_cursor_inside(self):
        self.open_document("print x = strle")
        function = next(item for item in self.complete(0, 15) if item["label"].startswith("strlen"))
        self.assertEqual(function["insertTextFormat"], 2)
        self.assertEqual(function["textEdit"]["newText"], "strlen($0)")

        self.send(
            None,
            "textDocument/didChange",
            {"textDocument": {"uri": URI, "version": 2},
             "contentChanges": [{"text": "datatable(a:long)[1] | where a be"}]},
        )
        self.receive()
        between = next(item for item in self.complete(0, 33) if item["label"] == "between")
        self.assertEqual(between["textEdit"]["newText"], "between ($0 .. )")

    def test_a_completion_without_closing_text_is_plain_text(self):
        self.open_document("pri")
        keyword = next(item for item in self.complete(0, 3) if item["label"] == "print")
        self.assertEqual(keyword["insertTextFormat"], 1)
        self.assertNotIn("$0", keyword["textEdit"]["newText"])

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

    def test_offline_schema_table_and_column_completion(self):
        with tempfile.TemporaryDirectory() as directory:
            schema_path = Path(directory) / ".kusto-schema.json"
            schema_path.write_text(json.dumps({
                "database": "Samples",
                "tables": {
                    "StormEvents": {"State": "string", "StartTime": "datetime"}
                },
            }))
            self.stop_server()
            self.start_server(Path(directory).as_uri())

            self.open_document("StormEv")
            self.assertIn("StormEvents", [item["label"] for item in self.complete(0, 7)])

            self.send(
                None,
                "textDocument/didChange",
                {"textDocument": {"uri": URI, "version": 2},
                 "contentChanges": [{"text": "StormEvents | where Sta"}]},
            )
            self.receive()
            self.assertIn("State", [item["label"] for item in self.complete(0, 23)])


def management_response(columns, rows):
    return {
        "Tables": [
            {
                "TableName": "Table_0",
                "Columns": [{"ColumnName": name, "ColumnType": "string"} for name in columns],
                "Rows": rows,
            }
        ]
    }


class FakeKusto:
    """A cluster that answers the two management commands the language server sends."""

    ENTITY_COLUMNS = [
        "EntityType", "EntityName", "DatabaseName", "DocString", "Folder",
        "CslInputSchema", "Content", "CslOutputSchema", "Properties",
    ]

    def __init__(self, databases, entities=None, status=200):
        self.databases = databases
        self.entities = entities or {}
        self.status = status
        self.commands = []
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                outer.commands.append((body["db"], body["csl"]))
                if outer.status != 200:
                    payload = {"error": {"message": "denied", "innererror": {"@message": "Not authorized"}}}
                    self.respond(outer.status, payload)
                elif body["csl"].startswith(".show databases entities"):
                    rows = [
                        [kind, name, body["db"], doc, "", parameters, content, schema, {}]
                        for kind, name, doc, parameters, content, schema in outer.entities.get(body["db"], [])
                    ]
                    self.respond(200, management_response(outer.ENTITY_COLUMNS, rows))
                else:
                    rows = [[name, ""] for name in outer.databases]
                    self.respond(200, management_response(["DatabaseName", "PrettyName"], rows))

            def respond(self, status, payload):
                data = json.dumps(payload).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *arguments):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    @property
    def url(self):
        return f"http://127.0.0.1:{self.server.server_address[1]}"

    def stop(self):
        self.server.shutdown()
        self.server.server_close()


STORM_ENTITIES = {
    "Samples": [
        ("Table", "StormEvents", "Storm reports", "", "", "State:string, StartTime:datetime, DamageProperty:long"),
        ("Function", "StatesOver", "States with more than a number of events.", "(minimum:long, region:string)",
         "{ StormEvents | summarize n = count() by State | where n > minimum }", ""),
    ]
}


class SchemaTest(unittest.TestCase):
    def setUp(self):
        self.clusters = []
        self.process = None

    def tearDown(self):
        if self.process:
            self.process.terminate()
            try:
                self.process.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.communicate()
        for cluster in self.clusters:
            cluster.stop()

    def fake(self, *arguments, **keywords):
        cluster = FakeKusto(*arguments, **keywords)
        self.clusters.append(cluster)
        return cluster

    def start(self, endpoints, options=None):
        environment = {
            "KUSTO_LSP_TEST_TOKEN": "test-token",
            "KUSTO_LSP_TEST_ENDPOINTS": json.dumps(
                {f"{name}.kusto.windows.net": cluster.url for name, cluster in endpoints.items()}
            ),
        }
        LanguageServerTest.start_server(self, None, options, environment)

    send = LanguageServerTest.send
    receive = LanguageServerTest.receive
    open_document = LanguageServerTest.open_document
    complete = LanguageServerTest.complete

    def change(self, text, version=2):
        self.send(
            None,
            "textDocument/didChange",
            {"textDocument": {"uri": URI, "version": version}, "contentChanges": [{"text": text}]},
        )
        self.receive()

    def signature_help(self, line, character):
        self.send(
            5,
            "textDocument/signatureHelp",
            {"textDocument": {"uri": URI}, "position": {"line": line, "character": character}},
        )
        return self.receive()["result"]

    def completions_include(self, text, label, timeout=10):
        """Asks again until the schema has loaded, because it is fetched in the background."""
        deadline = time.time() + timeout
        self.change(text)
        while True:
            labels = [item["label"] for item in self.complete(0, len(text))]
            if label in labels or time.time() > deadline:
                return labels
            time.sleep(0.1)

    def test_default_cluster_and_database_complete_tables_and_columns(self):
        cluster = self.fake(["Samples", "Other"], STORM_ENTITIES)
        self.start({"help": cluster}, {"cluster": "https://help.kusto.windows.net", "database": "Samples"})

        self.open_document("Storm")
        self.assertIn("StormEvents", self.completions_include("Storm", "StormEvents"))
        self.assertIn("State", self.completions_include("StormEvents | where Sta", "State"))
        self.assertIn(
            "StatesOver(minimum, region)",
            self.completions_include("States", "StatesOver(minimum, region)"),
        )

    def test_a_query_that_names_another_cluster_loads_that_cluster(self):
        home = self.fake(["Samples"], STORM_ENTITIES)
        away = self.fake(
            ["Logs"],
            {"Logs": [("Table", "Requests", "", "", "", "RequestId:guid, DurationMs:real")]},
        )
        self.start({"home": home, "away": away}, {"cluster": "home", "database": "Samples"})

        self.open_document("print 1")
        query = "cluster('away').database('Logs').Requ"
        self.assertIn("Requests", self.completions_include(query, "Requests"))
        self.assertIn(
            "DurationMs",
            self.completions_include(query + "ests | where Dur", "DurationMs"),
        )
        self.assertTrue(any(command.startswith(".show databases entities") for _, command in away.commands))

    def test_function_signature_help_follows_the_argument(self):
        cluster = self.fake(["Samples"], STORM_ENTITIES)
        self.start({"help": cluster}, {"cluster": "help", "database": "Samples"})
        self.open_document("print 1")
        self.completions_include("States", "StatesOver(minimum, region)")

        help_at_start = self.signature_help_after("StatesOver(")
        signature = help_at_start["signatures"][0]
        self.assertEqual(signature["label"], "StatesOver(minimum: long, region: string)")
        self.assertEqual(signature["documentation"], "States with more than a number of events.")
        self.assertEqual(help_at_start["activeParameter"], 0)

        second = self.signature_help_after("StatesOver(5, ")
        self.assertEqual(second["activeParameter"], 1)
        label = second["signatures"][0]["label"]
        start, end = second["signatures"][0]["parameters"][1]["label"]
        self.assertEqual(label[start:end], "region: string")

    def test_signature_help_ignores_commas_in_nested_calls_and_strings(self):
        self.start({}, {})
        self.open_document("print 1")
        text = "print x = strcat(strlen('a,b'), "
        self.change(text)
        result = self.signature_help(0, len(text))
        self.assertEqual(result["activeParameter"], 1)
        self.assertTrue(result["signatures"][0]["label"].startswith("strcat("))

    def test_signature_help_is_empty_outside_a_call(self):
        self.start({}, {})
        self.open_document("print 1")
        self.change("print 1 + 2")
        self.assertIsNone(self.signature_help(0, 11))

    def signature_help_after(self, text):
        self.change(text)
        return self.signature_help(0, len(text))

    def test_a_cluster_that_refuses_us_does_not_break_completion(self):
        cluster = self.fake(["Samples"], STORM_ENTITIES, status=401)
        self.start({"help": cluster}, {"cluster": "help", "database": "Samples"})
        self.open_document("let threshold = 5;\nStormEvents | where th")
        labels = [item["label"] for item in self.complete(1, 22)]
        self.assertIn("threshold", labels)

    def test_a_failed_load_is_not_retried_on_every_edit(self):
        cluster = self.fake(["Samples"], STORM_ENTITIES, status=401)
        self.start({"help": cluster}, {"cluster": "help", "database": "Samples"})
        self.open_document("print 1")
        for version in range(2, 8):
            self.change(f"print {version}", version)
        time.sleep(0.5)
        self.assertLessEqual(len(cluster.commands), 2, cluster.commands)


if __name__ == "__main__":
    if not SERVER.exists():
        sys.exit("Build KustoLanguageServer.csproj before running these tests")
    unittest.main()
