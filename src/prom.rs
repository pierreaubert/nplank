//! Prometheus-compatible monitoring source: query parsing and fetch.
//!
//! Fetches go through the system `curl` (same thin-process pattern as the
//! CI board), so plain HTTP and HTTPS sources both work without adding a
//! second async HTTP stack to the UI process. All JSON parsing is pure
//! and tested; process execution is a thin wrapper.

// Rust guideline compliant 2026-02-21
use std::collections::BTreeMap;
use std::process::Command;

/// Hard curl timeout: a hung source must not stall the auto-refresh loop.
const CURL_TIMEOUT_SECS: &str = "15";
/// Roughly this many points per series keeps panels readable and bounded.
const TARGET_POINTS: i64 = 120;

/// One labelled time series from a range query.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    /// Metric labels, sorted by key (`__name__` included when present).
    pub labels: BTreeMap<String, String>,
    /// `(unix seconds, value)` points in ascending time order.
    pub points: Vec<(i64, f64)>,
}

impl Series {
    /// Legend text: metric name plus non-name labels, `{job="a"}` style.
    pub fn legend(&self) -> String {
        let name = self
            .labels
            .get("__name__")
            .map(String::as_str)
            .unwrap_or("");
        let rest: Vec<String> = self
            .labels
            .iter()
            .filter(|(key, _)| key.as_str() != "__name__")
            .map(|(key, value)| format!("{key}=\"{value}\""))
            .collect();
        if rest.is_empty() {
            name.to_owned()
        } else {
            format!("{name}{{{}}}", rest.join(","))
        }
    }

    /// Most recent finite value, for stat/bar panels.
    pub fn latest(&self) -> Option<f64> {
        self.points.last().map(|(_, value)| *value)
    }
}

/// Percent-encodes one URL query component (unreserved set passes through).
pub fn encode_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn base_trimmed(base: &str) -> &str {
    base.trim().trim_end_matches('/')
}

/// `query_range` URL for `[end-range, end]` at `step` seconds.
pub fn range_url(base: &str, query: &str, start: i64, end: i64, step: i64) -> String {
    format!(
        "{}/api/v1/query_range?query={}&start={start}&end={end}&step={step}",
        base_trimmed(base),
        encode_component(query)
    )
}

/// Metric-names URL (`/api/v1/label/__name__/values`).
pub fn metric_names_url(base: &str) -> String {
    format!("{}/api/v1/label/__name__/values", base_trimmed(base))
}

/// Series URL for one metric (`/api/v1/series?match[]=<name>`).
pub fn series_url(base: &str, metric: &str) -> String {
    format!(
        "{}/api/v1/series?match[]={}",
        base_trimmed(base),
        encode_component(metric)
    )
}

/// Step in seconds targeting ~`TARGET_POINTS` points across `range_secs`.
pub fn step_for_range(range_secs: i64) -> i64 {
    // Below 5s Prometheus default resolution is wasted; above it we thin.
    (range_secs / TARGET_POINTS).max(5)
}

fn check_status(value: &serde_json::Value) -> Result<&serde_json::Value, String> {
    if value.get("status").and_then(serde_json::Value::as_str) == Some("error") {
        let kind = value
            .get("errorType")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("error");
        let message = value
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown Prometheus error");
        return Err(format!("Prometheus {kind}: {message}"));
    }
    value
        .get("data")
        .ok_or_else(|| "Prometheus reply had no data field".to_owned())
}

/// Parses a `query_range` (matrix) response; instant/vector replies with
/// a single `value` per series are accepted too.
pub fn parse_range_response(json: &str) -> Result<Vec<Series>, String> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| format!("Prometheus reply was not JSON: {error}"))?;
    let data = check_status(&value)?;
    let result = data
        .get("result")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Prometheus reply had no result array".to_owned())?;
    let mut series = Vec::new();
    for item in result {
        let labels: BTreeMap<String, String> = item
            .get("metric")
            .and_then(serde_json::Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|text| (key.clone(), text.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut points: Vec<(i64, f64)> = Vec::new();
        if let Some(values) = item.get("values").and_then(serde_json::Value::as_array) {
            for point in values {
                if let Some((ts, parsed)) = parse_point(point) {
                    points.push((ts, parsed));
                }
            }
        } else if let Some(point) = item.get("value")
            && let Some((ts, parsed)) = parse_point(point)
        {
            points.push((ts, parsed));
        }
        points.sort_by_key(|(ts, _)| *ts);
        if !points.is_empty() {
            series.push(Series { labels, points });
        }
    }
    Ok(series)
}

fn parse_point(point: &serde_json::Value) -> Option<(i64, f64)> {
    let pair = point.as_array()?;
    let ts = pair.first()?.as_f64()? as i64;
    // Prometheus encodes values as strings; "NaN"/"+Inf" parse to
    // non-finite floats and are dropped so scales stay sane.
    let value: f64 = pair.get(1)?.as_str()?.parse().ok()?;
    value.is_finite().then_some((ts, value))
}

/// Parses label-values replies (metric names, label values).
pub fn parse_string_list(json: &str) -> Result<Vec<String>, String> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| format!("Prometheus reply was not JSON: {error}"))?;
    let data = check_status(&value)?;
    let items = data
        .as_array()
        .ok_or_else(|| "Prometheus reply data was not a list".to_owned())?;
    Ok(items
        .iter()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect())
}

/// Parses a `/api/v1/series` reply into label sets, one per series.
pub fn parse_series_labels(json: &str) -> Result<Vec<BTreeMap<String, String>>, String> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| format!("Prometheus reply was not JSON: {error}"))?;
    let data = check_status(&value)?;
    let items = data
        .as_array()
        .ok_or_else(|| "Prometheus reply data was not a list".to_owned())?;
    Ok(items
        .iter()
        .filter_map(|item| {
            item.as_object().map(|map| {
                map.iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|text| (key.clone(), text.to_owned()))
                    })
                    .collect()
            })
        })
        .collect())
}

fn curl_get(url: &str) -> Result<String, String> {
    let output = Command::new("curl")
        .args(["-sS", "-g", "--max-time", CURL_TIMEOUT_SECS, url])
        .output()
        .map_err(|error| format!("Could not run curl: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let first = stderr.lines().next().unwrap_or("curl failed").trim();
        Err(first.to_owned())
    }
}

/// Fetches range data for one PromQL query (blocking; call off the UI thread).
pub fn fetch_range(
    base: &str,
    query: &str,
    start: i64,
    end: i64,
    step: i64,
) -> Result<Vec<Series>, String> {
    parse_range_response(&curl_get(&range_url(base, query, start, end, step))?)
}

/// Fetches all metric names from the source (blocking; off the UI thread).
pub fn fetch_metric_names(base: &str) -> Result<Vec<String>, String> {
    parse_string_list(&curl_get(&metric_names_url(base))?)
}

/// Fetches label sets for one metric (blocking; off the UI thread).
pub fn fetch_series_labels(
    base: &str,
    metric: &str,
) -> Result<Vec<BTreeMap<String, String>>, String> {
    parse_series_labels(&curl_get(&series_url(base, metric))?)
}

/// Compact value formatting for stats and axes (`1.2k`, `3.4M`, `950`).
pub fn format_value(value: f64) -> String {
    let abs = value.abs();
    if abs >= 1_000_000.0 {
        format!("{:.2}M", value / 1_000_000.0)
    } else if abs >= 10_000.0 {
        format!("{:.1}k", value / 1_000.0)
    } else if abs >= 100.0 {
        format!("{value:.0}")
    } else if abs >= 1.0 {
        format!("{value:.2}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    } else {
        format!("{value:.3}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_response_parses_matrix_and_drops_nan() {
        let series = parse_range_response(
            r#"{"status":"success","data":{"resultType":"matrix","result":[
              {"metric":{"__name__":"up","job":"node"},"values":[[1700000000,"1"],[1700000060,"0"]]},
              {"metric":{"__name__":"temp"},"values":[[1700000000,"NaN"],[1700000060,"21.5"]]}]}}"#,
        )
        .expect("matrix");
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].legend(), "up{job=\"node\"}");
        assert_eq!(series[0].latest(), Some(0.0));
        assert_eq!(series[1].points, vec![(1_700_000_060, 21.5)]);
    }

    #[test]
    fn error_status_and_vector_shapes() {
        let error = parse_range_response(
            r#"{"status":"error","errorType":"bad_data","error":"parse error"}"#,
        )
        .expect_err("error status");
        assert!(error.contains("bad_data"), "{error}");
        let vector = parse_range_response(
            r#"{"status":"success","data":{"resultType":"vector","result":[{"metric":{"__name__":"up"},"value":[1700000000,"1"]}]}}"#,
        )
        .expect("vector");
        assert_eq!(vector[0].points, vec![(1_700_000_000, 1.0)]);
        assert_eq!(
            parse_string_list(r#"{"status":"success","data":["up","node_cpu"]}"#).expect("names"),
            ["up", "node_cpu"]
        );
    }

    #[test]
    fn urls_encode_promql_and_steps_are_bounded() {
        assert_eq!(
            range_url(
                "http://localhost:9090/",
                "rate(http_requests_total[5m])",
                0,
                60,
                5
            ),
            "http://localhost:9090/api/v1/query_range?query=rate%28http_requests_total%5B5m%5D%29&start=0&end=60&step=5"
        );
        assert_eq!(step_for_range(900), 7);
        assert_eq!(step_for_range(60), 5);
        assert_eq!(
            series_url("http://h:9090", "up"),
            "http://h:9090/api/v1/series?match[]=up"
        );
        assert_eq!(format_value(1_234_567.0), "1.23M");
        assert_eq!(format_value(21.5), "21.5");
        assert_eq!(format_value(0.25), "0.25");
    }
}
