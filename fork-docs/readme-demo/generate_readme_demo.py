#!/usr/bin/env python3
"""Writes the mock result files the README screenshots are taken from.

Every value is invented. The StormEvents files have the shape of the `StormEvents` table of
the public `help.kusto.windows.net/Samples` database, and the trace is a made-up checkout
request across four services. Run from anywhere; the files are written beside this script.
"""

import json
import random
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
CLUSTER = "https://help.kusto.windows.net"
DATABASE = "Samples"

STATES = [
    ("TEXAS", 31.0, -99.0),
    ("KANSAS", 38.5, -98.0),
    ("IOWA", 42.0, -93.5),
    ("ILLINOIS", 40.0, -89.0),
    ("MISSOURI", 38.5, -92.5),
    ("GEORGIA", 32.7, -83.5),
    ("MINNESOTA", 46.0, -94.5),
    ("WISCONSIN", 44.5, -89.5),
    ("NEBRASKA", 41.5, -99.8),
    ("NEW YORK", 42.9, -75.5),
]
EVENT_TYPES = ["Thunderstorm Wind", "Hail", "Flash Flood", "Tornado"]
SOURCES = ["Trained Spotter", "Law Enforcement", "Emergency Manager", "Public", "Mesonet"]
PLACES = ["Ashford", "Brookfield", "Cedar Hill", "Dalton", "Elm Grove", "Fairview", "Glenwood"]


def ticks(moment):
    return moment.strftime("%Y-%m-%dT%H:%M:%S.") + f"{moment.microsecond:06d}0Z"


def guid(random_source):
    return str(uuid.UUID(int=random_source.getrandbits(128), version=4))


def statistics(execution_seconds, rows, size, scanned_extents, total_extents):
    return {
        "QueryHash": "5f1c0e9a2b7d4c31",
        "ExecutionTime": execution_seconds,
        "resource_usage": {
            "cache": {
                "shards": {
                    "hot": {"hitbytes": 48_211_840, "missbytes": 1_204_224, "retrievebytes": 1_204_224},
                    "cold": {"hitbytes": 0, "missbytes": 0, "retrievebytes": 0},
                    "bypassbytes": 0,
                }
            },
            "cpu": {
                "user": "00:00:00.2968750",
                "kernel": "00:00:00.0156250",
                "total cpu": "00:00:00.3125000",
                "breakdown": {
                    "query execution": "00:00:00.2968750",
                    "query planning": "00:00:00.0156250",
                },
            },
            "memory": {"peak_per_node": 52_428_800},
            "network": {"inter_cluster_total_bytes": 1_843_221, "cross_cluster_total_bytes": 0},
        },
        "input_dataset_statistics": {
            "extents": {"total": total_extents, "scanned": scanned_extents},
            "rows": {"total": 59_066, "scanned": 59_066},
        },
        "dataset_statistics": [{"table_row_count": rows, "table_size": size}],
    }


def write(name, document):
    path = HERE / name
    path.write_text(json.dumps(document, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"wrote {path.relative_to(HERE.parent.parent)}")


def storm_events(random_source):
    columns = [
        ("StartTime", "datetime"),
        ("EndTime", "datetime"),
        ("EpisodeId", "int"),
        ("EventId", "int"),
        ("State", "string"),
        ("EventType", "string"),
        ("InjuriesDirect", "int"),
        ("DeathsDirect", "int"),
        ("DamageProperty", "long"),
        ("Source", "string"),
        ("BeginLocation", "string"),
        ("BeginLat", "real"),
        ("BeginLon", "real"),
        ("EventNarrative", "string"),
        ("StormSummary", "dynamic"),
    ]
    rows = []
    start = datetime(2007, 4, 1, tzinfo=timezone.utc)
    for index in range(240):
        state, latitude, longitude = STATES[random_source.randrange(len(STATES))]
        event_type = random_source.choices(EVENT_TYPES, weights=[5, 4, 2, 1])[0]
        begin = start + timedelta(minutes=random_source.randrange(0, 60 * 24 * 60))
        end = begin + timedelta(minutes=random_source.choice([5, 10, 20, 45, 90, 180]))
        damage = random_source.choice([0, 0, 0, 2_000, 5_000, 15_000, 50_000, 250_000, 1_200_000])
        injuries = random_source.choice([0] * 12 + [1, 2, 4])
        deaths = 1 if event_type == "Tornado" and random_source.random() < 0.08 else 0
        place = random_source.choice(PLACES)
        begin_latitude = round(latitude + random_source.uniform(-1.5, 1.5), 4)
        begin_longitude = round(longitude + random_source.uniform(-1.5, 1.5), 4)
        narrative = {
            "Thunderstorm Wind": f"Several trees were blown down near {place}, one onto a parked car.",
            "Hail": f"Hail up to the size of golf balls fell for about ten minutes in {place}.",
            "Flash Flood": f"Water up to a foot deep flowed across the main road through {place}.",
            "Tornado": f"A brief tornado touched down west of {place}, damaging two barns.",
        }[event_type]
        summary = {
            "TotalDamages": damage,
            "StartTime": ticks(begin),
            "EndTime": ticks(end),
            "Details": {
                "Description": narrative,
                "Location": state,
            },
        }
        rows.append(
            [
                ticks(begin),
                ticks(end),
                10_000 + index // 3,
                60_000 + index,
                state,
                event_type,
                injuries,
                deaths,
                damage,
                random_source.choice(SOURCES),
                place.upper(),
                begin_latitude,
                begin_longitude,
                narrative,
                summary,
            ]
        )
    rows.sort(key=lambda row: row[0])
    write(
        "storm-events.ktt",
        {
            "query": "// Storm events from April and May 2007\n"
            "StormEvents\n"
            "| where StartTime between (datetime(2007-04-01) .. datetime(2007-06-01))\n"
            "| project StartTime, EndTime, EpisodeId, EventId, State, EventType, InjuriesDirect,\n"
            "    DeathsDirect, DamageProperty, Source, BeginLocation, BeginLat, BeginLon,\n"
            "    EventNarrative, StormSummary\n"
            "| take 240",
            "cluster": CLUSTER,
            "database": DATABASE,
            "parameters": {},
            "tables": [
                {
                    "name": "PrimaryResult",
                    "columns": [{"name": name, "type": kind} for name, kind in columns],
                    "rows": rows,
                }
            ],
            "executionStartedAt": "2026-11-12T14:05:12.000Z",
            "executionDurationMs": 412,
            "clientRequestId": "KustoTraceTools;8a8f1f0e-2a51-4c49-9d43-1f2e4cc0d001",
            "queryStatistics": statistics(0.3281, len(rows), 96_512, 1, 1),
        },
    )


def storm_events_by_week(random_source):
    rows = []
    week = datetime(2007, 1, 1, tzinfo=timezone.utc)
    base = {"Thunderstorm Wind": 70, "Hail": 55, "Flash Flood": 20, "Tornado": 8}
    for index in range(52):
        # Severe weather peaks in late spring and early summer.
        season = max(0.15, 1 - abs(index - 23) / 18)
        for event_type in EVENT_TYPES:
            count = int(base[event_type] * season * random_source.uniform(0.6, 1.5) * 2.2)
            rows.append([ticks(week), event_type, count])
        week += timedelta(days=7)
    write(
        "storm-events-by-week.ktt",
        {
            "query": "// Storm events a week, by type\n"
            "StormEvents\n"
            "| where EventType in (\"Thunderstorm Wind\", \"Hail\", \"Flash Flood\", \"Tornado\")\n"
            "| summarize Events = count() by bin(StartTime, 7d), EventType\n"
            "| render timechart with (title=\"Storm events a week, 2007\")",
            "cluster": CLUSTER,
            "database": DATABASE,
            "parameters": {},
            "tables": [
                {
                    "name": "PrimaryResult",
                    "columns": [
                        {"name": "StartTime", "type": "datetime"},
                        {"name": "EventType", "type": "string"},
                        {"name": "Events", "type": "long"},
                    ],
                    "rows": rows,
                }
            ],
            "executionStartedAt": "2026-11-12T14:07:40.000Z",
            "executionDurationMs": 288,
            "clientRequestId": "KustoTraceTools;8a8f1f0e-2a51-4c49-9d43-1f2e4cc0d002",
            "queryStatistics": statistics(0.1875, len(rows), 6_240, 1, 1),
            "charts": [
                {
                    "options": {
                        "type": "TimeLine",
                        "title": "Storm events a week, 2007",
                        "xColumn": "StartTime",
                        "yColumns": ["Events"],
                        "seriesColumns": ["EventType"],
                        "yTitle": "Events",
                        "referenceLines": [
                            {
                                "orientation": "Horizontal",
                                "value": 150,
                                "label": "Alert threshold",
                                "color": "Red",
                            },
                            {
                                "orientation": "Vertical",
                                "value": "2007-05-04",
                                "label": "Greensburg outbreak",
                            },
                        ],
                    }
                }
            ],
        },
    )


def storm_events_by_state(random_source):
    rows = []
    for state, _, _ in STATES:
        for event_type in EVENT_TYPES:
            weight = {"Thunderstorm Wind": 900, "Hail": 700, "Flash Flood": 260, "Tornado": 90}[event_type]
            rows.append([state, event_type, int(weight * random_source.uniform(0.3, 1.6))])
    write(
        "storm-events-by-state.ktt",
        {
            "query": "// Which states had the most severe weather\n"
            "StormEvents\n"
            "| where EventType in (\"Thunderstorm Wind\", \"Hail\", \"Flash Flood\", \"Tornado\")\n"
            "| summarize Events = count() by State, EventType\n"
            "| render columnchart with (kind=stacked, title=\"Severe weather by state\")",
            "cluster": CLUSTER,
            "database": DATABASE,
            "parameters": {},
            "tables": [
                {
                    "name": "PrimaryResult",
                    "columns": [
                        {"name": "State", "type": "string"},
                        {"name": "EventType", "type": "string"},
                        {"name": "Events", "type": "long"},
                    ],
                    "rows": rows,
                }
            ],
            "executionStartedAt": "2026-11-12T14:08:03.000Z",
            "executionDurationMs": 241,
            "clientRequestId": "KustoTraceTools;8a8f1f0e-2a51-4c49-9d43-1f2e4cc0d003",
            "queryStatistics": statistics(0.1563, len(rows), 1_920, 1, 1),
            "charts": [
                {
                    "options": {
                        "type": "ColumnStacked",
                        "title": "Severe weather by state",
                        "xColumn": "State",
                        "yColumns": ["Events"],
                        "seriesColumns": ["EventType"],
                    }
                }
            ],
        },
    )


def checkout_trace(random_source):
    """A checkout request: the storefront calls the order service, which reserves stock and
    takes a payment. The first call to the card network times out and is retried."""
    root_id = guid(random_source)
    started = datetime(2026, 11, 12, 13, 41, 7, 250_000, tzinfo=timezone.utc)
    activities = {}
    rows = []

    def activity(key, parent, process, marker, start_ms, end_ms, events):
        activity_id = root_id if parent is None else guid(random_source)
        activities[key] = activity_id
        parent_id = None if parent is None else activities[parent]
        timeline = [(start_ms, f"Monitored scope start: {marker}", 5, None)]
        timeline += events
        timeline.append((end_ms, f"Monitored scope end: {marker}", 5, None))
        for offset_ms, message, level, custom in timeline:
            rows.append(
                [
                    ticks(started + timedelta(milliseconds=offset_ms)),
                    message,
                    level,
                    marker,
                    custom,
                    activity_id,
                    parent_id,
                    root_id,
                    process,
                ]
            )

    order_id = "ORD-2026-118342"
    activity(
        "root", None, "Storefront", "PlaceOrder", 0, 1_860,
        [
            (6, f"Checkout requested for cart with 3 items, order {order_id}", 4, {"items": 3, "currency": "USD"}),
            (1_852, f"Order {order_id} placed", 4, None),
        ],
    )
    activity(
        "create", "root", "OrderService", "CreateOrder", 40, 1_800,
        [(48, f"Validating order {order_id}", 4, None), (1_790, f"Order {order_id} confirmed", 4, None)],
    )
    activity(
        "reserve", "create", "InventoryService", "ReserveStock", 80, 430,
        [(410, "Reserved 3 items in warehouse SEA-02", 4, {"warehouse": "SEA-02", "skus": ["A-1001", "B-2210", "C-0042"]})],
    )
    activity(
        "levels", "reserve", "InventoryService", "ReadStockLevels", 100, 380,
        [(104, "Stock cache miss, reading levels from the store", 3, {"cache": "stock-levels", "ageSeconds": 912})],
    )
    activity(
        "authorize", "create", "PaymentGateway", "AuthorizePayment", 450, 1_500,
        [
            (1_275, "Card network did not answer, retrying (attempt 2 of 3)", 3, {"attempt": 2}),
            (1_492, "Payment authorized for USD 184.20", 4, {"amount": 184.20, "currency": "USD"}),
        ],
    )
    activity(
        "network1", "authorize", "PaymentGateway", "CallCardNetwork", 470, 1_270,
        [
            (
                1_268,
                "Card network call timed out after 800 ms",
                2,
                {
                    "exception": "System.TimeoutException",
                    "callStack": "System.TimeoutException: The operation has timed out.\n"
                    "   at Orders.Payments.CardNetworkClient.AuthorizeAsync(PaymentRequest request) in CardNetworkClient.cs:line 88\n"
                    "   at Orders.Payments.AuthorizationService.AuthorizeAsync(Order order) in AuthorizationService.cs:line 41",
                },
            )
        ],
    )
    activity(
        "network2", "authorize", "PaymentGateway", "CallCardNetwork", 1_280, 1_480,
        [(1_470, "Card network approved the payment", 4, {"approvalCode": "A71Q3"})],
    )
    activity(
        "write", "create", "OrderService", "WriteOrder", 1_520, 1_760,
        [(1_610, f"Wrote order {order_id} to the orders store", 4, None)],
    )
    activity(
        "render", "root", "Storefront", "RenderConfirmation", 1_810, 1_848,
        [],
    )
    rows.sort(key=lambda row: row[0])
    write(
        "checkout-trace.ktt",
        {
            "query": "// Checkout of order ORD-2026-118342\n"
            "declare query_parameters(raid:string);\n"
            "CheckoutTraces\n"
            "| where RootActivityId == raid\n"
            "| order by TIMESTAMP asc",
            "cluster": CLUSTER,
            "database": DATABASE,
            "parameters": {"raid": root_id},
            "tables": [
                {
                    "name": "PrimaryResult",
                    "columns": [
                        {"name": "TIMESTAMP", "type": "datetime"},
                        {"name": "MessageText", "type": "string"},
                        {"name": "level", "type": "long"},
                        {"name": "MarkerName", "type": "string"},
                        {"name": "CustomData", "type": "dynamic"},
                        {"name": "CurrentActivityId", "type": "string"},
                        {"name": "ParentActivityId", "type": "string"},
                        {"name": "RootActivityId", "type": "string"},
                        {"name": "ProcessName", "type": "string"},
                    ],
                    "rows": rows,
                }
            ],
            "executionStartedAt": "2026-11-12T14:10:22.000Z",
            "executionDurationMs": 655,
            "clientRequestId": "KustoTraceTools;8a8f1f0e-2a51-4c49-9d43-1f2e4cc0d004",
        },
    )


def main():
    random_source = random.Random(2007)
    storm_events(random_source)
    storm_events_by_week(random_source)
    storm_events_by_state(random_source)
    checkout_trace(random_source)


if __name__ == "__main__":
    main()
