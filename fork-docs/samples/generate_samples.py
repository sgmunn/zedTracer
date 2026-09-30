#!/usr/bin/env python3
"""Generates the synthetic result files in this folder.

Everything here is invented: no real telemetry, identifiers or hostnames. The
output is deterministic, so regenerating produces identical files.

    python3 generate_samples.py                  # the committed small fixtures
    python3 generate_samples.py --large          # also generated/synthetic-large.ktt
    python3 generate_samples.py --large --rows 500000

Files written next to this script:
    synthetic-types.ktt               typed columns, edge values, several tables, saved layout
    synthetic-types.expected.json     expected sort orders and filter results for that file
    synthetic-legacy.kqr              the same format under the legacy extension (small subset)
    synthetic-trace-edge.ktt          activity hierarchy, severity, multipart and call-stack cases
    synthetic-trace-edge.expected.json  the expected outcome of every case in the file above
    generated/synthetic-large.ktt     (only with --large; not committed) scale fixture
"""

import argparse
import json
import random
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
NAMESPACE = uuid.UUID("6f1d0c1e-0000-4000-8000-000000000001")
BASE_TIME = datetime(2026, 1, 1, 0, 0, 0, tzinfo=timezone.utc)
CLUSTER = "example.kusto.windows.net"
DATABASE = "Samples"


# --------------------------------------------------------------------------
# Value helpers: produce the same wire formats the VS Code server emits.
# --------------------------------------------------------------------------

def iso(moment: datetime, extra_ticks: int = 0) -> str:
    """ISO 8601 with seven fractional digits, as .NET's "o" format."""
    ticks = moment.microsecond * 10 + extra_ticks
    return moment.strftime("%Y-%m-%dT%H:%M:%S") + f".{ticks:07d}Z"


def timespan(days=0, hours=0, minutes=0, seconds=0, ticks=0, negative=False) -> str:
    """.NET TimeSpan "c" format: [-][d.]hh:mm:ss[.fffffff]."""
    text = f"{hours:02d}:{minutes:02d}:{seconds:02d}"
    if days:
        text = f"{days}." + text
    if ticks:
        text += f".{ticks:07d}"
    return ("-" if negative else "") + text


def guid(name: str) -> str:
    return str(uuid.uuid5(NAMESPACE, name))


def result_data(tables, query, parameters=None, table_views=None, duration_ms=1234):
    data = {
        "query": query,
        "cluster": CLUSTER,
        "database": DATABASE,
        "parameters": parameters or {},
        "tables": tables,
        "executionStartedAt": "2026-01-01T12:00:00.000Z",
        "executionDurationMs": duration_ms,
        "clientRequestId": "KustoTraceTools;" + guid("run:" + query[:40]),
    }
    if table_views:
        data["tableViews"] = table_views
    return data


def write_json(path: Path, data, indent=2):
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(data, handle, indent=indent, ensure_ascii=False)
        handle.write("\n" if indent else "")
    print(f"wrote {path.relative_to(HERE)}  ({path.stat().st_size:,} bytes)")


# --------------------------------------------------------------------------
# synthetic-types.ktt
# --------------------------------------------------------------------------

def build_types(limit=None):
    columns = [
        ("Id", "long"), ("Flag", "bool"), ("Small", "int"), ("Big", "long"),
        ("Ratio", "real"), ("Money", "decimal"), ("Name", "string"),
        ("Stamp", "datetime"), ("Elapsed", "timespan"), ("Correlation", "guid"),
        ("Payload", "dynamic"), ("Note", "string"),
    ]

    names = ["Step2", "Step10", "step1", "Step 3", "a-b", "ab", "Éclair", "eclair",
             "", None, "Retry 1/3 scheduled", "10:42:11", "café", "cafe", "Zeta", "alpha",
             "Step2", "Step10"]  # repeated values give sort ties
    bigs = [9007199254740993, 9223372036854775807, -9223372036854775808, 0, None,
            -1, 1, 9007199254740992, 42]
    ratios = [5.0, 5, 0.1, 1e-7, 1.7976931348623157e308, -0.0, None, 2.5, 10.25, -3.75]
    stamps = [
        iso(BASE_TIME, 0), iso(BASE_TIME, 7), iso(BASE_TIME, 1),
        "2025-12-31T23:59:59.9999999Z", "2025-01-01T00:00:00.0000000Z",
        "2025-01-01T00:00:00.0000007Z", "2025-01-01T00:00:01.0000000Z",
        None, "2024-02-29T12:00:00.0000000Z", "2026-06-15T08:30:00.5000000Z",
    ]
    elapsed = [
        timespan(seconds=30), timespan(days=1), timespan(seconds=5, negative=True),
        timespan(seconds=59), timespan(minutes=1), timespan(days=10),
        timespan(ticks=1), None, timespan(hours=23, minutes=59, seconds=59, ticks=9999999),
        timespan(days=2, hours=3, minutes=4, seconds=5, ticks=5000000),
    ]
    payloads = [
        {"a": 1, "b": [1, 2, 3], "c": {"d": None}},
        [1, "two", {"three": 3}],
        {"b": 1, "a": 2},                       # key order must be preserved
        {"10": "x", "2": "y"},                  # integer-like keys must keep their order
        '{"a":1}',                              # JSON text held in a dynamic column
        '{"k":1,"k":2}',                        # duplicate keys as text
        '{"n":9007199254740993,"r":5.0}',       # big number and 5.0 literal as text
        42,
        "plain text",
        None,
        {"message": "line1\nline2", "text": "tab\there"},
        {"callStack": "at App.Work() at System.IO.File.ReadAllText()"},
    ]
    notes = [
        "has\ttab", "line1\nline2", 'quote "x" inside', "pipe | char", "<b>html</b> & \"amp\"",
        "George <gw@x.com>", "  leading and trailing  ", "😀 emoji and 日本語", "true", "1",
        "line1\r\nline2 windows", None, "", "x" * 5000,
    ]
    flags = [True, False, None]
    smalls = [-2147483648, 2147483647, 0, None, 7]
    money = [12.5, 0, None, 79228162514264337593543950335, -0.01, 100]

    rows = []
    count = 40
    for i in range(count):
        rows.append([
            i + 1,
            flags[i % 3],
            smalls[i % len(smalls)],
            bigs[i % len(bigs)],
            ratios[i % len(ratios)],
            money[i % len(money)],
            names[i % len(names)],
            stamps[i % len(stamps)],
            elapsed[i % len(elapsed)],
            None if i % 11 == 5 else guid(f"row{i}"),
            payloads[i % len(payloads)],
            notes[i % len(notes)],
        ])
    if limit:
        rows = rows[:limit]

    stats = {
        "name": "Stats",
        "columns": [{"name": "Metric", "type": "string"}, {"name": "Value", "type": "long"},
                    {"name": "Unit", "type": "string"}],
        "rows": [["Rows scanned", 1240000, "rows"], ["Extents", 18, None],
                 ["Cache hit ratio", 97, "percent"], ["Elapsed", None, "ms"]],
    }
    empty = {
        "name": "Empty",
        "columns": [{"name": "Timestamp", "type": "datetime"}, {"name": "Level", "type": "long"},
                    {"name": "Message", "type": "string"}],
        "rows": [],
    }
    primary = {
        "name": "PrimaryResult",
        "columns": [{"name": n, "type": t} for n, t in columns],
        "rows": rows,
    }
    # Saved column layout: reordered, some widths, wide gutter.
    order = [6, 0, 7, 1, 2, 3, 4, 5, 8, 9, 10, 11]
    widths = {6: 160, 7: 220, 10: 300}
    table_views = [{
        "name": "PrimaryResult",
        "gutterWidth": 64,
        "columns": [{"index": i, **({"width": widths[i]} if i in widths else {})} for i in order],
    }]
    query = (
        "declare query_parameters(raid:string);\n"
        "datatable(Id:long, Flag:bool, Name:string, Stamp:datetime, Elapsed:timespan)\n"
        "[\n    1, true, 'Step2', datetime(2026-01-01), 30s\n]\n"
        "| where Name has raid"
    )
    tables = [primary, stats, empty]
    if limit:
        tables = [primary]
        table_views = None
    return result_data(tables, query,
                       parameters={"raid": guid("raid")}, table_views=table_views)


def parse_ticks(text):
    """datetime text -> integer 100 ns ticks since 0001-01-01 (exact, no rounding)."""
    date, _, rest = text.rstrip("Z").partition("T")
    whole, _, fraction = rest.partition(".")
    moment = datetime.fromisoformat(f"{date}T{whole or '00:00:00'}").replace(tzinfo=timezone.utc)
    days = (moment.date() - datetime(1, 1, 1).date()).days
    seconds = days * 86400 + moment.hour * 3600 + moment.minute * 60 + moment.second
    return seconds * 10_000_000 + int((fraction or "0").ljust(7, "0")[:7])


def parse_timespan_ticks(text):
    negative = text.startswith("-")
    body = text.lstrip("-")
    days = 0
    if "." in body.split(":")[0]:
        day_text, body = body.split(".", 1)
        days = int(day_text)
    clock, _, fraction = body.partition(".")
    hours, minutes, seconds = (int(x) for x in clock.split(":"))
    ticks = ((days * 24 + hours) * 60 + minutes) * 60 + seconds
    ticks = ticks * 10_000_000 + int((fraction or "0").ljust(7, "0")[:7])
    return -ticks if negative else ticks


def comparable(kind, value):
    """Typed comparison value per spec SRT-5; None is handled by callers."""
    if kind in ("int", "long", "real", "decimal"):
        return value
    if kind == "datetime":
        return parse_ticks(value)
    if kind == "timespan":
        return parse_timespan_ticks(value)
    if kind == "bool":
        return 1 if value else 0
    return value


def build_types_expected(data):
    table = data["tables"][0]
    names = [c["name"] for c in table["columns"]]
    kinds = {c["name"]: c["type"] for c in table["columns"]}
    rows = table["rows"]
    count = len(rows)

    def order(column, descending):
        index = names.index(column)

        def key(row):
            value = rows[row][index]
            return (0, 0) if value is None else (1, comparable(kinds[column], value))
        return sorted(range(count), key=key, reverse=descending)   # reverse keeps equal keys in source order

    sorts = {}
    for column in ("Small", "Big", "Ratio", "Money", "Stamp", "Elapsed", "Flag"):
        sorts[column] = {"ascending": order(column, False), "descending": order(column, True)}

    def matches(column, operator, argument=None):
        index = names.index(column)
        kind = kinds[column]
        result = []
        for row in range(count):
            value = rows[row][index]
            text = "" if value is None else (json.dumps(value, ensure_ascii=False) if isinstance(value, (dict, list)) else str(value))
            if operator == "isEmpty":
                hit = text == ""
            elif operator == "isNotEmpty":
                hit = text != ""
            elif operator == "isTrue":
                hit = value is True
            elif operator == "isFalse":
                hit = value is False
            elif kind in ("string", "guid", "dynamic"):
                folded, wanted = text.casefold(), str(argument).casefold()
                hit = {"contains": wanted in folded, "notContains": wanted not in folded,
                       "equals": folded == wanted, "notEquals": folded != wanted,
                       "startsWith": folded.startswith(wanted)}[operator]
            else:
                if value is None:
                    hit = operator in ("notEquals",)    # null satisfies only the negative operators
                else:
                    left = comparable(kind, value)
                    right = comparable(kind, argument)
                    hit = {"equals": left == right, "notEquals": left != right, "greaterThan": left > right,
                           "greaterThanOrEqual": left >= right, "lessThan": left < right,
                           "lessThanOrEqual": left <= right}[operator]
            if hit:
                result.append(row)
        return result

    cases = [
        ("Big", "greaterThan", 9007199254740992, "exact 64-bit comparison; a double-based parse cannot separate 2^53 from 2^53+1"),
        ("Small", "lessThan", 0, "null does not satisfy an ordering comparison"),
        ("Ratio", "lessThanOrEqual", 0.1, "null does not satisfy an ordering comparison (VS Code reads a null real as 0 and matches it)"),
        ("Stamp", "equals", "2025-01-01T00:00:00.0000000Z", "compares at 100 ns resolution (VS Code compares to the millisecond)"),
        ("Stamp", "greaterThan", "2025-01-01", "date-only text is read as UTC; a value 7 ticks after midnight is after it (VS Code compares to the millisecond and says it is not)"),
        ("Stamp", "notEquals", "2025-01-01T00:00:00.0000000Z", "a null datetime satisfies Not on (VS Code does not match it)"),
        ("Elapsed", "lessThan", "00:01:00", "durations, including negative ones; text order would be wrong for 10.00:00:00"),
        ("Flag", "isTrue", None, None),
        ("Flag", "isFalse", None, None),
        ("Flag", "isEmpty", None, "null bool"),
        ("Name", "contains", "STEP", "case-insensitive"),
        ("Name", "startsWith", "step", None),
        ("Name", "isEmpty", None, "both null and the empty string"),
        ("Payload", "contains", "callstack", "dynamic values are filtered against their compact JSON text"),
        ("Correlation", "isEmpty", None, None),
    ]
    filters = [{"column": c, "operator": o, "value": a, "matchingRows": matches(c, o, a), "note": n}
               for c, o, a, n in cases]
    both = sorted(set(matches("Flag", "isTrue")) & set(matches("Small", "greaterThanOrEqual", 0)))
    filters.append({"column": "combined", "operator": "Flag isTrue AND Small >= 0 (filters on two columns)",
                    "matchingRows": both, "note": "filters on different columns combine with AND"})
    return {
        "description": "Expected sort orders and filter results for the PrimaryResult table of synthetic-types.ktt. "
                       "Row numbers are zero-based source row indexes. Sort: null is the smallest value, ties are "
                       "ordered by source row index in both directions (SRT-5, SRT-6, SRT-3). Filters follow FLT-6.",
        "sorts": sorts,
        "pairwiseStringOrder": [
            {"before": "Step2", "after": "Step10", "note": "natural (numeric-aware) ordering, Q-12"},
            {"before": "alpha", "after": "Zeta", "note": "case-insensitive"},
        ],
        "filters": filters,
        "tableSummary": {
            "tables": [t["name"] + f" ({len(t['rows'])} rows)" for t in data["tables"]],
            "badgeTotal": sum(len(t["rows"]) for t in data["tables"]),
        },
    }


# --------------------------------------------------------------------------
# synthetic-trace-edge.ktt
# --------------------------------------------------------------------------

class Trace:
    """Collects trace events in source order and records what is expected."""

    def __init__(self):
        self.events = []         # dicts, in final source order once built
        self.activities = {}     # name -> metadata for the expected file
        self.tags = {}           # tag -> event
        self.first_seen = []     # activity names in first-observed order

    def aid(self, name):
        return guid("activity:" + name)

    def declare(self, name, parent, issue=None, note=None):
        """Declare the expected parent (a name, or None) and hierarchy issue."""
        self.activities[name] = {"name": name, "expectedParent": parent, "issue": issue,
                                 "note": note}

    def ev(self, name, level, msg=None, parent="<declared>", marker=None, custom=None,
           context=None, tag=None, raw_activity="<name>", raw_parent="<name>"):
        activity = self.aid(name) if raw_activity == "<name>" else raw_activity
        if raw_parent != "<name>":
            parent_value = raw_parent
        else:
            declared = self.activities[name]["expectedParent"] if parent == "<declared>" else parent
            parent_value = self.aid(declared) if declared else None
        event = {"name": name, "activity": activity, "parent": parent_value, "level": level,
                 "message": msg if msg is not None else f"{marker or name} event",
                 "marker": marker if marker is not None else name, "custom": custom,
                 "context": context, "tag": tag}
        self.events.append(event)
        if tag:
            self.tags[tag] = event
        if name not in self.first_seen:
            self.first_seen.append(name)
        return event


def interleave(*lists):
    merged, index = [], 0
    while any(lists):
        for events in lists:
            if events:
                merged.append(events.pop(0))
        index += 1
    return merged


def severity_outcome(levels):
    """The spec's rule (ACT-9, ACT-10) applied to one activity's own levels."""
    def valid(v):
        return isinstance(v, int) and 1 <= v <= 5
    final = levels[-1] if valid(levels[-1]) else None
    earlier = [v for v in levels[:-1] if valid(v) and v <= 3]
    worst = min(earlier) if earlier else None
    if final is not None and final <= 3:
        outcome = {"level": final, "strength": "full"}
    elif (final is None or final >= 4) and worst is not None:
        outcome = {"level": worst, "strength": "muted"}
    elif final is not None:
        outcome = {"level": final, "strength": "full"}
    else:
        return None, False
    return outcome, outcome["level"] <= 3


def build_trace_edge():
    trace = Trace()
    T = trace
    blocks = []   # each block is a list of events; blocks are concatenated or interleaved

    def block(events):
        blocks.append(events)
        return events

    def run(events_fn):
        """Collect the events an authoring function appends, then remove them from trace.events."""
        start = len(T.events)
        events_fn()
        made = T.events[start:]
        del T.events[start:]
        return made

    # ---- T1: deep chains with two equally deep branches (Deepest cycles between them)
    for name, parent in [("t1_root", None), ("t1_c1", "t1_root"), ("t1_c2", "t1_c1"),
                         ("t1_c3", "t1_c2"), ("t1_c4", "t1_c3"), ("t1_c5", "t1_c4"),
                         ("t1_d1", "t1_root"), ("t1_d2", "t1_d1"), ("t1_d3", "t1_d2"),
                         ("t1_d4", "t1_d3"), ("t1_d5", "t1_d4")]:
        T.declare(name, parent)
    root_events = run(lambda: [T.ev("t1_root", 4, "Rollout started"), T.ev("t1_root", 4)])
    chain_c = run(lambda: [T.ev("t1_c1", 4), T.ev("t1_c1", 4), T.ev("t1_c2", 5), T.ev("t1_c2", 4),
                           T.ev("t1_c3", 4), T.ev("t1_c3", 4), T.ev("t1_c3", 4), T.ev("t1_c4", 4),
                           T.ev("t1_c5", 4), T.ev("t1_c5", 2, "Deepest branch failed")])
    chain_d = run(lambda: [T.ev("t1_d1", 4), T.ev("t1_d2", 4), T.ev("t1_d3", 5), T.ev("t1_d4", 4),
                           T.ev("t1_d5", 4), T.ev("t1_d5", 4)])
    block(root_events)
    block(interleave(chain_c, chain_d))

    # ---- T2: severity outcomes; every case is its own child of one root
    T.declare("t2_root", None)
    T.declare("t2_parent_of_error", "t2_root")
    T.declare("t2_child_error", "t2_parent_of_error")
    severity_cases = [
        ("t2_final_error", [4, 2]), ("t2_final_warning", [5, 3]), ("t2_final_critical", [4, 1]),
        ("t2_handled_warning", [3, 4]), ("t2_handled_worst_error", [3, 2, 4]),
        ("t2_handled_then_verbose", [2, 5]), ("t2_normal_then_verbose", [4, 5]),
        ("t2_verbose_then_normal", [5, 4]), ("t2_only_normal", [4, 4]),
        ("t2_no_level", [None, None]), ("t2_invalid_final_after_warning", [3, 9]),
        ("t2_all_invalid", [9, 0]), ("t2_final_warning_after_critical", [1, 3]),
    ]
    for name, _ in severity_cases:
        T.declare(name, "t2_root")

    def t2():
        T.ev("t2_root", 4, "Severity cases")
        for name, levels in severity_cases:
            for level in levels:
                T.ev(name, level, f"{name} level {level}")
        T.ev("t2_parent_of_error", 4)
        T.ev("t2_parent_of_error", 4)
        T.ev("t2_child_error", 4)
        T.ev("t2_child_error", 2, "Child failed; parent events are all normal")
    block(run(t2))

    # ---- T3: hierarchy anomalies
    T.declare("anom_pa", None); T.declare("anom_pb", None)
    T.declare("anom_conflict", None, issue="conflictingParents")
    T.declare("anom_orphan", None, issue="orphan")
    T.declare("anom_orphan_child", "anom_orphan")
    T.declare("anom_partial", "anom_pa")
    T.declare("ws_parent", None); T.declare("ws_child", "ws_parent")
    T.declare("cyc_a", None, issue="cycle", note="earliest-observed activity of the cycle becomes the root")
    T.declare("cyc_b", "cyc_a")
    T.declare("cyc3_x", None, issue="cycle"); T.declare("cyc3_y", "cyc3_x"); T.declare("cyc3_z", "cyc3_y")
    T.declare("cyc_self", None, issue="cycle", note="an activity that names itself as parent")

    def t3():
        T.ev("anom_pa", 4); T.ev("anom_pb", 4)
        # conflicting: two different parents across its rows
        T.ev("anom_conflict", 4, parent="anom_pa")
        T.ev("anom_conflict", 3, "Second parent differs", parent="anom_pb")
        # orphan: parent id that does not exist in the table
        T.ev("anom_orphan", 4, raw_parent=guid("activity:not-in-this-result"))
        T.ev("anom_orphan_child", 4)
        # parent only named on a later row; still a single candidate
        T.ev("anom_partial", 4, parent=None)
        T.ev("anom_partial", 4, parent="anom_pa")
        # parent id carries stray whitespace; ids are trimmed
        T.ev("ws_parent", 4)
        T.ev("ws_child", 4, raw_parent=T.aid("ws_parent") + "  ")
        # cycles
        T.ev("cyc_a", 4, raw_parent=T.aid("cyc_b"))
        T.ev("cyc_b", 4, raw_parent=T.aid("cyc_a"))
        T.ev("cyc3_x", 4, raw_parent=T.aid("cyc3_z"))
        T.ev("cyc3_y", 4, raw_parent=T.aid("cyc3_x"))
        T.ev("cyc3_z", 4, raw_parent=T.aid("cyc3_y"))
        T.ev("cyc_self", 4, raw_parent=T.aid("cyc_self"))
    block(run(t3))

    # missing CurrentActivityId: every such row is its own root; a parent named on such a row is ignored
    def t3b():
        for i, blank in enumerate([None, None, "", None]):
            name = f"missing_{i}"
            T.declare(name, None, issue="missingCurrentActivityId")
            T.ev(name, 4 if i != 2 else 3,
                 f"Row without an activity id ({'empty' if blank == '' else 'null'})",
                 raw_activity=blank, raw_parent=T.aid("t1_root") if i == 3 else None)
    block(run(t3b))

    # ---- T4: a high-volume parent whose children must appear right under its first event
    T.declare("hv_parent", None)
    for i in (1, 2, 3):
        T.declare(f"hv_child{i}", "hv_parent")

    def t4():
        for i in range(400):
            T.ev("hv_parent", 4 if i % 50 else 3, f"Repeated event {i}")
        for i in (1, 2, 3):
            T.ev(f"hv_child{i}", 4)
            T.ev(f"hv_child{i}", 4)
    block(run(t4))

    # ---- multipart messages (M*) and call stacks (C*)
    expected_multipart = []
    expected_callstacks = []

    def mp(case, rows_spec, expect, activity=None, note=None):
        """rows_spec: list of message strings, appended as consecutive events."""
        name = activity or f"mp_{case}"
        T.declare(name, None)
        tags = []
        for idx, text in enumerate(rows_spec):
            tag = f"{case}:{idx}"
            T.ev(name, 4, text, tag=tag)
            tags.append(tag)
        expected_multipart.append({"case": case, "activity": name, "tags": tags,
                                   "expect": expect, "note": note})

    payload = json.dumps({
        "type": "AggregateException", "message": "Provisioning failed",
        "callStack": "at App.Provision() in D:\\a\\_work\\1\\s\\Core\\Provision.cs :line 88 "
                     "at System.Threading.Tasks.Task.Execute()",
        "innerExceptions": [{"type": "SqlTimeout", "callStack": "at Db.Query() at System.Net.Http.HttpClient.SendAsync()"}],
    })
    third = len(payload) // 3
    parts = [payload[:third], payload[third:2 * third], payload[2 * third:]]

    def block_mp():
        mp("M01_complete_ordered", [f"1/3:{parts[0]}", f"2/3:{parts[1]}", f"3/3:{parts[2]}"],
           {"assembled": True, "text": payload, "isJson": True})
        mp("M02_complete_source_order_shuffled", [f"3/3:{parts[2]}", f"1/3:{parts[0]}", f"2/3:{parts[1]}"],
           {"assembled": True, "text": payload, "isJson": True},
           note="parts appear out of order in the source; assembly sorts by part number")
        mp("M03_incomplete", ["1/3:alpha", "2/3:beta"], {"assembled": False})
        mp("M04_mismatched_totals", ["1/2:alpha", "2/3:beta"], {"assembled": False})
        mp("M05_duplicate_part", ["1/2:alpha", "1/2:beta"], {"assembled": False})
        mp("M06_spacing_variant", ["1 / 2 : x", "2 / 2 : y"], {"assembled": True, "text": "xy", "isJson": False})
        mp("M07_total_of_one", ["1/1:x", "1/1:y"], {"assembled": False, "note": "N must be at least 2"})
        mp("M08_ordinary_row_among_parts", ["1/2:x", "hello"], {"assembled": False})
        mp("M09_marker_not_at_start", ["see item 1/3: of the list", "and 2/3: of it", "and 3/3: too"],
           {"assembled": False})
        mp("M10_extra_space_after_colon", ["1/2:  two spaces", "2/2:end"],
           {"assembled": True, "text": " two spacesend", "isJson": False,
            "note": "only one space after the colon belongs to the marker"})
        mp("M11_six_part_plain_text", [f"{i}/6:part{i};" for i in range(1, 7)],
           {"assembled": True, "text": "".join(f"part{i};" for i in range(1, 7)), "isJson": False})
        # Two complete sets interleaved in one activity: selecting all six must not assemble,
        # selecting one set's three rows must.
        T.declare("mp_M12_interleaved", None)
        a_parts, b_parts = ["A1;", "A2;", "A3;"], ["B1;", "B2;", "B3;"]
        tags_a, tags_b, tags_all = [], [], []
        for i in range(3):
            T.ev("mp_M12_interleaved", 4, f"{i + 1}/3:{a_parts[i]}", tag=f"M12:a{i}")
            T.ev("mp_M12_interleaved", 4, f"{i + 1}/3:{b_parts[i]}", tag=f"M12:b{i}")
            tags_a.append(f"M12:a{i}"); tags_b.append(f"M12:b{i}")
        tags_all = [t for pair in zip(tags_a, tags_b) for t in pair]
        expected_multipart.append({"case": "M12_interleaved_all_six", "activity": "mp_M12_interleaved",
                                   "tags": tags_all, "expect": {"assembled": False},
                                   "note": "six rows selected but each marker declares 3"})
        expected_multipart.append({"case": "M12_interleaved_set_a", "activity": "mp_M12_interleaved",
                                   "tags": tags_a, "expect": {"assembled": True, "text": "".join(a_parts), "isJson": False}})
        expected_multipart.append({"case": "M12_interleaved_set_b", "activity": "mp_M12_interleaved",
                                   "tags": tags_b, "expect": {"assembled": True, "text": "".join(b_parts), "isJson": False}})
    block(run(block_mp))

    def cs(case, message, expected_stacks, note=None, custom=None, level=2, is_json=True,
           column="MessageText"):
        """message: a python object (dumped as JSON text) or a string used verbatim."""
        text = message if isinstance(message, str) else json.dumps(message)
        T.declare(f"cs_{case}", None)
        T.ev(f"cs_{case}", level, text, tag=f"cs:{case}", custom=custom)
        expected_callstacks.append({"case": case, "tag": f"cs:{case}", "isJson": is_json,
                                    "column": column, "expectedCallStacks": expected_stacks,
                                    "note": note})

    def block_cs():
        cs("C01_noise_and_generated_names",
           {"callStack": "at Contoso.Service.Handler.<HandleAsync>d__12.MoveNext() "
                         "at System.Runtime.CompilerServices.AsyncTaskMethodBuilder.Start() "
                         "at Contoso.Service.Program.Main() at System.Net.Http.HttpClient.SendAsync() "
                         "at Polly.Retry.AsyncRetryEngine.ImplementationAsync()"},
           ["at Contoso.Service.Handler.HandleAsync()\nat Contoso.Service.Program.Main()"],
           note="from the VS Code unit tests")
        cs("C02_lambda_frame_and_escaped_newlines",
           {"callStack": "t+<>c__DisplayClass21_0.<LoadIntoBufferAsync>b__0(Task copyTask) \\r\\n at System.Threading.Tasks.Task.Execute()"},
           ["t.LoadIntoBufferAsync()"], note="the callStack holds the four-character escape sequences")
        cs("C03_only_framework_frame", {"callStack": "at System.Net.Http.HttpClient.SendAsync()"},
           ["at System.Net.Http.HttpClient.SendAsync()"], note="never erase a stack (EXC-6)")
        cs("C04_every_frame_has_source_location",
           {"callStack": "at App.A() in D:\\a\\_work\\1\\s\\Core\\A.cs :line 10 "
                         "at App.B() in D:\\a\\_work\\1\\s\\Core\\B.cs :line 20 "
                         "at App.C() in D:\\a\\_work\\1\\s\\Core\\C.cs :line 30"},
           ["at App.A() in A.cs:line 10\nat App.B() in B.cs:line 20\nat App.C() in C.cs:line 30"],
           note="Zed (Q-13): one frame per line. VS Code chains all three onto one line.")
        cs("C05_path_with_r_and_n_directories",
           {"callStack": "at App.Run() in D:\\repos\\node\\src\\Foo.cs :line 12\\nat App.Next()"},
           ["at App.Run() in Foo.cs:line 12\nat App.Next()"],
           note="VS Code corrupts the path to 'D: epos ode\\src\\Foo.cs' (spec section 7, row 2)")
        cs("C06_unc_path",
           {"callStack": "at App.Share() in \\\\build01\\share\\src\\Unc.cs :line 5"},
           ["at App.Share() in Unc.cs:line 5"])
        cs("C07_posix_path_left_alone",
           {"callStack": "at App.Posix() in /mnt/build/src/Posix.cs:line 7"},
           ["at App.Posix() in /mnt/build/src/Posix.cs:line 7"], note="POSIX paths are not shortened (EXC-2)")
        cs("C08_nested_and_array",
           {"type": "Outer", "callStack": "at A.Outer() at System.IO.File.Read()",
            "innerException": {"callStack": "at Inner.Work() at System.Threading.Tasks.Task.Execute()"},
            "exceptions": [{"callStack": "at One.Go() at System.Runtime.Foo()"},
                           {"callStack": "at Two.Go() at Polly.Retry.Bar()"}]},
           ["at A.Outer()", "at Inner.Work()", "at One.Go()", "at Two.Go()"],
           note="any depth, including inside arrays (JSN-5)")
        cs("C09_key_casing_and_non_string",
           {"CallStack": "at Upper.Case() at System.Net.Http.X()", "callstack": "at Lower.Case() at System.IO.Y()",
            "callStackHash": "at Not.AStack() at System.IO.Y()", "callStack2": 42},
           ["at Upper.Case()", "at Lower.Case()"],
           note="matched case-insensitively on the exact key name; callStackHash is left alone")
        cs("C10_real_newlines_between_frames",
           {"callStack": "at App.X()\nat System.Runtime.Foo()\nat App.Y()"}, ["at App.X()\nat App.Y()"])
        cs("C11_callstack_in_dynamic_column", {"type": "InColumnToo"},
           ["at Column.Work()"], custom={"exception": {"callStack": "at Column.Work() at System.IO.File.Read()"}},
           note="the same rules apply to a dynamic column's object", column="CustomData")
        cs("C12_scalar_json_strings", "123", [], is_json=False,
           note="JSON scalars are ordinary values, not reformatted (JSN-1)")
        cs("C13_invalid_json", "{not json", [], is_json=False, note="starts with { but does not parse: plain text")
        cs("C14_plain_text_with_escaped_newline", "first line\\nsecond line", [], is_json=False,
           note="not JSON: shown as it is")
    block(run(block_cs))

    # ---- assemble in source order. Blocks were authored as separate lists; concatenate.
    ordered = [event for events in blocks for event in events]
    ordered_names = []
    for event in ordered:
        if event["name"] not in ordered_names:
            ordered_names.append(event["name"])

    # timestamps: strictly increasing, 15 ms apart
    columns = [("TIMESTAMP", "datetime"), ("MessageText", "string"), ("level", "long"),
               ("MarkerName", "string"), ("CustomData", "dynamic"), ("ExecutionContext", "dynamic"),
               ("CurrentActivityId", "string"), ("ParentActivityId", "string"),
               ("RootActivityId", "string"), ("ProcessName", "string")]
    processes = ["Frontend", "Worker", "Scheduler"]
    root = guid("root")
    rows = []
    for index, event in enumerate(ordered):
        rows.append([
            iso(BASE_TIME + timedelta(milliseconds=15 * index)),
            event["message"], event["level"], event["marker"], event["custom"], event["context"],
            event["activity"], event["parent"], root, processes[index % 3],
        ])
        event["row"] = index

    # ---- expected file
    by_activity = {}
    for event in ordered:
        by_activity.setdefault(event["name"], []).append(event)

    def subtree_stats(name, children):
        kids = children.get(name, [])
        stats = [subtree_stats(k, children) for k in kids]
        return (1 + sum(s[0] for s in stats), 1 + max(s[1] for s in stats) if stats else 0)

    # final parents after anomaly repair are exactly the declared expectedParent
    children = {}
    for name in ordered_names:
        meta = T.activities.get(name)
        if meta and meta["expectedParent"]:
            children.setdefault(meta["expectedParent"], []).append(name)

    expected_activities = []
    for name in ordered_names:
        meta = T.activities.get(name)
        if not meta:
            continue
        levels = [e["level"] for e in by_activity[name]]
        outcome, triangle = severity_outcome(levels)
        count, depth = subtree_stats(name, children)
        expected_activities.append({
            "name": name, "activityId": None if meta["issue"] == "missingCurrentActivityId" else T.aid(name),
            "eventCount": len(levels), "firstSourceRow": by_activity[name][0]["row"],
            "parent": meta["expectedParent"], "hierarchyIssue": meta["issue"],
            "severity": outcome, "warningTriangle": triangle,
            "subtreeActivityCount": count, "maxDescendantDepth": depth, "note": meta["note"],
        })
    roots_in_order = [a["name"] for a in expected_activities if a["parent"] is None]

    def tree_depth(name):
        depth = 0
        while T.activities[name]["expectedParent"]:
            name = T.activities[name]["expectedParent"]
            depth += 1
        return depth

    deepest_depth = max(tree_depth(a["name"]) for a in expected_activities)
    deepest_names = [a["name"] for a in expected_activities if tree_depth(a["name"]) == deepest_depth]

    def resolve(tag):
        return T.tags[tag]["row"]

    expected = {
        "description": "Expected outcomes for synthetic-trace-edge.ktt. Source row numbers are "
                       "zero-based indexes into tables[0].rows; the grid shows them plus one.",
        "sourceTable": "PrimaryResult",
        "rowCount": len(rows),
        "severityColumn": "level",
        "severityRows": {
            "untintedRows": [i for i, r in enumerate(rows) if not (isinstance(r[2], int) and 1 <= r[2] <= 5)],
            "tintedCountByLevel": {str(l): sum(1 for r in rows if r[2] == l) for l in (1, 2, 3, 4, 5)},
            "note": "level values 0, 9 and null are not tinted; every other row is tinted by its level",
        },
        "structured": {
            "rootsInFirstObservedOrder": roots_in_order,
            "deepest": {"depth": deepest_depth, "activities": deepest_names,
                        "note": "Deepest cycles through these in first-seen order; depth counted from the root at 0"},
            "activities": expected_activities,
            "note": "Activities not listed here do not exist. For missingCurrentActivityId each row is "
                    "its own root and is labelled '(missing CurrentActivityId)'.",
        },
        "multipart": [
            {**c, "rows": [resolve(t) for t in c["tags"]]} for c in expected_multipart
        ],
        "callStacks": [
            {**c, "row": resolve(c["tag"])} for c in expected_callstacks
        ],
    }
    for group in ("multipart", "callStacks"):
        for item in expected[group]:
            item.pop("tags", None)
            item.pop("tag", None)

    table = {"name": "PrimaryResult", "columns": [{"name": n, "type": t} for n, t in columns], "rows": rows}
    query = ("declare query_parameters(raid:string);\nASTrace\n| where RootActivityId == raid\n"
             "| order by TIMESTAMP asc")
    data = result_data([table], query, parameters={"raid": root})
    return data, expected


# --------------------------------------------------------------------------
# synthetic-large.ktt (generated on demand, not committed)
# --------------------------------------------------------------------------

def build_large(row_count, seed=7):
    rng = random.Random(seed)
    columns = [
        ("Id", "long"), ("TIMESTAMP", "datetime"), ("Level", "long"), ("MessageText", "string"),
        ("MarkerName", "string"), ("CustomData", "dynamic"), ("ExecutionContext", "dynamic"),
        ("CurrentActivityId", "string"), ("ParentActivityId", "string"), ("RootActivityId", "string"),
        ("Region", "string"), ("DurationMs", "real"), ("Elapsed", "timespan"), ("Succeeded", "bool"),
        ("Retries", "int"), ("Correlation", "guid"), ("Attempt", "long"), ("Bucket", "string"),
        ("Note", "string"), ("ProcessName", "string"),
    ]
    markers = [f"Component{n % 12}.Operation{n}" for n in range(60)]
    regions = ["westus2", "eastus", "westeurope", "southeastasia", "centralus", "brazilsouth",
               "uksouth", "japaneast", "canadacentral", "australiaeast"]
    words = ("request retry connection timeout provisioning workspace database scheduler lease "
             "token refresh cache partition shard replica commit rollback snapshot").split()
    activity_count = max(1, row_count // 12)
    parents = [None]
    for i in range(1, activity_count):
        parents.append(rng.randrange(max(0, i - 40), i) if rng.random() > 0.02 else None)
    root = guid("large-root")
    rows = []
    for i in range(row_count):
        activity = rng.randrange(activity_count) if rng.random() < 0.3 else min(activity_count - 1, i // 12)
        level = rng.choices([1, 2, 3, 4, 5], weights=[0.2, 2, 4, 40, 54])[0]
        length = rng.choice([30, 60, 120, 240, 600, 2000]) if rng.random() < 0.97 else 6000
        message = " ".join(rng.choice(words) for _ in range(max(3, length // 7)))
        if rng.random() < 0.08:
            message = json.dumps({"type": "Failure", "message": message[:80],
                                  "callStack": "at App.Work() in D:\\a\\_work\\1\\s\\App.cs :line %d "
                                               "at System.Threading.Tasks.Task.Execute()" % rng.randrange(1, 900)})
        custom = None if rng.random() < 0.6 else {"attempt": rng.randrange(1, 6), "key": rng.choice(words),
                                                  "items": [rng.randrange(100) for _ in range(rng.randrange(1, 8))]}
        rows.append([
            i + 1, iso(BASE_TIME + timedelta(milliseconds=7 * i)), level, message,
            rng.choice(markers), custom, None if rng.random() < 0.05 else {"tenant": guid(f"t{i % 50}")},
            guid(f"a{activity}"), guid(f"a{parents[activity]}") if parents[activity] is not None else None, root,
            rng.choice(regions), None if rng.random() < 0.02 else round(rng.random() * 5000, 3),
            timespan(seconds=rng.randrange(0, 90), ticks=rng.randrange(0, 10_000_000)),
            rng.random() < 0.9 if rng.random() > 0.01 else None,
            rng.randrange(0, 5), guid(f"c{i % 5000}"), rng.randrange(1, 10**6),
            f"bucket-{rng.randrange(200)}", None if rng.random() < 0.3 else rng.choice(words),
            rng.choice(["Frontend", "Worker", "Scheduler", "Gateway"]),
        ])
    table = {"name": "PrimaryResult", "columns": [{"name": n, "type": t} for n, t in columns], "rows": rows}
    return result_data([table], f"// synthetic scale fixture, {row_count} rows\nASTrace | take {row_count}",
                       duration_ms=42000)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--large", action="store_true", help="also write generated/synthetic-large.ktt")
    parser.add_argument("--rows", type=int, default=200_000, help="row count for --large (default 200000)")
    args = parser.parse_args()

    types = build_types()
    write_json(HERE / "synthetic-types.ktt", types)
    write_json(HERE / "synthetic-types.expected.json", build_types_expected(types))
    write_json(HERE / "synthetic-legacy.kqr", build_types(limit=10))
    trace, expected = build_trace_edge()
    write_json(HERE / "synthetic-trace-edge.ktt", trace)
    write_json(HERE / "synthetic-trace-edge.expected.json", expected)
    if args.large:
        write_json(HERE / "generated" / "synthetic-large.ktt", build_large(args.rows), indent=None)


if __name__ == "__main__":
    main()
