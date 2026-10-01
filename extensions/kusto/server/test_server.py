import datetime
import json
import os
import select
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
        result = self.receive()["result"]
        self.assertIn("capabilities", result)
        self.capabilities = result["capabilities"]

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

    def test_a_query_does_not_continue_into_the_next_one(self):
        # With the text after the blank line read as part of the first query, the last column of
        # the project is reported as followed by something that is not a comma.
        diagnostics = self.open_document("T1\n| project a, b\n\nT2\n| take 1")
        self.assertEqual(diagnostics, [])

    def test_a_syntax_error_in_a_later_query_is_reported_where_it_is(self):
        diagnostics = self.open_document("print 1\n\nprint 2\n\nlet = 5;")
        self.assertTrue(diagnostics)
        self.assertEqual(diagnostics[0]["range"]["start"]["line"], 4)
        self.assertEqual(diagnostics[0]["range"]["start"]["character"], 4)

    def test_names_declared_in_one_query_are_not_offered_in_the_next(self):
        self.open_document("let threshold = 5;\nprint threshold\n\nprint x = thr")
        labels = [item["label"] for item in self.complete(3, 13)]
        self.assertNotIn("threshold", labels)

    def test_completion_in_a_later_query_replaces_the_text_being_typed(self):
        self.open_document("print 1\n\nprint x = strle")
        strlen = next(
            item for item in self.complete(2, 15) if item["label"].startswith("strlen")
        )
        self.assertEqual(
            strlen["textEdit"]["range"],
            {"start": {"line": 2, "character": 10}, "end": {"line": 2, "character": 15}},
        )

    def test_a_blank_line_between_queries_starts_a_fresh_query(self):
        self.open_document("print 1\n\n")
        labels = [item["label"] for item in self.complete(2, 0)]
        self.assertIn("print", labels)

    def test_hover_and_signature_help_work_in_a_later_query(self):
        self.open_document("print 1\n\nprint value = strlen('abc')")
        self.send(
            3,
            "textDocument/hover",
            {"textDocument": {"uri": URI}, "position": {"line": 2, "character": 17}},
        )
        self.assertIn("strlen", self.receive()["result"]["contents"]["value"])
        self.send(
            4,
            "textDocument/signatureHelp",
            {"textDocument": {"uri": URI}, "position": {"line": 2, "character": 24}},
        )
        help_ = self.receive()["result"]
        self.assertTrue(help_["signatures"][0]["label"].startswith("strlen("))

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


SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
SPINNING = rf"^[{SPINNER}] Running… \d+ s$"


class CodeLensTest(unittest.TestCase):
    """Lenses above each query, from what the editor recorded about its runs."""

    QUERIES = "T1\n| take 1\n\nT2\n| count\n"

    def setUp(self):
        self.data = tempfile.TemporaryDirectory()
        self.log = Path(self.data.name) / "kusto" / "history" / "runs.jsonl"
        self.log.parent.mkdir(parents=True)
        # The recorded runs are of help.kusto.windows.net / Samples, so these queries run there.
        # Schema loading is pointed at a port nothing listens on, so it fails fast.
        LanguageServerTest.start_server(
            self,
            None,
            {"cluster": "help", "database": "Samples"},
            {
                "KUSTO_ZED_DATA_DIR": self.data.name,
                "KUSTO_LSP_TEST_TOKEN": "unused",
                "KUSTO_LSP_TEST_ENDPOINTS": json.dumps(
                    {"help.kusto.windows.net": "http://127.0.0.1:9"}
                ),
            },
        )

    def tearDown(self):
        self.process.terminate()
        try:
            self.process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.communicate()
        self.data.cleanup()

    send = LanguageServerTest.send
    receive = LanguageServerTest.receive

    def receive_where(self, wanted):
        """The next message that `wanted` accepts. The server may ask for a refresh at any time."""
        while True:
            message = self.receive()
            if wanted(message):
                return message

    def open_document(self, text):
        self.send(
            None,
            "textDocument/didOpen",
            {"textDocument": {"uri": URI, "languageId": "kusto", "version": 1, "text": text}},
        )
        return self.receive_where(
            lambda message: message.get("method") == "textDocument/publishDiagnostics"
        )

    def request(self, request_id, method, parameters):
        self.send(request_id, method, parameters)
        return self.receive_where(lambda message: message.get("id") == request_id and "result" in message)["result"]

    def record(self, event, run_id, query, minutes_ago=0, **fields):
        started = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(minutes=minutes_ago)
        line = {
            "event": event,
            "cid": run_id,
            "query": query,
            "cluster": "help.kusto.windows.net",
            "database": "Samples",
            "at": started.isoformat().replace("+00:00", "Z"),
            **fields,
        }
        with self.log.open("a") as file:
            file.write(json.dumps(line) + "\n")

    def lenses(self, text=None):
        self.open_document(text or self.QUERIES)
        return self.request(7, "textDocument/codeLens", {"textDocument": {"uri": URI}})

    def titles(self, lenses, line):
        """The lens titles on a line, without the one that says where the query runs."""
        return [
            lens["command"]["title"]
            for lens in lenses
            if lens["range"]["start"]["line"] == line
            and lens["command"]["command"] != "kusto.connection"
        ]

    def connection_titles(self, lenses):
        return [
            lens["command"]["title"]
            for lens in lenses
            if lens["command"]["command"] == "kusto.connection"
        ]

    def wait_for_refresh(self, timeout=10):
        """The server asks the client to ask again when the log changes."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            ready, _, _ = select.select([self.process.stdout], [], [], 0.2)
            if ready:
                message = self.receive()
                if message.get("method") == "workspace/codeLens/refresh":
                    return True
        return False

    def test_each_query_has_a_run_lens_on_its_first_line(self):
        lenses = self.lenses()
        self.assertEqual(self.titles(lenses, 0), ["▶ Run"])
        self.assertEqual(self.titles(lenses, 3), ["▶ Run"])
        self.assertEqual(
            self.connection_titles(lenses),
            ["help.kusto.windows.net / Samples"] * 2,
        )
        command = lenses[0]["command"]
        self.assertEqual(command["command"], "zed.dispatchAction")
        self.assertEqual(command["arguments"], ["kusto::RunQuery"])

    def test_the_last_run_of_the_same_query_is_shown_even_when_it_was_reformatted(self):
        self.record("started", "id-1", "T1 | take 1")
        self.record(
            "finished", "id-1", "T1 | take 1",
            durationMs=1840, rows=1240, path="/history/a.ktt",
        )
        lenses = self.lenses("T1\n| take 1 // the first ten\n\nT2\n| count\n")

        first = self.titles(lenses, 0)
        self.assertEqual(first[0], "▶ Run")
        self.assertTrue(first[1].startswith("Last run: "), first)
        self.assertTrue(first[1].endswith("took 1.8 s, 1,240 rows"), first)
        self.assertEqual(first[2:], ["Results", "Copy CID"])
        self.assertEqual(self.titles(lenses, 3), ["▶ Run"], "another query has no history")

        by_title = {lens["command"]["title"]: lens["command"] for lens in lenses}
        self.assertEqual(
            by_title["Results"]["arguments"],
            ["kusto::ShowResult", {"path": "/history/a.ktt"}],
        )
        self.assertEqual(
            by_title["Copy CID"]["arguments"],
            ["kusto::CopyClientRequestId", {"id": "id-1"}],
        )

    def test_the_same_text_on_another_cluster_is_another_query(self):
        self.record("started", "id-1", "T1\n| take 1")
        self.record("finished", "id-1", "T1\n| take 1", durationMs=900, rows=7, path="/history/a.ktt")

        here = self.titles(self.lenses(), 0)
        self.assertTrue(here[1].startswith("Last run: "), here)

        elsewhere = self.titles(
            self.lenses('//:setDefaultCluster("other")\n//:setDefaultDb("Samples")\nT1\n| take 1\n'),
            0,
        )
        self.assertEqual(elsewhere, ["▶ Run"], "no history on that cluster")

    def test_a_running_query_offers_cancel_instead_of_run(self):
        self.record("started", "id-1", "T1\n| take 1")
        lenses = self.lenses()
        first = self.titles(lenses, 0)
        self.assertRegex(first[0], SPINNING)
        self.assertEqual(first[1:], ["Cancel"])
        cancel = next(lens for lens in lenses if lens["command"]["title"] == "Cancel")
        self.assertEqual(cancel["command"]["arguments"], ["kusto::CancelQuery"])
        self.assertEqual(self.titles(lenses, 3), ["▶ Run"])

    def test_the_server_asks_for_fresh_lenses_when_a_run_ends(self):
        self.record("started", "id-1", "T1\n| take 1")
        self.assertRegex(self.titles(self.lenses(), 0)[0], SPINNING)

        self.record("finished", "id-1", "T1\n| take 1", durationMs=500, rows=3, path="/history/a.ktt")
        self.assertTrue(self.wait_for_refresh(), "no refresh request arrived")

        titles = self.titles(
            self.request(8, "textDocument/codeLens", {"textDocument": {"uri": URI}}), 0
        )
        self.assertEqual(titles[0], "▶ Run")
        self.assertTrue(titles[1].endswith("took 500 ms, 3 rows"), titles)

    def test_the_running_lens_shows_the_elapsed_time(self):
        self.record("started", "id-1", "T1\n| take 1", minutes_ago=2)
        title = self.titles(self.lenses(), 0)[0]
        self.assertRegex(title, rf"^[{SPINNER}] Running… 2 m 0\d s$")

    def test_lenses_are_refreshed_repeatedly_while_a_query_runs_and_stop_when_it_ends(self):
        self.record("started", "id-1", "T1\n| take 1")
        self.lenses()

        refreshes = 0
        deadline = time.time() + 3
        while refreshes < 5 and time.time() < deadline:
            if self.wait_for_refresh(timeout=1):
                refreshes += 1
        self.assertGreaterEqual(refreshes, 5, "the spinner needs repeated refreshes")

        self.record("finished", "id-1", "T1\n| take 1", durationMs=500, rows=1, path="/history/a.ktt")
        # One refresh announces the change; once it has been seen the refreshing stops.
        self.assertTrue(self.wait_for_refresh())
        time.sleep(0.6)
        while self.wait_for_refresh(timeout=0.1):
            pass
        self.assertFalse(self.wait_for_refresh(timeout=1.2), "refreshing went on after the run ended")

    def test_a_failed_run_shows_the_first_line_of_its_message(self):
        self.record("started", "id-1", "T1\n| take 1")
        self.record(
            "failed", "id-1", "T1\n| take 1",
            message="Semantic error: SEM0100: bad table\nmore details",
        )
        titles = self.titles(self.lenses(), 0)
        self.assertEqual(titles[0], "▶ Run")
        self.assertEqual(titles[1], "Last run failed: Semantic error: SEM0100: bad table")
        self.assertEqual(titles[2], "Copy CID")

    def test_a_cancelled_run_leaves_the_earlier_result_in_place(self):
        self.record("finished", "id-1", "T1\n| take 1", durationMs=900, rows=7, path="/history/a.ktt")
        self.record("started", "id-2", "T1\n| take 1")
        self.record("cancelled", "id-2", "T1\n| take 1")
        titles = self.titles(self.lenses(), 0)
        self.assertEqual(titles[0], "▶ Run")
        self.assertTrue(titles[1].endswith("took 900 ms, 7 rows"), titles)

    def test_a_run_that_never_reported_an_end_stops_counting_as_running(self):
        self.record("started", "id-1", "T1\n| take 1", minutes_ago=60)
        self.assertEqual(self.titles(self.lenses(), 0), ["▶ Run"])

    def test_without_a_log_there_are_only_run_lenses_and_the_connection(self):
        self.assertFalse(self.log.exists())
        lenses = self.lenses()
        self.assertEqual(len(lenses), 4)
        self.assertEqual(self.titles(lenses, 0), ["▶ Run"])

    def test_the_noop_command_is_accepted(self):
        self.assertIsNone(
            self.request(9, "workspace/executeCommand", {"command": "kusto.noop", "arguments": []})
        )


CASES = Path(__file__).resolve().parents[3] / "fork-docs" / "samples" / "connection-directives.json"


def expected_title(connection):
    """What the connection lens says, which the editor and the server must agree on."""
    cluster, database = connection.get("cluster"), connection.get("database")
    if cluster is None:
        return "no cluster"
    host = cluster.split("://")[-1].rstrip("/").lower()
    if ".kusto." not in host:
        host += ".kusto.windows.net"
    return f"{host} / {database or 'no database'}"


class DirectiveTest(unittest.TestCase):
    """Where a query runs, from `//:setDefaultCluster(...)` and `//:setDefaultDb(...)` lines."""

    def setUp(self):
        self.process = None

    def tearDown(self):
        if self.process:
            self.process.terminate()
            try:
                self.process.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.communicate()

    send = LanguageServerTest.send
    receive = LanguageServerTest.receive
    open_document = LanguageServerTest.open_document
    complete = LanguageServerTest.complete

    def start(self, options=None):
        # Nothing listens here, so loading schema for the defaults fails at once.
        LanguageServerTest.start_server(
            self,
            None,
            options or {},
            {
                "KUSTO_LSP_TEST_TOKEN": "unused",
                "KUSTO_LSP_TEST_ENDPOINTS": json.dumps(
                    {
                        "a.kusto.windows.net": "http://127.0.0.1:9",
                        "b.kusto.windows.net": "http://127.0.0.1:9",
                    }
                ),
            },
        )

    def stop(self):
        self.process.terminate()
        self.process.communicate(timeout=5)
        self.process = None

    def connection_titles(self, text):
        self.open_document(text)
        self.send(7, "textDocument/codeLens", {"textDocument": {"uri": URI}})
        lenses = self.receive_result(7)
        return [
            lens["command"]["title"]
            for lens in lenses
            if lens["command"]["command"] == "kusto.connection"
        ]

    def receive_result(self, request_id):
        while True:
            message = self.receive()
            if message.get("id") == request_id and "result" in message:
                return message["result"]

    def test_the_shared_cases_give_the_same_connections_as_the_editor_computes(self):
        cases = json.loads(CASES.read_text())
        self.assertTrue(cases)
        for case in cases:
            defaults = {
                key: value
                for key, value in (case.get("defaults") or {}).items()
                if value is not None
            }
            self.start(defaults)
            try:
                titles = self.connection_titles(case["text"])
            finally:
                self.stop()
            self.assertEqual(
                titles,
                [expected_title(query) for query in case["queries"]],
                case["name"],
            )

    def directive_items(self, text, character):
        self.open_document(text)
        self.send(
            2,
            "textDocument/completion",
            {"textDocument": {"uri": URI}, "position": {"line": 0, "character": character}},
        )
        return {item["label"]: item for item in self.receive_result(2)}

    def test_the_directives_are_offered_with_the_cursor_inside_the_quotes(self):
        self.start()
        items = self.directive_items("// :set", 7)
        self.assertEqual(sorted(items), ["setDefaultCluster", "setDefaultDb"])
        edit = items["setDefaultCluster"]["textEdit"]
        self.assertEqual(edit["newText"], 'setDefaultCluster("$0")')
        self.assertEqual(items["setDefaultCluster"]["insertTextFormat"], 2)
        self.assertEqual(
            edit["range"],
            {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 7}},
            "it replaces what was typed after the colon",
        )

    def test_the_directives_are_offered_as_soon_as_the_colon_is_typed(self):
        self.start()
        self.assertIn(":", self.capabilities["completionProvider"]["triggerCharacters"])
        items = self.directive_items("// :", 4)
        self.assertEqual(sorted(items), ["setDefaultCluster", "setDefaultDb"])
        self.assertEqual(
            items["setDefaultDb"]["textEdit"]["range"],
            {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 4}},
        )

    def test_the_form_without_a_space_is_still_completed(self):
        self.start()
        items = self.directive_items("//:set", 6)
        self.assertEqual(sorted(items), ["setDefaultCluster", "setDefaultDb"])
        self.assertEqual(
            items["setDefaultDb"]["textEdit"]["range"]["start"],
            {"line": 0, "character": 3},
        )

    def test_an_ordinary_comment_gets_no_directive_completion(self):
        self.start()
        self.open_document("// set")
        self.assertNotIn("setDefaultDb", self.complete(0, 6))

    def test_a_directive_the_editor_does_not_know_is_warned_about(self):
        self.start()
        diagnostics = self.open_document(
            '// :setDefaultCluster(https://a)\n// :setDefaultDb("db")\n//:somethingElse("x")\n// :somethingElse("x")\n\nprint 1'
        )
        lines = [(d["range"]["start"]["line"], d["severity"], d["code"]) for d in diagnostics]
        self.assertEqual(lines, [(0, 2, "directive"), (2, 2, "directive"), (3, 2, "directive")])

    def test_valid_directives_and_comment_only_blocks_raise_no_diagnostics(self):
        self.start()
        diagnostics = self.open_document(
            '// :setDefaultCluster("a")\n//:setDefaultDb("db")\n\n// a note\n\nprint 1'
        )
        self.assertEqual(diagnostics, [])

    def test_a_comment_only_block_has_no_run_lens(self):
        self.start({"cluster": "a", "database": "db"})
        self.open_document('//:setDefaultDb("other")\n\n// note\n\nprint 1')
        self.send(7, "textDocument/codeLens", {"textDocument": {"uri": URI}})
        lenses = self.receive_result(7)
        lines = sorted({lens["range"]["start"]["line"] for lens in lenses})
        self.assertEqual(lines, [4], "only the query has lenses")


class DirectiveSchemaTest(SchemaTest):
    def test_each_query_gets_the_schema_of_the_cluster_its_directives_name(self):
        first = self.fake(["Samples"], STORM_ENTITIES)
        second = self.fake(
            ["Logs"],
            {"Logs": [("Table", "Requests", "", "", "", "RequestId:guid, DurationMs:real")]},
        )
        self.start({"first": first, "second": second})
        text = (
            '//:setDefaultCluster("first")\n//:setDefaultDb("Samples")\nStorm\n\n'
            '//:setDefaultCluster("second")\n//:setDefaultDb("Logs")\nRequ'
        )
        self.open_document(text)

        def labels_at(line, character, wanted):
            deadline = time.time() + 10
            while True:
                self.change(text)
                labels = [item["label"] for item in self.complete(line, character)]
                if wanted in labels or time.time() > deadline:
                    return labels
                time.sleep(0.1)

        self.assertIn("StormEvents", labels_at(2, 5, "StormEvents"))
        in_second = labels_at(6, 4, "Requests")
        self.assertIn("Requests", in_second)
        self.assertNotIn("StormEvents", in_second, "the first query's cluster is not the second's")
        # Nothing was fetched from a cluster no query names.
        self.assertTrue(first.commands)
        self.assertTrue(second.commands)


if __name__ == "__main__":
    if not SERVER.exists():
        sys.exit("Build KustoLanguageServer.csproj before running these tests")
    unittest.main()
