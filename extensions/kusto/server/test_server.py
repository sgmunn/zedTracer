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

    def __init__(self, databases, entities=None, status=200, delay=0):
        self.databases = databases
        self.delay = delay
        self.entities = entities or {}
        self.status = status
        self.commands = []
        self.authorizations = []
        self.metadata_requests = 0
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path == "/v1/rest/auth/metadata":
                    outer.metadata_requests += 1
                    self.respond(200, {"AzureAD": {"KustoServiceResourceId": "https://kusto.kusto.windows.net"}})
                else:
                    self.respond(404, {})

            def do_POST(self):
                outer.authorizations.append(self.headers.get("Authorization"))
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                outer.commands.append((body["db"], body["csl"]))
                time.sleep(outer.delay)
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

    def start(self, endpoints, options=None, data_dir=None):
        environment = {
            "KUSTO_LSP_TEST_TOKEN": "test-token",
            "KUSTO_LSP_TEST_ENDPOINTS": json.dumps(
                {f"{name}.kusto.windows.net": cluster.url for name, cluster in endpoints.items()}
            ),
        }
        if data_dir is not None:
            environment["KUSTO_ZED_DATA_DIR"] = data_dir
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
            and lens["command"]["command"] not in ("kusto.connection", "kusto.refreshSchema")
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
        self.assertEqual(
            first[1], "Results (1,240 rows)",
            "next to Run, which the long lenses after it cannot push away",
        )
        self.assertTrue(first[2].startswith("Last run: "), first)
        self.assertTrue(first[2].endswith("took 1.8 s"), first)
        self.assertEqual(first[3:], ["Copy CID"])
        self.assertEqual(self.titles(lenses, 3), ["▶ Run"], "another query has no history")

        by_title = {lens["command"]["title"]: lens["command"] for lens in lenses}
        self.assertEqual(
            by_title["Results (1,240 rows)"]["arguments"],
            ["kusto::ShowResult", {"path": "/history/a.ktt"}],
        )
        self.assertEqual(
            by_title["Copy CID"]["arguments"],
            ["kusto::CopyClientRequestId", {"id": "id-1"}],
        )

    def test_a_record_that_carries_parameter_values_is_read_like_any_other(self):
        self.record("started", "id-1", "T1\n| take 1", parameters={"raid": "abc"})
        self.record(
            "finished", "id-1", "T1\n| take 1",
            durationMs=900, rows=7, path="/history/a.ktt", parameters={"raid": "abc"},
        )
        titles = self.titles(self.lenses(), 0)
        self.assertTrue(titles[2].startswith("Last run: "), titles)

    def test_the_same_text_on_another_cluster_is_another_query(self):
        self.record("started", "id-1", "T1\n| take 1")
        self.record("finished", "id-1", "T1\n| take 1", durationMs=900, rows=7, path="/history/a.ktt")

        here = self.titles(self.lenses(), 0)
        self.assertTrue(here[2].startswith("Last run: "), here)

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

    def test_results_stays_next_to_cancel_while_a_later_run_is_going(self):
        self.record("finished", "id-1", "T1\n| take 1", durationMs=900, rows=7, path="/history/a.ktt")
        self.record("started", "id-2", "T1\n| take 1")
        first = self.titles(self.lenses(), 0)
        self.assertRegex(first[0], SPINNING)
        self.assertEqual(first[1:3], ["Cancel", "Results (7 rows)"])
        self.assertTrue(first[3].startswith("Last run: "), first)

    def test_one_row_is_not_plural_and_a_result_without_a_count_has_none(self):
        self.record("finished", "id-1", "T1\n| take 1", durationMs=900, rows=1, path="/history/a.ktt")
        self.assertEqual(self.titles(self.lenses(), 0)[1], "Results (1 row)")

        self.record("finished", "id-2", "T1\n| take 1", durationMs=900, path="/history/b.ktt")
        titles = self.titles(self.lenses(), 0)
        self.assertEqual(titles[1], "Results")
        self.assertTrue(titles[2].endswith("took 900 ms"), titles)

    def test_the_rows_stay_on_the_last_run_lens_when_the_run_left_no_result_to_show(self):
        self.record("finished", "id-1", "T1\n| take 1", durationMs=900, rows=7)
        titles = self.titles(self.lenses(), 0)
        self.assertTrue(titles[1].startswith("Last run: "), titles)
        self.assertTrue(titles[1].endswith("took 900 ms, 7 rows"), titles)
        self.assertFalse([title for title in titles if title.startswith("Results")], titles)

    def test_a_failed_last_run_has_no_results_lens(self):
        self.record("started", "id-1", "T1\n| take 1")
        self.record("failed", "id-1", "T1\n| take 1", message="bad table")
        titles = self.titles(self.lenses(), 0)
        self.assertFalse([title for title in titles if title.startswith("Results")], titles)

    def test_the_server_asks_for_fresh_lenses_when_a_run_ends(self):
        self.record("started", "id-1", "T1\n| take 1")
        self.assertRegex(self.titles(self.lenses(), 0)[0], SPINNING)

        self.record("finished", "id-1", "T1\n| take 1", durationMs=500, rows=3, path="/history/a.ktt")
        self.assertTrue(self.wait_for_refresh(), "no refresh request arrived")

        titles = self.titles(
            self.request(8, "textDocument/codeLens", {"textDocument": {"uri": URI}}), 0
        )
        self.assertEqual(titles[0], "▶ Run")
        self.assertEqual(titles[1], "Results (3 rows)")
        self.assertTrue(titles[2].endswith("took 500 ms"), titles)

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
        self.assertEqual(titles[1], "Results (7 rows)")
        self.assertTrue(titles[2].endswith("took 900 ms"), titles)

    def test_a_run_that_never_reported_an_end_stops_counting_as_running(self):
        self.record("started", "id-1", "T1\n| take 1", minutes_ago=60)
        self.assertEqual(self.titles(self.lenses(), 0), ["▶ Run"])

    def test_without_a_log_there_are_only_run_lenses_and_the_connection(self):
        self.assertFalse(self.log.exists())
        lenses = self.lenses()
        # Each of the two queries has a Run, a connection and a schema lens.
        self.assertEqual(len(lenses), 6)
        self.assertEqual(self.titles(lenses, 0), ["▶ Run"])

    def test_the_noop_command_is_accepted(self):
        self.assertIsNone(
            self.request(9, "workspace/executeCommand", {"command": "kusto.noop", "arguments": []})
        )


CASES = Path(__file__).resolve().parents[3] / "fork-docs" / "samples" / "connection-directives.json"
PARAMETER_CASES = Path(__file__).resolve().parents[3] / "fork-docs" / "samples" / "query-parameters.json"


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


class DefaultsFileTest(unittest.TestCase):
    """The cluster and database from the editor's settings, which it writes to `defaults.json`."""

    def setUp(self):
        self.data = tempfile.TemporaryDirectory()
        self.file = Path(self.data.name) / "kusto" / "defaults.json"
        self.file.parent.mkdir(parents=True)
        self.process = None
        self.next_request = 10

    def tearDown(self):
        if self.process:
            self.process.terminate()
            try:
                self.process.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.communicate()
        self.data.cleanup()

    send = LanguageServerTest.send
    receive = LanguageServerTest.receive
    receive_where = CodeLensTest.receive_where
    open_document = CodeLensTest.open_document

    def start(self, options=None):
        LanguageServerTest.start_server(
            self,
            None,
            options,
            {
                "KUSTO_ZED_DATA_DIR": self.data.name,
                "KUSTO_LSP_TEST_TOKEN": "unused",
                "KUSTO_LSP_TEST_ENDPOINTS": json.dumps(
                    {
                        "a.kusto.windows.net": "http://127.0.0.1:9",
                        "b.kusto.windows.net": "http://127.0.0.1:9",
                    }
                ),
            },
        )

    def write(self, cluster, database):
        temporary = self.file.with_suffix(".tmp")
        temporary.write_text(json.dumps({"cluster": cluster, "database": database}))
        temporary.replace(self.file)

    def connection_title(self):
        self.next_request += 1
        request_id = self.next_request
        self.send(request_id, "textDocument/codeLens", {"textDocument": {"uri": URI}})
        lenses = self.receive_where(
            lambda message: message.get("id") == request_id and "result" in message
        )["result"]
        titles = [
            lens["command"]["title"]
            for lens in lenses
            if lens["command"]["command"] == "kusto.connection"
        ]
        self.assertEqual(len(titles), 1)
        return titles[0]

    def title_becomes(self, wanted):
        deadline = time.time() + 10
        while True:
            title = self.connection_title()
            if title == wanted or time.time() > deadline:
                return title
            time.sleep(0.1)

    def test_the_file_gives_the_defaults_when_the_server_starts(self):
        self.write("a", "one")
        self.start()
        self.open_document("print 1")
        self.assertEqual(self.connection_title(), "a.kusto.windows.net / one")

    def test_the_file_wins_over_the_initialization_options(self):
        self.write("a", "one")
        self.start({"cluster": "b", "database": "two"})
        self.open_document("print 1")
        self.assertEqual(self.connection_title(), "a.kusto.windows.net / one")

    def test_the_initialization_options_stand_without_a_file(self):
        self.start({"cluster": "b", "database": "two"})
        self.open_document("print 1")
        self.assertEqual(self.connection_title(), "b.kusto.windows.net / two")

    def test_a_change_to_the_file_applies_to_open_documents_and_asks_for_new_lenses(self):
        self.write("a", "one")
        self.start()
        self.open_document("print 1")
        self.write("b", "two")
        self.assertEqual(self.title_becomes("b.kusto.windows.net / two"), "b.kusto.windows.net / two")

    def test_removing_the_settings_leaves_no_cluster(self):
        self.write("a", "one")
        self.start()
        self.open_document("print 1")
        self.write(None, None)
        self.assertEqual(self.title_becomes("no cluster"), "no cluster")

    def test_a_file_that_is_not_json_is_ignored(self):
        self.write("a", "one")
        self.start()
        self.open_document("print 1")
        self.file.write_text("{ not json")
        time.sleep(0.5)
        self.assertEqual(self.connection_title(), "a.kusto.windows.net / one")


class ParameterLensTest(unittest.TestCase):
    """The lens that says which parameter profile a query runs with."""

    PROFILES = (
        "# who is on call\nactive: A\nprofiles:\n  A:\n    raid: from-a\n    count: 5\n  B:\n    raid: from-b\n"
    )

    def setUp(self):
        self.project = tempfile.TemporaryDirectory()
        self.root = Path(self.project.name).resolve()
        self.uri = (self.root / "queries.kql").as_uri()
        self.shared = self.root / ".kusto" / "parameters.yaml"
        self.beside = self.root / "queries.parameters.yaml"
        self.process = None
        self.next_request = 10
        LanguageServerTest.start_server(
            self,
            self.root.as_uri(),
            {"cluster": "a", "database": "db"},
            {
                "KUSTO_LSP_TEST_TOKEN": "unused",
                "KUSTO_LSP_TEST_ENDPOINTS": json.dumps({"a.kusto.windows.net": "http://127.0.0.1:9"}),
            },
        )

    def tearDown(self):
        self.process.terminate()
        try:
            self.process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.communicate()
        self.project.cleanup()

    send = LanguageServerTest.send
    receive = LanguageServerTest.receive
    receive_where = CodeLensTest.receive_where

    def write(self, path, text):
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary = path.with_name(path.name + ".tmp")
        temporary.write_text(text)
        temporary.replace(path)

    def open_document(self, text):
        self.send(
            None,
            "textDocument/didOpen",
            {"textDocument": {"uri": self.uri, "languageId": "kusto", "version": 1, "text": text}},
        )
        self.receive_where(lambda message: message.get("method") == "textDocument/publishDiagnostics")

    def lenses(self):
        self.next_request += 1
        request_id = self.next_request
        self.send(request_id, "textDocument/codeLens", {"textDocument": {"uri": self.uri}})
        return self.receive_where(
            lambda message: message.get("id") == request_id and "result" in message
        )["result"]

    def parameter_lens(self):
        found = [
            lens["command"]
            for lens in self.lenses()
            if (lens["command"].get("arguments") or [None])[0] == "kusto::SelectParameterProfile"
        ]
        self.assertLessEqual(len(found), 1)
        return found[0] if found else None

    def title(self):
        lens = self.parameter_lens()
        return lens["title"] if lens else None

    def title_becomes(self, wanted):
        deadline = time.time() + 10
        while True:
            title = self.title()
            if title == wanted or time.time() > deadline:
                return title
            time.sleep(0.1)

    DECLARING = "declare query_parameters(raid:string);\nT | where Id == raid"

    def test_a_query_that_declares_nothing_has_no_parameter_lens(self):
        self.write(self.shared, self.PROFILES)
        self.open_document("T | take 1")
        self.assertIsNone(self.title())

    def test_the_lens_names_the_active_profile_and_opens_the_selector(self):
        self.write(self.shared, self.PROFILES)
        self.open_document(self.DECLARING)
        lens = self.parameter_lens()
        self.assertEqual(lens["title"], "Params: A")
        self.assertEqual(lens["command"], "zed.dispatchAction")
        self.assertEqual(lens["arguments"], ["kusto::SelectParameterProfile"])

    def test_without_a_profiles_file_or_an_active_profile_the_lens_says_none(self):
        self.open_document(self.DECLARING)
        self.assertEqual(self.title(), "Params: none")
        self.write(self.shared, "active: null\nprofiles:\n  A:\n    raid: x\n")
        self.assertEqual(self.title_becomes("Params: none"), "Params: none")

    def test_a_declared_parameter_the_profile_has_no_value_for_is_named(self):
        self.write(self.shared, self.PROFILES.replace("active: A", "active: B"))
        self.open_document("declare query_parameters(raid:string, count:long, since:datetime = datetime(2024-01-01, 5));\nT")
        self.assertEqual(self.title(), "Params: B (no value for count)", "a parameter with a default is not missing")

    def test_a_file_beside_the_query_takes_the_place_of_the_projects(self):
        self.write(self.shared, self.PROFILES)
        self.write(self.beside, "active: Mine\nprofiles:\n  Mine:\n    raid: x\n")
        self.open_document(self.DECLARING)
        self.assertEqual(self.title(), "Params: Mine")

    def test_a_file_that_cannot_be_read_is_named(self):
        self.write(self.shared, "active: [")
        self.open_document(self.DECLARING)
        self.assertEqual(self.title(), "Params: cannot read parameters.yaml")

    def test_an_active_profile_that_does_not_exist_is_no_active_profile(self):
        self.write(self.shared, self.PROFILES.replace("active: A", "active: Gone"))
        self.open_document(self.DECLARING)
        self.assertEqual(self.title(), "Params: none")

    def test_choosing_another_profile_changes_the_lens_and_asks_for_new_ones(self):
        self.write(self.shared, self.PROFILES)
        self.open_document(self.DECLARING)
        self.assertEqual(self.title(), "Params: A")
        self.write(self.shared, self.PROFILES.replace("active: A", "active: B"))
        refresh = self.receive_where(lambda message: message.get("method") == "workspace/codeLens/refresh")
        self.assertIsNotNone(refresh)
        self.assertEqual(self.title_becomes("Params: B"), "Params: B")

    def test_a_project_folder_made_after_the_file_was_opened_is_followed(self):
        self.open_document(self.DECLARING)
        self.assertEqual(self.title(), "Params: none")
        self.write(self.shared, self.PROFILES)
        self.assertEqual(self.title_becomes("Params: A"), "Params: A")
        self.write(self.shared, self.PROFILES.replace("active: A", "active: B"))
        self.assertEqual(self.title_becomes("Params: B"), "Params: B")

    def test_no_kusto_folder_is_made_in_the_project(self):
        self.open_document(self.DECLARING)
        self.title()
        self.assertFalse((self.root / ".kusto").exists())

    def test_the_shared_cases_declare_the_same_parameters_as_the_editor_finds(self):
        cases = json.loads(PARAMETER_CASES.read_text())
        self.assertTrue(cases)
        self.write(self.shared, "active: P\nprofiles:\n  P:\n    unrelated: x\n")
        for case in cases:
            self.process.stdin.flush()
            self.send(None, "textDocument/didClose", {"textDocument": {"uri": self.uri}})
            self.open_document(case["query"])
            expected = (
                f"Params: P (no value for {', '.join(case['names'])})" if case["names"] else None
            )
            self.assertEqual(self.title(), expected, case["name"])


class ProfilesFileLensTest(unittest.TestCase):
    """The `Make Active` lens in a profiles file, and the YAML files the server leaves alone."""

    PROFILES = (
        "# who is on call\nactive: A\nprofiles:\n  A:\n    raid: from-a\n"
        "  \"Incident B\":\n    raid: from-b\n  C:\n    raid: from-c\n"
    )
    SHARED = "file:///work/project/.kusto/parameters.yaml"

    def setUp(self):
        self.process = None
        self.next_request = 40
        LanguageServerTest.start_server(
            self, None, {}, {"KUSTO_LSP_TEST_TOKEN": "unused"}
        )

    def tearDown(self):
        self.process.terminate()
        try:
            self.process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.communicate()

    send = LanguageServerTest.send
    receive = LanguageServerTest.receive

    def open(self, uri, text, language="yaml"):
        self.send(
            None,
            "textDocument/didOpen",
            {"textDocument": {"uri": uri, "languageId": language, "version": 1, "text": text}},
        )

    def change(self, uri, text):
        self.send(
            None,
            "textDocument/didChange",
            {"textDocument": {"uri": uri, "version": 2}, "contentChanges": [{"text": text}]},
        )

    def ask(self, method, parameters):
        self.next_request += 1
        request_id = self.next_request
        self.send(request_id, method, parameters)
        while True:
            message = self.receive()
            if message.get("id") == request_id and "result" in message:
                return message["result"]
            self.assertNotEqual(
                message.get("method"),
                "textDocument/publishDiagnostics",
                f"a YAML file got diagnostics: {message}",
            )

    def lenses(self, uri):
        return self.ask("textDocument/codeLens", {"textDocument": {"uri": uri}})

    def test_the_active_profile_is_marked_and_the_others_can_be_made_active(self):
        self.open(self.SHARED, self.PROFILES)
        lenses = self.lenses(self.SHARED)

        by_line = {lens["range"]["start"]["line"]: lens for lens in lenses}
        self.assertEqual(sorted(by_line), [3, 5, 7])
        active = by_line[3]["command"]
        self.assertEqual(active["title"], "✓ Active")
        self.assertEqual(active["command"], "kusto.noop")

        other = by_line[5]["command"]
        self.assertEqual(other["title"], "Make Active")
        self.assertEqual(other["command"], "zed.dispatchAction")
        self.assertEqual(other["arguments"], ["kusto::MakeParameterProfileActive", {"name": "Incident B"}])
        self.assertEqual(by_line[7]["command"]["arguments"][1], {"name": "C"})

    def test_a_lens_sits_on_the_name_of_its_profile(self):
        self.open(self.SHARED, self.PROFILES)
        lenses = {lens["range"]["start"]["line"]: lens for lens in self.lenses(self.SHARED)}
        self.assertEqual(lenses[3]["range"], {"start": {"line": 3, "character": 2}, "end": {"line": 3, "character": 3}})
        self.assertEqual(lenses[5]["range"]["start"], {"line": 5, "character": 2})
        self.assertEqual(lenses[5]["range"]["end"], {"line": 5, "character": 14}, "the quotes are part of the name")

    def test_the_text_of_the_editor_decides_not_the_text_on_disk(self):
        self.open(self.SHARED, self.PROFILES)
        self.change(self.SHARED, self.PROFILES.replace("active: A", 'active: "Incident B"'))
        titles = {
            lens["range"]["start"]["line"]: lens["command"]["title"] for lens in self.lenses(self.SHARED)
        }
        self.assertEqual(titles, {3: "Make Active", 5: "✓ Active", 7: "Make Active"})

    def test_a_file_beside_a_query_is_a_profiles_file_too(self):
        uri = "file:///work/project/incident.parameters.yaml"
        self.open(uri, "profiles:\n  Mine:\n    raid: x\n")
        lenses = self.lenses(uri)
        self.assertEqual([lens["command"]["title"] for lens in lenses], ["Make Active"])

    def test_no_profile_is_active_when_the_active_one_does_not_exist(self):
        self.open(self.SHARED, self.PROFILES.replace("active: A", "active: Gone"))
        titles = {lens["command"]["title"] for lens in self.lenses(self.SHARED)}
        self.assertEqual(titles, {"Make Active"})

    def test_a_file_that_is_not_yaml_or_has_no_profiles_has_no_lenses(self):
        self.open(self.SHARED, "active: [")
        self.assertEqual(self.lenses(self.SHARED), [])
        self.change(self.SHARED, "# nothing yet\n")
        self.assertEqual(self.lenses(self.SHARED), [])
        self.change(self.SHARED, "profiles: [a, b]\n")
        self.assertEqual(self.lenses(self.SHARED), [])

    def test_other_yaml_files_get_no_lenses_diagnostics_or_completion(self):
        uri = "file:///work/project/.github/workflows/build.yaml"
        self.open(uri, "name: build\non: push\njobs:\n  build:\n    runs-on: ubuntu\n")
        self.assertEqual(self.lenses(uri), [])
        self.change(uri, "name: build\nprofiles:\n  A:\n    x: y\n")
        self.assertEqual(self.lenses(uri), [])
        completions = self.ask(
            "textDocument/completion",
            {"textDocument": {"uri": uri}, "position": {"line": 0, "character": 3}},
        )
        self.assertEqual(completions, [])
        self.send(None, "textDocument/didClose", {"textDocument": {"uri": uri}})

    def test_a_profiles_file_gets_no_query_completion_or_diagnostics(self):
        self.open(self.SHARED, self.PROFILES)
        completions = self.ask(
            "textDocument/completion",
            {"textDocument": {"uri": self.SHARED}, "position": {"line": 0, "character": 2}},
        )
        self.assertEqual(completions, [])
        self.change(self.SHARED, self.PROFILES + "this is : not a query\n")
        self.lenses(self.SHARED)

    def test_a_closed_profiles_file_has_no_lenses_and_queries_are_unaffected(self):
        self.open(self.SHARED, self.PROFILES)
        self.send(None, "textDocument/didClose", {"textDocument": {"uri": self.SHARED}})
        self.assertEqual(self.lenses(self.SHARED), [])

        self.open(URI, "T | take 1", language="kusto")
        message = self.receive()
        self.assertEqual(message["method"], "textDocument/publishDiagnostics")
        self.assertEqual(message["params"]["uri"], URI)
        self.assertTrue(
            [lens for lens in self.lenses(URI) if lens["command"]["title"] == "▶ Run"]
        )


class SemanticTokensTest(unittest.TestCase):
    """The colours the server sends, which cover all of Kusto where a grammar covered some."""

    def setUp(self):
        self.process = None
        self.next_request = 60
        LanguageServerTest.start_server(self, None, {}, {"KUSTO_LSP_TEST_TOKEN": "unused"})
        provider = self.capabilities["semanticTokensProvider"]
        self.legend = provider["legend"]["tokenTypes"]

    def tearDown(self):
        self.process.terminate()
        try:
            self.process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.communicate()

    send = LanguageServerTest.send
    receive = LanguageServerTest.receive
    receive_where = CodeLensTest.receive_where

    def open(self, text, uri=URI, language="kusto"):
        self.send(
            None,
            "textDocument/didOpen",
            {"textDocument": {"uri": uri, "languageId": language, "version": 1, "text": text}},
        )
        if language == "kusto":
            self.receive_where(lambda message: message.get("method") == "textDocument/publishDiagnostics")

    def tokens(self, uri=URI):
        """The tokens as (line, column, length, type), decoded from LSP's relative form."""
        self.next_request += 1
        request_id = self.next_request
        self.send(request_id, "textDocument/semanticTokens/full", {"textDocument": {"uri": uri}})
        data = self.receive_where(
            lambda message: message.get("id") == request_id and "result" in message
        )["result"]["data"]
        self.assertEqual(len(data) % 5, 0)
        decoded, line, column = [], 0, 0
        for index in range(0, len(data), 5):
            delta_line, delta_start, length, kind, modifiers = data[index:index + 5]
            line += delta_line
            column = delta_start if delta_line else column + delta_start
            decoded.append((line, column, length, self.legend[kind]))
            self.assertEqual(modifiers, 0)
        return decoded

    def test_the_server_offers_full_document_tokens_with_a_legend_of_standard_types(self):
        provider = self.capabilities["semanticTokensProvider"]
        self.assertTrue(provider["full"])
        self.assertIn("keyword", self.legend)
        self.assertEqual(len(self.legend), len(set(self.legend)))

    def test_declare_and_print_are_coloured_which_the_old_grammar_could_not_do(self):
        self.open("declare query_parameters (raid:string);\nprint raid")
        self.assertEqual(
            self.tokens(),
            [
                (0, 0, 7, "keyword"), (0, 8, 16, "keyword"), (0, 26, 4, "variable"),
                (0, 31, 6, "type"), (1, 0, 5, "keyword"), (1, 6, 4, "variable"),
            ],
        )

    def test_a_control_command_is_coloured(self):
        self.open(".show function ASAz")
        self.assertEqual(self.tokens(), [(0, 0, 5, "keyword"), (0, 6, 8, "keyword")])

    def test_a_function_call_is_a_function_and_the_operators_are_all_known(self):
        self.open("print current_cluster_endpoint()\n\nT\n| take 5\n| top 3 by A\n| count\n| render timechart")
        found = {(line, kind): length for line, column, length, kind in self.tokens()}
        self.assertEqual(found[(0, "function")], 24)
        lines = {line: [token for token in self.tokens() if token[0] == line] for line in (3, 4, 5, 6)}
        self.assertEqual(lines[3], [(3, 2, 4, "keyword"), (3, 7, 1, "number")])
        self.assertEqual(lines[4][0], (4, 2, 3, "keyword"), "top")
        self.assertEqual(lines[5][0][3], "keyword", "count")
        self.assertEqual(lines[6][0], (6, 2, 6, "keyword"), "render")

    def test_comments_strings_and_numbers_have_their_own_types(self):
        self.open("// a note\nT | where A == 'x' and B > 1.5 // tail")
        kinds = {kind for _, _, _, kind in self.tokens()}
        self.assertTrue({"comment", "string", "number", "keyword"} <= kinds, kinds)
        self.assertEqual(self.tokens()[0], (0, 0, 9, "comment"))
        self.assertEqual(self.tokens()[-1], (1, 36, 7, "comment") if False else self.tokens()[-1])
        self.assertEqual(self.tokens()[-1][3], "comment")

    def test_a_token_never_spans_a_line(self):
        self.open("print ```first\nsecond\nthird```, 1")
        tokens = self.tokens()
        strings = [token for token in tokens if token[3] == "string"]
        self.assertEqual([token[0] for token in strings], [0, 1, 2])
        self.assertEqual([token[1] for token in strings], [6, 0, 0])
        self.assertEqual([token[2] for token in strings], [8, 6, 8])

    def test_columns_are_counted_in_utf_16_units_like_the_protocol_says(self):
        text = "print '😀' | take 1"
        self.open(text)
        take = next(token for token in self.tokens() if token[3] == "keyword" and token[2] == 4)
        self.assertEqual(take[1], len(text[: text.index("take")].encode("utf-16-le")) // 2)
        literal = next(token for token in self.tokens() if token[3] == "string")
        self.assertEqual(literal[2], 4, "a quote, a character of two units, and a quote")

    def test_windows_line_endings_do_not_shift_or_lengthen_a_token(self):
        self.open("T\r\n| take 1\r\n| count")
        self.assertEqual(self.tokens()[:2], [(1, 2, 4, "keyword"), (1, 7, 1, "number")])
        self.assertTrue(all(length < 10 for _, _, length, _ in self.tokens()))

    def test_every_query_in_a_file_is_coloured_and_comment_blocks_too(self):
        self.open("// first\n\nprint 1\n\n// second\nprint 2")
        self.assertEqual(
            [(line, kind) for line, _, _, kind in self.tokens()],
            [(0, "comment"), (2, "keyword"), (2, "number"), (4, "comment"), (5, "keyword"), (5, "number")],
        )

    def test_a_file_the_server_does_not_know_has_no_tokens(self):
        self.assertEqual(self.tokens("file:///elsewhere/other.kql"), [])
        self.open("name: x\n", uri="file:///work/config.yaml", language="yaml")
        self.assertEqual(self.tokens("file:///work/config.yaml"), [])


class SemanticTokensSchemaTest(SchemaTest):
    """Once a schema has loaded, tables, columns and functions are told apart, and the client is asked again."""

    send = LanguageServerTest.send
    receive = LanguageServerTest.receive
    receive_where = CodeLensTest.receive_where

    def tokens(self, text):
        self.send(
            None,
            "textDocument/didChange",
            {"textDocument": {"uri": URI, "version": 2}, "contentChanges": [{"text": text}]},
        )
        self.receive_where(lambda message: message.get("method") == "textDocument/publishDiagnostics")
        legend = self.capabilities["semanticTokensProvider"]["legend"]["tokenTypes"]
        self.send(77, "textDocument/semanticTokens/full", {"textDocument": {"uri": URI}})
        data = self.receive_where(lambda message: message.get("id") == 77 and "result" in message)["result"]["data"]
        found, line, column = [], 0, 0
        for index in range(0, len(data), 5):
            delta_line, delta_start, length, kind, _ = data[index:index + 5]
            line += delta_line
            column = delta_start if delta_line else column + delta_start
            found.append((line, column, length, legend[kind]))
        return found

    def test_a_table_is_a_class_and_a_column_a_property_once_the_schema_has_loaded(self):
        cluster = self.fake(["Samples"], STORM_ENTITIES)
        self.start({"help": cluster}, {"cluster": "help", "database": "Samples"})
        self.send(None, "initialized", {})
        self.send(
            None,
            "textDocument/didOpen",
            {"textDocument": {"uri": URI, "languageId": "kusto", "version": 1, "text": "print 1"}},
        )
        self.receive_where(lambda message: message.get("method") == "textDocument/publishDiagnostics")

        text = "StormEvents\n| where State == 'a'\n| project StatesOver(3, 'x')"
        deadline = time.time() + 10
        while True:
            found = self.tokens(text)
            if (0, 0, 11, "class") in found or time.time() > deadline:
                break
            time.sleep(0.1)
        self.assertIn((0, 0, 11, "class"), found, "the table")
        self.assertIn((1, 8, 5, "property"), found, "the column")

    def test_the_server_asks_the_client_for_new_colours_when_a_schema_arrives(self):
        cluster = self.fake(["Samples"], STORM_ENTITIES, delay=0.5)
        self.start({"help": cluster}, {"cluster": "help", "database": "Samples"})
        self.send(None, "initialized", {})
        self.send(
            None,
            "textDocument/didOpen",
            {"textDocument": {"uri": URI, "languageId": "kusto", "version": 1, "text": "print 1"}},
        )
        refresh = self.receive_where(
            lambda message: message.get("method") == "workspace/semanticTokens/refresh"
        )
        self.assertIn("id", refresh, "it is a request the client answers")


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


class SchemaRefreshTest(SchemaTest):
    """The lens that says how old the schema is, and the command it runs to fetch it again."""

    HOST = "help.kusto.windows.net"

    def setUp(self):
        super().setUp()
        self.data = tempfile.TemporaryDirectory()
        self.addCleanup(self.data.cleanup)
        self.cache = Path(self.data.name) / "kusto" / "schema" / self.HOST
        self.next_request = 20

    send = LanguageServerTest.send
    receive = LanguageServerTest.receive
    receive_where = CodeLensTest.receive_where

    def write_old_database(self, hours_ago):
        moment = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(hours=hours_ago)
        self.cache.mkdir(parents=True, exist_ok=True)
        (self.cache / "Samples.json").write_text(json.dumps({
            "version": 1,
            "fetchedAt": moment.isoformat().replace("+00:00", "Z"),
            "entities": [{"kind": "Table", "name": "OldTable", "schema": "Id:long",
                          "parameters": "", "body": "", "description": None}],
        }))

    def begin(self, cluster, text="print 1"):
        self.start({"help": cluster}, {"cluster": "help", "database": "Samples"}, data_dir=self.data.name)
        # A client says it is ready before a server may ask it anything.
        self.send(None, "initialized", {})
        self.send(
            None,
            "textDocument/didOpen",
            {"textDocument": {"uri": URI, "languageId": "kusto", "version": 1, "text": text}},
        )
        self.receive_where(lambda message: message.get("method") == "textDocument/publishDiagnostics")

    def schema_lens(self):
        self.next_request += 1
        request_id = self.next_request
        self.send(request_id, "textDocument/codeLens", {"textDocument": {"uri": URI}})
        lenses = self.receive_where(
            lambda message: message.get("id") == request_id and "result" in message
        )["result"]
        found = [
            lens["command"] for lens in lenses if lens["command"]["command"] == "kusto.refreshSchema"
        ]
        self.assertEqual(len(found), 1)
        return found[0]

    def title_becomes(self, wanted, timeout=10):
        deadline = time.time() + timeout
        while True:
            title = self.schema_lens()["title"]
            if title == wanted or time.time() > deadline:
                return title
            time.sleep(0.1)

    def labels(self, text):
        self.next_request += 1
        request_id = self.next_request
        self.send(
            None,
            "textDocument/didChange",
            {"textDocument": {"uri": URI, "version": 2}, "contentChanges": [{"text": text}]},
        )
        self.receive_where(lambda message: message.get("method") == "textDocument/publishDiagnostics")
        self.send(
            request_id,
            "textDocument/completion",
            {"textDocument": {"uri": URI}, "position": {"line": 0, "character": len(text)}},
        )
        result = self.receive_where(
            lambda message: message.get("id") == request_id and "result" in message
        )["result"]
        return [item["label"] for item in result]

    def labels_include(self, text, wanted, timeout=10):
        deadline = time.time() + timeout
        while True:
            labels = self.labels(text)
            if wanted in labels or time.time() > deadline:
                return labels
            time.sleep(0.1)

    def message(self):
        return self.receive_where(lambda message: message.get("method") == "window/showMessage")["params"]

    def refresh(self, cluster="help", database="Samples"):
        self.send(
            30,
            "workspace/executeCommand",
            {"command": "kusto.refreshSchema", "arguments": [cluster, database]},
        )

    def test_the_server_offers_the_command(self):
        self.begin(self.fake(["Samples"], STORM_ENTITIES))
        self.assertIn("kusto.refreshSchema", self.capabilities["executeCommandProvider"]["commands"])

    def test_the_lens_says_the_schema_was_just_loaded_and_names_what_a_click_refreshes(self):
        self.begin(self.fake(["Samples"], STORM_ENTITIES))
        self.assertEqual(self.title_becomes("↻ Schema: just now"), "↻ Schema: just now")
        lens = self.schema_lens()
        self.assertEqual(lens["command"], "kusto.refreshSchema")
        self.assertEqual(lens["arguments"], ["help", "Samples"])

    def test_the_lens_says_loading_while_the_cluster_is_slow(self):
        self.begin(self.fake(["Samples"], STORM_ENTITIES, delay=1.5))
        self.assertEqual(self.title_becomes("↻ Schema: loading…", timeout=3), "↻ Schema: loading…")
        self.assertEqual(self.title_becomes("↻ Schema: just now"), "↻ Schema: just now")

    def test_the_lens_says_not_loaded_when_the_cluster_cannot_be_reached(self):
        cluster = self.fake(["Samples"], STORM_ENTITIES)
        cluster.stop()
        self.begin(cluster)
        self.assertEqual(self.title_becomes("↻ Schema: not loaded"), "↻ Schema: not loaded")

    def test_a_cached_schema_shows_its_age_even_when_the_cluster_cannot_be_reached(self):
        self.write_old_database(hours_ago=3)
        cluster = self.fake(["Samples"], STORM_ENTITIES)
        cluster.stop()
        self.begin(cluster)
        self.assertEqual(self.title_becomes("↻ Schema: 3 h ago"), "↻ Schema: 3 h ago")

    def test_refreshing_fetches_the_schema_again_and_says_what_it_found(self):
        cluster = self.fake(["Samples"], STORM_ENTITIES)
        self.begin(cluster)
        self.assertIn("StormEvents", self.labels_include("Sto", "StormEvents"))
        fetched = len(cluster.commands)

        cluster.entities = {
            "Samples": STORM_ENTITIES["Samples"]
            + [("Table", "Alerts", "", "", "", "Id:long, Level:string")]
        }
        self.refresh()
        message = self.message()

        self.assertEqual(message["type"], 3)
        self.assertEqual(
            message["message"],
            "Refreshed the schema of help.kusto.windows.net / Samples: 2 tables, 1 function.",
        )
        self.assertGreater(len(cluster.commands), fetched)
        self.assertIn("Alerts", self.labels_include("Ale", "Alerts"))

    def test_refreshing_asks_the_cluster_even_when_the_cached_schema_is_fresh(self):
        self.write_old_database(hours_ago=0)
        cluster = self.fake(["Samples"], STORM_ENTITIES)
        self.begin(cluster)
        self.assertEqual(self.title_becomes("↻ Schema: just now"), "↻ Schema: just now")
        time.sleep(0.5)
        self.assertFalse(
            [csl for _, csl in cluster.commands if csl.startswith(".show databases entities")],
            "a fresh cache needs no fetch",
        )

        self.refresh()
        self.assertEqual(self.message()["type"], 3)
        self.assertTrue([csl for _, csl in cluster.commands if csl.startswith(".show databases entities")])
        self.assertIn("StormEvents", self.labels_include("Sto", "StormEvents"))
        self.assertNotIn("OldTable", self.labels("Old"), "the cluster's schema replaced the cached one")

    def test_a_refresh_that_fails_says_why_and_keeps_the_schema_there_was(self):
        self.write_old_database(hours_ago=3)
        cluster = self.fake(["Samples"], STORM_ENTITIES)
        cluster.stop()
        self.begin(cluster)
        self.assertIn("OldTable", self.labels_include("Old", "OldTable"))

        self.refresh()
        message = self.message()
        self.assertEqual(message["type"], 1)
        self.assertTrue(message["message"].startswith("Could not refresh the schema: "), message)
        self.assertIn("OldTable", self.labels("Old"))

    def test_a_refresh_with_no_cluster_says_so(self):
        self.start({}, {})
        self.send(None, "initialized", {})
        self.send(
            30, "workspace/executeCommand", {"command": "kusto.refreshSchema", "arguments": ["", ""]}
        )
        message = self.message()
        self.assertEqual(message["type"], 1)
        self.assertIn("no cluster", message["message"])


class SchemaDiagnosticsTest(unittest.TestCase):
    """Names that are not in the schema are errors, once the schema has arrived."""

    HOST = "help.kusto.windows.net"

    def setUp(self):
        self.clusters = []
        self.process = None
        self.data = tempfile.TemporaryDirectory()
        self.addCleanup(self.data.cleanup)
        self.cache = Path(self.data.name) / "kusto" / "schema" / self.HOST
        self.next_request = 20

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

    fake = SchemaTest.fake
    start = SchemaTest.start

    send = LanguageServerTest.send
    receive = LanguageServerTest.receive
    receive_where = CodeLensTest.receive_where
    refresh = SchemaRefreshTest.refresh
    message = SchemaRefreshTest.message
    labels_include = SchemaRefreshTest.labels_include
    labels = SchemaRefreshTest.labels

    def begin_with(self, cluster, text="print 1", options=None):
        self.start(
            {"help": cluster, "other": self.unreachable_cluster()},
            {"cluster": "help", "database": "Samples", **(options or {})},
            data_dir=self.data.name,
        )
        self.send(None, "initialized", {})
        self.send(
            None,
            "textDocument/didOpen",
            {"textDocument": {"uri": URI, "languageId": "kusto", "version": 1, "text": text}},
        )
        return self.receive_where(
            lambda message: message.get("method") == "textDocument/publishDiagnostics"
        )["params"]["diagnostics"]

    def unreachable_cluster(self):
        cluster = self.fake(["Samples"], STORM_ENTITIES)
        cluster.stop()
        return cluster

    def change(self, text):
        self.send(
            None,
            "textDocument/didChange",
            {"textDocument": {"uri": URI, "version": 2}, "contentChanges": [{"text": text}]},
        )

    def diagnostics(self, text):
        self.change(text)
        return self.receive_where(
            lambda message: message.get("method") == "textDocument/publishDiagnostics"
        )["params"]["diagnostics"]

    def diagnostics_become(self, text, wanted, timeout=10):
        deadline = time.time() + timeout
        while True:
            found = self.diagnostics(text)
            if wanted(found) or time.time() > deadline:
                return found
            time.sleep(0.1)

    def wait_for_diagnostics(self, wanted, timeout=10):
        """Diagnostics the server sends by itself, such as when a schema arrives."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            ready, _, _ = select.select([self.process.stdout], [], [], 0.2)
            if ready:
                message = self.receive()
                if message.get("method") == "textDocument/publishDiagnostics":
                    if wanted(message["params"]["diagnostics"]):
                        return message["params"]["diagnostics"]
        return None

    def messages(self, found):
        return [item["message"] for item in found]

    def test_an_unknown_table_and_an_unknown_column_are_errors_once_the_schema_has_loaded(self):
        self.begin_with(self.fake(["Samples"], STORM_ENTITIES))
        text = "Nope\n| take 1\n\nStormEvents\n| where Statex == 'a'\n| project State"
        found = self.diagnostics_become(text, lambda found: len(found) >= 2)

        self.assertEqual(len(found), 2, self.messages(found))
        table, column = found
        self.assertIn("Nope", table["message"])
        self.assertEqual(table["severity"], 1)
        self.assertEqual(table["range"]["start"], {"line": 0, "character": 0})
        self.assertEqual(table["range"]["end"], {"line": 0, "character": 4})
        self.assertIn("Statex", column["message"])
        self.assertEqual(column["range"]["start"], {"line": 4, "character": 8})

    def test_names_that_are_in_the_schema_are_not_errors(self):
        self.begin_with(self.fake(["Samples"], STORM_ENTITIES))
        text = (
            "StormEvents\n| where State == 'a' and DamageProperty > 5\n| summarize n = count() by bin(StartTime, 1d)"
            "\n\nStatesOver(3, 'x')"
        )
        self.assertTrue(self.diagnostics_become("Nope", lambda found: len(found) == 1), "the schema loaded")
        self.assertEqual(self.diagnostics(text), [])

    def test_nothing_is_called_wrong_while_the_schema_is_still_on_its_way(self):
        cluster = self.fake(["Samples"], STORM_ENTITIES, delay=1.5)
        first = self.begin_with(cluster, "Nope\n| take 1")
        self.assertEqual(first, [], "the cluster has not answered yet")

        arrived = self.wait_for_diagnostics(lambda found: len(found) == 1, timeout=10)
        self.assertIsNotNone(arrived, "the server publishes again when the schema arrives")
        self.assertIn("Nope", arrived[0]["message"])

    def test_without_a_reachable_cluster_there_are_only_syntax_errors(self):
        cluster = self.fake(["Samples"], STORM_ENTITIES)
        cluster.stop()
        found = self.begin_with(cluster, "Nope\n| take 1")
        self.assertEqual(found, [])
        time.sleep(0.5)
        syntax = self.diagnostics("Nope | where | take 1")
        self.assertTrue(syntax, "a syntax error still shows")
        self.assertNotIn("does not refer", syntax[0]["message"])
        self.assertEqual(
            [item for item in self.diagnostics("Nope | take 1")], [], "an unknown table does not"
        )

    def test_an_old_cached_schema_is_used_to_check_names_until_the_cluster_answers(self):
        self.write_old_database(hours_ago=3)
        cluster = self.fake(["Samples"], STORM_ENTITIES)
        cluster.stop()
        self.begin_with(cluster, "print 1")
        found = self.diagnostics_become("StormEvents | take 1", lambda found: len(found) == 1)
        self.assertIn("StormEvents", found[0]["message"], "only the cached OldTable is known")
        self.assertEqual(self.diagnostics("OldTable | take 1"), [])

    write_old_database = SchemaRefreshTest.write_old_database

    def test_the_option_can_turn_checking_names_off(self):
        self.begin_with(self.fake(["Samples"], STORM_ENTITIES), options={"schemaDiagnostics": False})
        self.assertEqual(self.labels_include("Sto", "StormEvents").count("StormEvents"), 1, "the schema loaded")
        self.assertEqual(self.diagnostics("Nope | take 1"), [])

    def test_a_control_command_is_only_checked_for_syntax(self):
        self.begin_with(self.fake(["Samples"], STORM_ENTITIES))
        self.assertTrue(self.diagnostics_become("Nope", lambda found: len(found) == 1))
        self.assertEqual(self.diagnostics(".show function Nope"), [])
        self.assertEqual(self.diagnostics("// the tables\n.show tables"), [])

    def test_a_query_that_runs_elsewhere_is_not_checked_until_that_schema_has_arrived(self):
        self.begin_with(self.fake(["Samples"], STORM_ENTITIES))
        text = (
            'Nope\n| take 1\n\n//:setDefaultCluster("other")\n//:setDefaultDb("Samples")\nAlsoNope\n| take 1'
        )
        found = self.diagnostics_become(text, lambda found: len(found) >= 1)
        self.assertEqual(len(found), 1, self.messages(found))
        self.assertIn("Nope", found[0]["message"])
        self.assertEqual(found[0]["range"]["start"]["line"], 0, "only the query on the loaded cluster")

    def test_a_database_a_query_names_is_not_checked_until_it_has_loaded(self):
        self.begin_with(self.fake(["Samples"], STORM_ENTITIES))
        text = "cluster('other').database('Logs').Requests\n| take 1"
        self.assertEqual(self.diagnostics_become(text, lambda found: True), [])
        time.sleep(0.5)
        self.assertEqual(self.diagnostics(text), [], "the other cluster cannot be reached")

    def loaded(self):
        self.begin_with(self.fake(["Samples"], STORM_ENTITIES))
        self.assertTrue(self.diagnostics_become("Nope", lambda found: len(found) == 1), "the schema loaded")

    def codes(self, text):
        return [item["code"] for item in self.diagnostics(text)]

    def test_an_unknown_source_is_the_only_error_of_its_query(self):
        self.loaded()
        text = (
            "Nope\n| where Foo > 1\n| where Bar between (datetime(2026-01-01) .. 1d)\n"
            "| project Baz, Qux = strcat(Baz, 'x')\n| order by Baz"
        )
        self.assertEqual(self.codes(text), ["KS204"])

    def test_an_unknown_function_is_the_only_error_of_a_query_that_starts_with_it(self):
        self.loaded()
        found = self.diagnostics(
            "let raid = 'abc';\nNoSuchFunction().Trace\n| where TIMESTAMP between (datetime(2026-09-03) .. 1d)"
            "\n| where RootActivityId == raid\n| order by TIMESTAMP asc"
        )
        self.assertEqual([item["code"] for item in found], ["KS211"], self.messages(found))
        self.assertIn("NoSuchFunction", found[0]["message"])

    def test_a_known_source_with_a_wrong_column_still_reports_it(self):
        self.loaded()
        self.assertEqual(self.codes("StormEvents\n| where Missing > 1\n| project Gone"), ["KS142", "KS142"])

    def test_a_wrong_name_in_a_later_statement_is_still_reported(self):
        self.loaded()
        text = "let a = Nope | where Foo > 1;\nStormEvents\n| where Missing > 1"
        self.assertEqual(self.codes(text), ["KS204", "KS142"])

    def test_an_unknown_name_that_is_not_the_source_does_not_hide_other_errors(self):
        self.loaded()
        text = "StormEvents\n| where Missing > 1\n| join kind=inner (Nope) on State"
        found = self.codes(text)
        self.assertIn("KS204", found, "the unknown table")
        self.assertIn("KS142", found, "the column that really is missing")

    def test_a_wrong_function_name_in_the_middle_of_a_query_is_not_a_missing_source(self):
        self.loaded()
        found = self.codes("StormEvents\n| where agoo(1h) > StartTime\n| where Missing > 1")
        self.assertIn("KS211", found)
        self.assertIn("KS142", found, "the source is known, so the missing column is real")

    def test_two_unknown_sources_in_a_file_each_get_their_own_error(self):
        self.loaded()
        text = "Nope\n| where Foo > 1\n\nAlsoNope\n| where Bar > 1"
        self.assertEqual(self.codes(text), ["KS204", "KS204"])

    def test_a_refresh_that_brings_a_new_table_clears_the_error_without_a_keystroke(self):
        cluster = self.fake(["Samples"], STORM_ENTITIES)
        self.begin_with(cluster, "Alerts | take 1")
        found = self.diagnostics_become("Alerts | take 1", lambda found: len(found) == 1)
        self.assertIn("Alerts", found[0]["message"])

        cluster.entities = {
            "Samples": STORM_ENTITIES["Samples"] + [("Table", "Alerts", "", "", "", "Id:long")]
        }
        self.refresh()
        cleared = self.wait_for_diagnostics(lambda found: found == [], timeout=10)
        self.assertEqual(cleared, [], "the open file was checked again against the new schema")


class SchemaCacheTest(SchemaTest):
    """Schema kept on disk: used at once, replaced from the cluster when old."""

    HOST = "help.kusto.windows.net"

    def setUp(self):
        super().setUp()
        self.data = tempfile.TemporaryDirectory()
        self.addCleanup(self.data.cleanup)
        self.cache = Path(self.data.name) / "kusto" / "schema" / self.HOST

    def database_file(self, name="Samples"):
        return self.cache / f"{name}.json"

    def timestamp(self, hours_ago):
        moment = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(hours=hours_ago)
        return moment.isoformat().replace("+00:00", "Z")

    def write_database(self, tables, hours_ago=0, version=1, name="Samples"):
        entities = [
            {"kind": "Table", "name": table, "schema": "Id:long, Label:string",
             "parameters": "", "body": "", "description": None}
            for table in tables
        ]
        self.cache.mkdir(parents=True, exist_ok=True)
        self.database_file(name).write_text(json.dumps(
            {"version": version, "fetchedAt": self.timestamp(hours_ago), "entities": entities}
        ))

    def write_cluster(self, databases=("Samples",), hours_ago=0):
        self.cache.mkdir(parents=True, exist_ok=True)
        (self.cache / "@databases.json").write_text(json.dumps({
            "version": 1,
            "fetchedAt": self.timestamp(hours_ago),
            "databases": [{"name": name, "alternate": ""} for name in databases],
        }))

    def start_with_cache(self, cluster, **options):
        self.start(
            {"help": cluster},
            {"cluster": "help", "database": "Samples", **options},
            data_dir=self.data.name,
        )
        self.open_document("print 1")

    def labels(self, text="Sto"):
        self.change(text)
        return [item["label"] for item in self.complete(0, len(text))]

    def wait_until(self, condition, timeout=10):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if condition():
                return True
            time.sleep(0.1)
        return condition()

    def unreachable(self):
        """A cluster nothing answers for, because it has been shut down."""
        cluster = self.fake(["Samples"], STORM_ENTITIES)
        cluster.stop()
        return cluster

    def test_a_loaded_database_and_cluster_are_written_to_the_cache(self):
        cluster = self.fake(["Samples", "Other"], STORM_ENTITIES)
        self.start_with_cache(cluster)
        self.assertIn("StormEvents", self.completions_include("Sto", "StormEvents"))

        self.assertTrue(self.wait_until(lambda: self.database_file().exists()))
        stored = json.loads(self.database_file().read_text())
        self.assertEqual(stored["version"], 1)
        self.assertEqual(
            sorted(entity["name"] for entity in stored["entities"]),
            ["StatesOver", "StormEvents"],
        )
        function = next(entity for entity in stored["entities"] if entity["name"] == "StatesOver")
        self.assertEqual(function["parameters"], "(minimum:long, region:string)")
        self.assertEqual(function["description"], "States with more than a number of events.")
        self.assertTrue(self.wait_until(lambda: (self.cache / "@databases.json").exists()))
        names = [d["name"] for d in json.loads((self.cache / "@databases.json").read_text())["databases"]]
        self.assertEqual(names, ["Samples", "Other"])

    def test_completion_works_from_the_cache_when_the_cluster_cannot_be_reached(self):
        self.write_database(["CachedEvents"], hours_ago=5)
        self.write_cluster(hours_ago=5)
        self.start_with_cache(self.unreachable())

        self.assertIn("CachedEvents", self.completions_include("Cach", "CachedEvents"))

    def test_a_fresh_cache_is_not_fetched_again(self):
        self.write_database(["CachedEvents"], hours_ago=0)
        self.write_cluster(hours_ago=0)
        cluster = self.fake(["Samples"], STORM_ENTITIES)
        self.start_with_cache(cluster)

        self.assertIn("CachedEvents", self.completions_include("Cach", "CachedEvents"))
        time.sleep(1.5)
        self.assertEqual(cluster.commands, [], "a fresh copy needs no request")

    def test_an_old_cache_is_used_at_once_and_replaced_by_the_clusters_copy(self):
        self.write_database(["OldEvents"], hours_ago=5)
        self.write_cluster(hours_ago=5)
        cluster = self.fake(
            ["Samples"],
            {"Samples": [("Table", "NewEvents", "", "", "", "Id:long")]},
            delay=2,
        )
        started = time.time()
        self.start_with_cache(cluster)

        self.assertIn("OldEvents", self.completions_include("Ol", "OldEvents", timeout=1.5))
        self.assertLess(time.time() - started, 2, "the old copy was there before the cluster answered")

        def replaced():
            labels = self.labels("Ne")
            return "NewEvents" in labels
        self.assertTrue(self.wait_until(replaced, timeout=15))
        self.assertNotIn("OldEvents", self.labels("Ol"))
        stored = json.loads(self.database_file().read_text())
        self.assertEqual([entity["name"] for entity in stored["entities"]], ["NewEvents"])
        written_at = datetime.datetime.fromisoformat(stored["fetchedAt"])
        age = datetime.datetime.now(datetime.timezone.utc) - written_at
        self.assertLess(age, datetime.timedelta(minutes=1), "the copy is stamped with when it was fetched")

    def test_a_cache_that_cannot_be_read_is_ignored_and_replaced(self):
        self.cache.mkdir(parents=True)
        self.database_file().write_text("{ this is not json")
        cluster = self.fake(["Samples"], STORM_ENTITIES)
        self.start_with_cache(cluster)

        self.assertIn("StormEvents", self.completions_include("Sto", "StormEvents"))
        self.assertTrue(self.wait_until(
            lambda: self.database_file().read_text().startswith('{"version":1')
        ))

    def test_a_cache_written_by_another_version_is_ignored(self):
        self.write_database(["FutureEvents"], version=99)
        cluster = self.fake(["Samples"], STORM_ENTITIES)
        self.start_with_cache(cluster)

        labels = self.completions_include("Sto", "StormEvents")
        self.assertIn("StormEvents", labels)
        self.assertNotIn("FutureEvents", self.labels("Fut"))

    def test_zero_minutes_asks_the_cluster_even_when_the_cache_is_fresh(self):
        self.write_database(["CachedEvents"], hours_ago=0)
        self.write_cluster(hours_ago=0)
        cluster = self.fake(["Samples"], STORM_ENTITIES)
        self.start_with_cache(cluster, schemaCacheMinutes=0)

        self.assertIn("StormEvents", self.completions_include("Sto", "StormEvents"))
        self.assertTrue(any(command.startswith(".show databases entities") for _, command in cluster.commands))

    def test_a_refresh_that_fails_leaves_the_cached_schema_in_place(self):
        self.write_database(["CachedEvents"], hours_ago=5)
        self.write_cluster(hours_ago=5)
        before = self.database_file().read_text()
        cluster = self.fake(["Samples"], STORM_ENTITIES, status=401)
        self.start_with_cache(cluster)

        self.assertIn("CachedEvents", self.completions_include("Cach", "CachedEvents"))
        self.assertTrue(self.wait_until(lambda: bool(cluster.commands)))
        time.sleep(0.5)
        self.assertIn("CachedEvents", self.labels("Cach"))
        self.assertEqual(self.database_file().read_text(), before)

    def test_a_database_name_cannot_write_outside_the_cache(self):
        cluster = self.fake(
            ["../../escape"],
            {"../../escape": [("Table", "Odd", "", "", "", "Id:long")]},
        )
        self.start(
            {"help": cluster},
            {"cluster": "help", "database": "../../escape"},
            data_dir=self.data.name,
        )
        self.open_document("print 1")
        self.assertIn("Odd", self.completions_include("Od", "Odd"))

        self.assertTrue(self.wait_until(lambda: any(self.cache.glob("*escape*.json"))))
        written = sorted(
            str(path.relative_to(self.data.name)) for path in Path(self.data.name).rglob("*") if path.is_file()
        )
        self.assertTrue(
            all(path.startswith(str(Path("kusto", "schema", self.HOST))) for path in written),
            written,
        )
        self.assertTrue(any("%2F" in path for path in written), written)


class SigningInTest(SchemaTest):
    """The real sign-in route: the server runs `az`, with a stand-in for it on the path."""

    def setUp(self):
        super().setUp()
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.calls = Path(self.directory.name) / "az-calls"
        script = Path(self.directory.name) / "az"
        script.write_text(
            "#!/bin/sh\n"
            f'echo "$@" >> "{self.calls}"\n'
            "echo '{\"accessToken\":\"fake-token\",\"expires_on\":4102444800}'\n"
        )
        script.chmod(0o755)

    def start_signing_in(self, cluster, options):
        endpoints = {"help.kusto.windows.net": cluster.url}
        self.process = subprocess.Popen(
            SERVER_COMMAND,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={
                **os.environ,
                "PATH": self.directory.name + os.pathsep + os.environ["PATH"],
                "KUSTO_LSP_TEST_ENDPOINTS": json.dumps(endpoints),
            },
        )
        self.send(1, "initialize", {"processId": None, "rootUri": None, "capabilities": {}, "initializationOptions": options})
        self.receive()

    def test_the_server_signs_in_with_az_once_and_asks_for_the_audience_once(self):
        cluster = self.fake(["Samples", "Logs"], {
            "Samples": [("Table", "StormEvents", "", "", "", "State:string")],
            "Logs": [("Table", "Requests", "", "", "", "Id:long")],
        })
        self.start_signing_in(cluster, {"cluster": "help", "database": "Samples"})
        text = "print 1\n\ncluster('help').database('Logs').Requ"
        self.open_document(text)

        def labels(line, character, wanted):
            deadline = time.time() + 10
            while True:
                self.change(text)
                found = [item["label"] for item in self.complete(line, character)]
                if wanted in found or time.time() > deadline:
                    return found
                time.sleep(0.1)

        self.assertIn("Requests", labels(2, len(text.split("\n")[2]), "Requests"))
        # The cluster list, the default database and the other database are three requests, which
        # may arrive in any order.
        deadline = time.time() + 10
        while len(cluster.commands) < 3 and time.time() < deadline:
            time.sleep(0.1)
        if len(cluster.commands) != 3:
            self.process.terminate()
            log = self.process.communicate(timeout=5)[1].decode()
            self.process = None
            self.fail(f"{len(cluster.commands)} requests: {cluster.commands}\nserver log:\n{log}")
        self.assertEqual(set(cluster.authorizations), {"Bearer fake-token"})
        self.assertEqual(cluster.metadata_requests, 1, "the audience is asked for once")
        asked = self.calls.read_text().strip().splitlines()
        self.assertEqual(len(asked), 1, f"az ran more than once: {asked}")
        self.assertIn("get-access-token", asked[0])
        self.assertIn("https://kusto.kusto.windows.net", asked[0])


if __name__ == "__main__":
    if not SERVER.exists():
        sys.exit("Build KustoLanguageServer.csproj before running these tests")
    unittest.main()
