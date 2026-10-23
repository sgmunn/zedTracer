//! What a query cost the service: the `QueryResourceConsumption` payload a v2 response ends with,
//! read into the figures the Stats tab shows.
//!
//! The result file keeps the payload exactly as the service sent it, so figures this module does
//! not read are still there to look at in the raw JSON.

use serde_json::{Map, Value};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryStatistics {
    pub query_hash: Option<String>,
    /// How long the service says the query ran, in seconds.
    pub execution_seconds: Option<f64>,
    /// What the cluster the query was sent to used.
    pub usage: ResourceUsage,
    /// What each other cluster the query reached used, in the order the service listed them.
    pub cross_cluster: Vec<(String, ResourceUsage)>,
    pub extents: Option<ScanCount>,
    pub rows: Option<ScanCount>,
    /// The size of each result table, in the order of the tables.
    pub result_tables: Vec<TableSize>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResourceUsage {
    /// CPU times are kept as the timespan text the service sent, such as `00:00:00.0156250`.
    pub cpu_total: Option<String>,
    pub cpu_user: Option<String>,
    pub cpu_kernel: Option<String>,
    pub cpu_execution: Option<String>,
    pub cpu_planning: Option<String>,
    pub memory_peak_per_node: Option<u64>,
    pub hot_cache: Option<CacheBytes>,
    pub cold_cache: Option<CacheBytes>,
    pub cache_bypass_bytes: Option<u64>,
    pub inter_cluster_bytes: Option<u64>,
    pub cross_cluster_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheBytes {
    pub hit: u64,
    pub miss: u64,
    pub retrieve: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanCount {
    pub total: u64,
    pub scanned: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TableSize {
    pub rows: u64,
    pub bytes: u64,
}

impl QueryStatistics {
    /// Reads what it can of a payload. A figure the payload lacks, or holds in a form this does
    /// not expect, is left out rather than failing the whole.
    pub fn from_payload(payload: &Value) -> Self {
        let input = payload.get("input_dataset_statistics");
        Self {
            query_hash: payload
                .get("QueryHash")
                .and_then(Value::as_str)
                .map(str::to_string),
            execution_seconds: payload.get("ExecutionTime").and_then(Value::as_f64),
            usage: payload
                .get("resource_usage")
                .map(ResourceUsage::from_value)
                .unwrap_or_default(),
            cross_cluster: payload
                .get("cross_cluster_resource_usage")
                .and_then(Value::as_object)
                .map(Map::iter)
                .into_iter()
                .flatten()
                .map(|(cluster, usage)| (cluster.clone(), ResourceUsage::from_value(usage)))
                .collect(),
            extents: input.and_then(|input| ScanCount::from_value(input.get("extents")?)),
            rows: input.and_then(|input| ScanCount::from_value(input.get("rows")?)),
            result_tables: payload
                .get("dataset_statistics")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .filter_map(|table| {
                    Some(TableSize {
                        rows: table.get("table_row_count")?.as_u64()?,
                        bytes: table.get("table_size")?.as_u64()?,
                    })
                })
                .collect(),
        }
    }
}

impl ResourceUsage {
    fn from_value(usage: &Value) -> Self {
        let cpu = usage.get("cpu");
        let cpu_text = |path: &[&str]| {
            let mut value = cpu?;
            for key in path {
                value = value.get(key)?;
            }
            value.as_str().map(str::to_string)
        };
        let shards = usage.get("cache").and_then(|cache| cache.get("shards"));
        let network = usage.get("network");
        Self {
            cpu_total: cpu_text(&["total cpu"]),
            cpu_user: cpu_text(&["user"]),
            cpu_kernel: cpu_text(&["kernel"]),
            cpu_execution: cpu_text(&["breakdown", "query execution"]),
            cpu_planning: cpu_text(&["breakdown", "query planning"]),
            memory_peak_per_node: usage
                .get("memory")
                .and_then(|memory| memory.get("peak_per_node")?.as_u64()),
            hot_cache: shards.and_then(|shards| CacheBytes::from_value(shards.get("hot")?)),
            cold_cache: shards.and_then(|shards| CacheBytes::from_value(shards.get("cold")?)),
            cache_bypass_bytes: shards.and_then(|shards| shards.get("bypassbytes")?.as_u64()),
            inter_cluster_bytes: network
                .and_then(|network| network.get("inter_cluster_total_bytes")?.as_u64()),
            cross_cluster_bytes: network
                .and_then(|network| network.get("cross_cluster_total_bytes")?.as_u64()),
        }
    }
}

impl CacheBytes {
    fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            hit: value.get("hitbytes")?.as_u64()?,
            miss: value.get("missbytes")?.as_u64()?,
            retrieve: value.get("retrievebytes")?.as_u64().unwrap_or_default(),
        })
    }
}

impl ScanCount {
    fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            total: value.get("total")?.as_u64()?,
            scanned: value.get("scanned")?.as_u64()?,
        })
    }
}

/// A byte count the way the service's own tools write it: `0 bytes`, `54 bytes`, `1.0 MB`, with
/// 1024 bytes to the kilobyte.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return if bytes == 1 {
            "1 byte".to_string()
        } else {
            format!("{bytes} bytes")
        };
    }
    let mut size = bytes as f64 / 1024.0;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload the service sent for a query that read another cluster, as Azure Data Explorer
    /// showed it.
    const CROSS_CLUSTER: &str = r#"{
      "QueryHash": "be3cfb5d5a8bdc89",
      "ExecutionTime": 28.8199127,
      "resource_usage": {
        "cache": {"shards": {"hot": {"hitbytes": 0, "missbytes": 0, "retrievebytes": 0},
                             "cold": {"hitbytes": 0, "missbytes": 0, "retrievebytes": 0},
                             "bypassbytes": 0}},
        "cpu": {"user": "00:00:00.0156250", "kernel": "00:00:00", "total cpu": "00:00:00.0156250",
                "breakdown": {"query execution": "00:00:00", "query planning": "00:00:00.0156250"}},
        "memory": {"peak_per_node": 1050944},
        "network": {"inter_cluster_total_bytes": 693, "cross_cluster_total_bytes": 3251}
      },
      "input_dataset_statistics": {
        "extents": {"total": 0, "scanned": 0, "scanned_min_datetime": "0001-01-01T00:00:00.0000000Z",
                    "scanned_max_datetime": "0001-01-01T00:00:00.0000000Z"},
        "rows": {"total": 0, "scanned": 0},
        "rowstores": {"scanned_rows": 0, "scanned_values_size": 0},
        "shards": {"queries_generic": 0, "queries_specialized": 0}
      },
      "dataset_statistics": [{"table_row_count": 3, "table_size": 54}],
      "cross_cluster_resource_usage": {
        "https://pbipnortheurope.northeurope.kusto.windows.net/": {
          "cache": {"shards": {"hot": {"hitbytes": 399787426605, "missbytes": 0, "retrievebytes": 0},
                               "cold": {"hitbytes": 0, "missbytes": 0, "retrievebytes": 0},
                               "bypassbytes": 0}},
          "cpu": {"user": "00:14:32.6093750", "kernel": "00:03:29.6718750", "total cpu": "00:18:02.2812500",
                  "breakdown": {"query execution": "00:18:02.2812500", "query planning": "00:00:00"}},
          "memory": {"peak_per_node": 2442367072},
          "network": {"inter_cluster_total_bytes": 8327554906, "cross_cluster_total_bytes": 0}
        }
      }
    }"#;

    #[test]
    fn reads_the_figures_of_the_cluster_and_of_each_cluster_it_reached() {
        let payload: Value = serde_json::from_str(CROSS_CLUSTER).unwrap();
        let statistics = QueryStatistics::from_payload(&payload);
        assert_eq!(statistics.query_hash.as_deref(), Some("be3cfb5d5a8bdc89"));
        assert_eq!(statistics.execution_seconds, Some(28.8199127));
        assert_eq!(
            statistics.usage.cpu_total.as_deref(),
            Some("00:00:00.0156250")
        );
        assert_eq!(statistics.usage.cpu_kernel.as_deref(), Some("00:00:00"));
        assert_eq!(
            statistics.usage.cpu_planning.as_deref(),
            Some("00:00:00.0156250")
        );
        assert_eq!(statistics.usage.memory_peak_per_node, Some(1050944));
        assert_eq!(statistics.usage.hot_cache, Some(CacheBytes::default()));
        assert_eq!(statistics.usage.cross_cluster_bytes, Some(3251));
        assert_eq!(statistics.extents, Some(ScanCount::default()));
        assert_eq!(statistics.result_tables, [TableSize { rows: 3, bytes: 54 }]);

        let [(cluster, remote)] = statistics.cross_cluster.as_slice() else {
            panic!("expected one other cluster: {:?}", statistics.cross_cluster);
        };
        assert_eq!(
            cluster,
            "https://pbipnortheurope.northeurope.kusto.windows.net/"
        );
        assert_eq!(remote.cpu_total.as_deref(), Some("00:18:02.2812500"));
        assert_eq!(remote.hot_cache.map(|cache| cache.hit), Some(399787426605));
        assert_eq!(remote.inter_cluster_bytes, Some(8327554906));
    }

    #[test]
    fn a_payload_missing_parts_gives_what_it_has() {
        let payload: Value =
            serde_json::from_str(r#"{"ExecutionTime": 0.5, "resource_usage": {"cpu": 3}}"#)
                .unwrap();
        let statistics = QueryStatistics::from_payload(&payload);
        assert_eq!(statistics.execution_seconds, Some(0.5));
        assert_eq!(statistics.usage, ResourceUsage::default());
        assert!(statistics.cross_cluster.is_empty());
        assert_eq!(statistics.rows, None);
        assert_eq!(
            QueryStatistics::from_payload(&Value::Null),
            QueryStatistics::default()
        );
    }

    #[test]
    fn bytes_are_written_in_the_largest_whole_unit() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(1), "1 byte");
        assert_eq!(format_bytes(1023), "1023 bytes");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1050944), "1.0 MB");
        assert_eq!(format_bytes(2442367072), "2.3 GB");
        assert_eq!(format_bytes(399787426605), "372.3 GB");
    }
}
