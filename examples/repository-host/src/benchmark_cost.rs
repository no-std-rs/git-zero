//! A fixed, comparable objective for the local workerd benchmark.
//!
//! One monthly unit is 1,000 repetitions of 32 pushes and 16 varied fetches,
//! plus one repository's observed R2 peak retained for 30 days. All four
//! size/cache profiles have equal weight. Allowances, invoice rounding and the
//! account minimum never enter this objective. Unknown compute stays unknown.
use crate::{
    accounting::{ServiceUsage, UsageCounters},
    cost::{self, AccountAllowances, MonthlyScenario, StorageProjection},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

const REPEATS: u64 = 1_000;
const RESPONSE_BYTES: u64 = 32 * 1024 * 1024;
const RESPONSE_ENTRIES: u64 = 256;
pub const MINIMUM_IMPROVEMENT: f64 = 0.01;
pub const PLATEAU_REJECTED_CANDIDATES: usize = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Distribution {
    pub mean: f64,
    pub min: f64,
    pub max: f64,
    pub median: f64,
    pub p95: f64,
}
impl Distribution {
    fn of(mut values: Vec<f64>) -> io::Result<Self> {
        require(
            !values.is_empty() && values.iter().all(|v| v.is_finite() && *v >= 0.0),
            "missing or invalid measurements",
        )?;
        values.sort_by(f64::total_cmp);
        let n = values.len();
        let median = if n % 2 == 0 {
            (values[n / 2 - 1] + values[n / 2]) / 2.0
        } else {
            values[n / 2]
        };
        Ok(Self {
            mean: values.iter().sum::<f64>() / n as f64,
            min: values[0],
            max: values[n - 1],
            median,
            p95: values[(n * 95).div_ceil(100) - 1],
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyGuard {
    pub host_ms: Distribution,
    pub git_ms: Distribution,
    /// Ratios of paired request latencies, not a ratio of unrelated summaries.
    pub paired_host_to_git: Distribution,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostTrial {
    pub sample: i64,
    pub fixed_workload: UsageCounters,
    pub known_monthly_fixed_workload_usd: f64,
    pub known_monthly_whole_trace_usd: f64,
    pub known_usd_per_1000_pushes: f64,
    pub known_usd_per_1000_varied_fetches: f64,
    pub resource_monthly_usd: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileCost {
    pub profile: String,
    /// Canonical workload/configuration identity; code hashes are provenance.
    pub identity: Value,
    pub source_report_sha256: String,
    pub host_binary_sha256: String,
    pub worker_source_sha256: String,
    pub trials: Vec<CostTrial>,
    pub known_monthly_fixed_workload_usd: Distribution,
    pub known_monthly_whole_trace_usd: Distribution,
    pub unmeasured_billing_dimensions: Vec<String>,
    pub total_estimate_usd: Option<f64>,
    pub guards: BTreeMap<String, Distribution>,
    pub latency: BTreeMap<String, LatencyGuard>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkCost {
    pub schema_version: u64,
    pub pricing_verified_date: String,
    pub objective: String,
    pub monthly_workload_repetitions: u64,
    pub profiles: BTreeMap<String, ProfileCost>,
    pub known_monthly_fixed_workload_usd: f64,
    pub conservative_trial_min_usd: f64,
    pub conservative_trial_max_usd: f64,
    pub total_estimate_usd: Option<f64>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Improved,
    Rejected,
    Inconclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostComparison {
    pub decision: Decision,
    pub known_cost_reduction_fraction: f64,
    pub reasons: Vec<String>,
    /// Only independently implemented, viable rejected designs count. A noisy
    /// or incomparable run never contributes to a plateau declaration.
    pub eligible_rejected_candidate_for_plateau: bool,
    pub plateau_requires_independent_rejected_candidates: usize,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
fn require(condition: bool, message: &str) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(invalid(message))
    }
}
fn field<'a>(value: &'a Value, key: &str) -> io::Result<&'a Value> {
    value
        .get(key)
        .filter(|v| !v.is_null())
        .ok_or_else(|| invalid(format!("missing {key}")))
}
fn count(value: &Value, key: &str) -> io::Result<u64> {
    let value = field(value, key)?;
    // hosting-bench's generic metrics delta serializes exact byte counts as
    // floating point JSON numbers. Accept only exactly representable integers.
    value
        .as_u64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|n| {
                    n.is_finite() && *n >= 0.0 && *n <= 9_007_199_254_740_991.0 && n.fract() == 0.0
                })
                .map(|n| n as u64)
        })
        .ok_or_else(|| invalid(format!("invalid integer {key}")))
}
fn number(value: &Value, key: &str) -> io::Result<f64> {
    let n = field(value, key)?
        .as_f64()
        .ok_or_else(|| invalid(format!("invalid {key}")))?;
    require(
        n.is_finite() && n >= 0.0,
        "negative or nonfinite measurement",
    )?;
    Ok(n)
}
fn text<'a>(value: &'a Value, key: &str) -> io::Result<&'a str> {
    field(value, key)?
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| invalid(format!("invalid string {key}")))
}
fn array<'a>(value: &'a Value, key: &str) -> io::Result<&'a Vec<Value>> {
    field(value, key)?
        .as_array()
        .ok_or_else(|| invalid(format!("invalid array {key}")))
}

fn counters(value: &Value) -> io::Result<UsageCounters> {
    // UsageCounters supports legacy serde defaults; this objective deliberately
    // requires the full modern trace, including explicit null measurements.
    for key in [
        "worker_requests",
        "worker_gateway_requests",
        "worker_service_requests",
        "worker_observer_requests",
        "r2_class_a",
        "r2_class_b",
        "r2_deletes",
        "r2_read_bytes",
        "r2_written_bytes",
        "do_requests",
    ] {
        count(value, key)?;
    }
    for key in ["do_rows_read", "do_rows_written"] {
        let v = value
            .get(key)
            .ok_or_else(|| invalid(format!("missing measurement {key}")))?;
        if !v.is_null() {
            number(value, key)?;
        }
    }
    let result: UsageCounters = serde_json::from_value(value.clone()).map_err(io::Error::other)?;
    require(
        result.worker_requests
            == result
                .worker_gateway_requests
                .checked_add(result.worker_service_requests)
                .and_then(|n| n.checked_add(result.worker_observer_requests))
                .ok_or_else(|| invalid("request count overflow"))?,
        "Worker request categories do not reconcile",
    )?;
    Ok(result)
}

fn snapshot(value: &Value) -> io::Result<ServiceUsage> {
    counters(value)?;
    for key in ["worker_cpu_ms", "do_gb_seconds"] {
        require(
            value.get(key).is_some(),
            "compute measurement must be explicit, including null",
        )?;
        if !value[key].is_null() {
            number(value, key)?;
        }
    }
    require(
        value["accounting_complete"] == true,
        "accounting must explicitly be complete",
    )?;
    require(
        array(value, "accounting_errors")?.is_empty(),
        "accounting errors present",
    )?;
    text(value, "counter_scope")?;
    text(value, "counter_epoch")?;
    for key in ["r2_live_objects", "r2_live_bytes", "r2_peak_bytes"] {
        count(value, key)?;
    }
    let phases = field(value, "by_phase")?
        .as_object()
        .ok_or_else(|| invalid("invalid phase counters"))?;
    require(phases.contains_key("stats"), "observer counters missing")?;
    for phase in phases.values() {
        counters(phase)?;
    }
    let operations = field(value, "operations")?
        .as_object()
        .ok_or_else(|| invalid("invalid operations"))?;
    require(!operations.is_empty(), "binding accounting missing")?;
    for operation in operations.values() {
        let attempts = count(operation, "attempts")?;
        require(
            count(operation, "failures")? == 0 && count(operation, "successes")? == attempts,
            "failed or incomplete service operation",
        )?;
        count(operation, "read_bytes")?;
        count(operation, "written_bytes")?;
    }
    let result: ServiceUsage = serde_json::from_value(value.clone()).map_err(io::Error::other)?;
    result.workload_counters()?;
    Ok(result)
}

fn sum(values: &[UsageCounters]) -> io::Result<UsageCounters> {
    let mut result = json!({});
    for key in [
        "worker_requests",
        "worker_gateway_requests",
        "worker_service_requests",
        "worker_observer_requests",
        "r2_class_a",
        "r2_class_b",
        "r2_deletes",
        "r2_read_bytes",
        "r2_written_bytes",
        "do_requests",
        "do_rows_read",
        "do_rows_written",
    ] {
        let mut total = Some(0_u64);
        for value in values {
            let wire = serde_json::to_value(value).map_err(io::Error::other)?;
            total = match (total, wire[key].as_u64()) {
                (Some(a), Some(b)) => Some(
                    a.checked_add(b)
                        .ok_or_else(|| invalid("counter sum overflow"))?,
                ),
                _ => None,
            };
        }
        result[key] = json!(total);
    }
    for key in ["worker_cpu_ms", "do_gb_seconds"] {
        let mut total = Some(0.0);
        for value in values {
            let wire = serde_json::to_value(value).map_err(io::Error::other)?;
            total = match (total, wire[key].as_f64()) {
                (Some(a), Some(b)) if b.is_finite() && b >= 0.0 => Some(a + b),
                _ => None,
            };
        }
        result[key] = json!(total);
    }
    counters(&result)
}

fn estimate(usage: &UsageCounters, peak: u64) -> io::Result<cost::MonthlyCostEstimate> {
    cost::estimate_monthly(
        &usage.metered_usage(),
        &MonthlyScenario {
            workload_repetitions: REPEATS,
            storage: StorageProjection::ConstantDailyPeakUpperBound { bytes: peak },
            remaining_account_allowances: AccountAllowances::default(),
            paid_account_baseline_usd: 0.0,
        },
    )
}

fn operation_key(row: &Value) -> io::Result<String> {
    let name = text(row, "workload")?;
    if name == "random_fetch" {
        Ok(format!(
            "random_fetch_{}",
            count(field(row, "probe")?, "index")?
        ))
    } else {
        Ok(name.to_owned())
    }
}
fn fixed(name: &str) -> bool {
    name == "initial_push"
        || name == "incremental_push"
        || name.starts_with("append_push_")
        || name.starts_with("random_fetch_")
}
fn workload_names() -> BTreeSet<String> {
    [
        "initial_push",
        "incremental_push",
        "fresh_clone",
        "warm_clone",
        "incremental_fetch",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain((1..=30).map(|n| format!("append_push_{n}")))
    .chain((0..16).map(|n| format!("random_fetch_{n}")))
    .collect()
}
fn latency_group(key: &str) -> &str {
    if key.starts_with("append_push_") {
        "append_push"
    } else if key.starts_with("random_fetch_") {
        "random_fetch"
    } else {
        key
    }
}
fn add_guard(guards: &mut BTreeMap<String, Vec<f64>>, key: &str, value: f64) {
    guards.entry(key.to_owned()).or_default().push(value);
}

fn profile_cost(report: &Value) -> io::Result<ProfileCost> {
    require(
        report["complete"] == true && count(report, "schema_version")? == 3,
        "requires a complete schema-v3 benchmark",
    )?;
    let config = field(report, "config")?;
    let cf = field(config, "cloudflare_local")?;
    let mib = number(config, "fixture_mib")?;
    require(
        mib == 4.0 || mib == 16.0,
        "fixed fixture must be 4 or 16 MiB",
    )?;
    let view = text(cf, "view_cache")?;
    require(view == "cold" || view == "warm", "unknown view policy")?;
    let samples = count(config, "samples")?;
    require(
        (2..=100).contains(&samples),
        "requires 2..100 measured paired trials",
    )?;
    require(
        count(config, "push_generations_per_run")? == 32
            && count(config, "random_fetches_per_run")? == 16
            && count(config, "warmups_per_backend")? == 1
            && count(config, "protocol")? == 0
            && config["debug_assertions"] == false,
        "fixed workload configuration mismatch",
    )?;
    require(
        count(cf, "monthly_workload_repetitions")? == REPEATS,
        "monthly repetitions must be 1000",
    )?;
    for key in ["cache_model", "cost_trace", "execution_model"] {
        text(cf, key)?;
    }
    for key in [
        "reference",
        "http_post_buffer",
        "reference_push_maintenance",
        "reference_random_fetch_preparation",
        "order",
        "cache_workloads",
        "random_fetch_setup",
        "storage_checks",
    ] {
        field(config, key)?;
    }
    require(
        matches!(
            text(config, "reference_push_maintenance")?,
            "bitmap" | "retained"
        ),
        "invalid reference maintenance",
    )?;
    let fixture = field(report, "fixture")?;
    require(
        array(fixture, "append_tips")?.len() == 30
            && array(fixture, "fetch_probes")?.len() == 16
            && count(fixture, "working_tree_bytes")? == mib as u64 * 1024 * 1024,
        "incomplete fixed fixture",
    )?;
    for key in ["base", "tip"] {
        text(fixture, key)?;
    }
    count(fixture, "files")?;
    count(fixture, "bytes_per_file")?;
    let raw = array(report, "raw")?;
    let expected = workload_names();
    let mut trials = Vec::new();
    let mut guards = BTreeMap::new();
    let mut latencies: BTreeMap<String, (Vec<f64>, Vec<f64>, Vec<f64>)> = BTreeMap::new();
    let mut missing = None;
    let mut epoch = None;
    for sample in -1..samples as i64 {
        let mut backends = BTreeMap::new();
        for backend in ["cloudflare-local", "git"] {
            let mut rows = BTreeMap::new();
            for row in raw
                .iter()
                .filter(|r| r["backend"] == backend && r["sample"] == sample)
            {
                require(
                    row["backend_validation_complete"] == true && row["warmup"] == (sample < 0),
                    "unvalidated trial or invalid warmup flag",
                )?;
                number(row, "wall_ms")?;
                let key = operation_key(row)?;
                if key.starts_with("maintenance_after_") && backend == "git" {
                    continue;
                }
                require(expected.contains(&key), "unexpected benchmark operation")?;
                require(
                    rows.insert(key, row).is_none(),
                    "duplicate benchmark operation",
                )?;
            }
            require(
                rows.keys().cloned().collect::<BTreeSet<_>>() == expected,
                "incomplete paired trial workload",
            )?;
            backends.insert(backend, rows);
        }
        let rows = &backends["cloudflare-local"];
        let first = rows["initial_push"];
        let mut pushes = Vec::new();
        let mut fetches = Vec::new();
        let mut read_bytes = 0_u64;
        let mut http_bytes = 0_u64;
        let mut received_bytes = 0_u64;
        for (key, row) in rows {
            require(
                row["counter_integrity"] == true,
                "counter integrity missing",
            )?;
            let before = snapshot(field(row, "cloudflare_services_before")?)?;
            let after = snapshot(field(row, "cloudflare_services_after")?)?;
            let delta = after.checked_workload_delta(&before)?;
            require(
                counters(field(row, "cloudflare_workload_delta")?)? == delta,
                "recorded delta differs from actual snapshots",
            )?;
            require(
                delta.worker_observer_requests == 0,
                "observer traffic leaked into workload",
            )?;
            let current_epoch = (before.counter_scope.clone(), before.counter_epoch.clone());
            if let Some(old) = &epoch {
                require(
                    *old == current_epoch,
                    "Worker scope/epoch changed during profile",
                )?;
            } else {
                epoch = Some(current_epoch);
            }
            let paired = backends["git"][key];
            if key.starts_with("random_fetch_") {
                let probe = field(row, "probe")?;
                let index = count(probe, "index")? as usize;
                let expected_probe = &fixture["fetch_probes"][index];
                for field_name in ["want", "have", "reference"] {
                    require(
                        probe.get(field_name).is_some()
                            && probe[field_name] == expected_probe[field_name]
                            && paired["probe"][field_name] == expected_probe[field_name],
                        "probe fixture differs",
                    )?;
                }
                let pack = count(row, "client_received_pack_bytes")?;
                require(
                    pack > 0 && count(paired, "client_received_pack_bytes")? > 0,
                    "fetch transferred no pack",
                )?;
                let cache = field(row, "server_cache_after_fetch")?;
                require(
                    count(cache, "budget_bytes")? == RESPONSE_BYTES
                        && count(cache, "entry_limit")? == RESPONSE_ENTRIES
                        && count(cache, "resident_bytes")? <= RESPONSE_BYTES
                        && count(cache, "entries")? <= RESPONSE_ENTRIES,
                    "response cache policy changed or budget exceeded",
                )?;
                require(
                    delta.r2_class_a == 0
                        && delta.r2_deletes == 0
                        && delta.r2_written_bytes == 0
                        && delta.do_rows_written == Some(0),
                    "read workload wrote authoritative storage",
                )?;
                read_bytes = read_bytes
                    .checked_add(delta.r2_read_bytes)
                    .ok_or_else(|| invalid("read bytes overflow"))?;
                http_bytes = http_bytes
                    .checked_add(count(row, "http_response_body_bytes")?)
                    .ok_or_else(|| invalid("HTTP bytes overflow"))?;
                received_bytes = received_bytes
                    .checked_add(pack)
                    .ok_or_else(|| invalid("pack bytes overflow"))?;
                fetches.push(delta);
            } else if fixed(key) {
                pushes.push(delta);
            }
            if sample >= 0 {
                let host_ms = number(row, "wall_ms")?;
                let git_ms = number(paired, "wall_ms")?;
                require(git_ms > 0.0, "zero reference latency")?;
                let entry = latencies.entry(latency_group(key).to_owned()).or_default();
                entry.0.push(host_ms);
                entry.1.push(git_ms);
                entry.2.push(host_ms / git_ms);
            }
        }
        require(
            pushes.len() == 32 && fetches.len() == 16,
            "incorrect fixed mix",
        )?;
        let before = field(
            field(first, "random_fetch_storage_before")?,
            "authoritative_services",
        )?;
        let after = field(
            field(first, "random_fetch_storage_after")?,
            "authoritative_services",
        )?;
        require(
            before == after,
            "read workload changed authoritative inventory",
        )?;
        for key in [
            "logical_bytes",
            "objects",
            "generation",
            "refs",
            "active_manifest_pack_and_index_bytes",
            "active_manifest_pack_count",
        ] {
            count(before, key)?;
        }
        let stats = field(first, "final_object_stats")?;
        let observed = snapshot(field(stats, "cloudflare_services")?)?;
        let objects = field(stats, "objects")?;
        require(
            count(objects, "unique_objects")? > 0
                && count(objects, "unique_objects")? == count(objects, "reachable_objects")?
                && count(objects, "unreachable_objects")? == 0,
            "object inventory is incomplete or unreachable",
        )?;
        require(
            observed.r2_live_bytes == count(after, "logical_bytes")?
                && observed.r2_live_objects == count(after, "objects")?
                && observed.r2_peak_bytes >= observed.r2_live_bytes,
            "final R2 inventory does not reconcile",
        )?;
        require(
            observed.counter_scope == epoch.as_ref().unwrap().0
                && observed.counter_epoch == epoch.as_ref().unwrap().1,
            "final accounting scope changed",
        )?;
        require(
            count(stats, "cache_budget_bytes")? == RESPONSE_BYTES
                && count(stats, "cache_entry_limit")? == RESPONSE_ENTRIES
                && count(stats, "cache_bytes")? <= RESPONSE_BYTES
                && count(stats, "cache_entries")? <= RESPONSE_ENTRIES,
            "final cache policy mismatch",
        )?;
        if view == "cold" {
            require(read_bytes > 0, "cold profile bypassed R2 reads")?;
        }
        if sample < 0 {
            continue;
        }
        let push_usage = sum(&pushes)?;
        let fetch_usage = sum(&fetches)?;
        let fixed_usage = sum(&[push_usage.clone(), fetch_usage.clone()])?;
        let priced = estimate(&fixed_usage, observed.r2_peak_bytes)?;
        let missing_now: Vec<String> = priced
            .unmeasured_billing_dimensions
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        if let Some(old) = &missing {
            require(
                *old == missing_now,
                "measurement availability differs between trials",
            )?;
        } else {
            missing = Some(missing_now);
        }
        let whole = estimate(&observed.workload_counters()?, observed.r2_peak_bytes)?;
        require(
            whole.unmeasured_billing_dimensions == priced.unmeasured_billing_dimensions,
            "whole trace measurement mask differs",
        )?;
        let resource_monthly_usd = [
            ("r2_storage", Some(&priced.r2_storage)),
            ("r2_class_a", Some(&priced.r2_class_a)),
            ("r2_class_b", Some(&priced.r2_class_b)),
            ("worker_requests", Some(&priced.worker_requests)),
            (
                "durable_object_requests",
                Some(&priced.durable_object_requests),
            ),
            ("worker_cpu", priced.worker_cpu.as_ref()),
            (
                "durable_object_duration",
                priced.durable_object_duration.as_ref(),
            ),
            (
                "durable_object_rows_read",
                priced.durable_object_rows_read.as_ref(),
            ),
            (
                "durable_object_rows_written",
                priced.durable_object_rows_written.as_ref(),
            ),
        ]
        .into_iter()
        .filter_map(|(name, charge)| charge.map(|c| (name.to_owned(), c.gross_linear_usd)))
        .collect();
        for (key, value) in [
            ("r2_peak_bytes", observed.r2_peak_bytes),
            ("r2_live_bytes", observed.r2_live_bytes),
            (
                "do_sqlite_bytes",
                count(field(stats, "cloudflare_services")?, "do_sqlite_bytes")?,
            ),
            ("fixed_r2_read_bytes", fixed_usage.r2_read_bytes),
            ("fixed_r2_written_bytes", fixed_usage.r2_written_bytes),
            ("varied_fetch_r2_read_bytes", read_bytes),
            ("varied_fetch_http_response_bytes", http_bytes),
            ("varied_fetch_client_pack_bytes", received_bytes),
        ] {
            add_guard(&mut guards, key, value as f64);
        }
        require(
            http_bytes > 0 && received_bytes > 0,
            "missing response byte measurements",
        )?;
        add_guard(
            &mut guards,
            "varied_fetch_read_amplification",
            read_bytes as f64 / http_bytes as f64,
        );
        trials.push(CostTrial {
            sample,
            fixed_workload: fixed_usage,
            known_monthly_fixed_workload_usd: priced.known_gross_linear_usage_usd,
            known_monthly_whole_trace_usd: whole.known_gross_linear_usage_usd,
            known_usd_per_1000_pushes: estimate(&push_usage, 0)?.known_gross_linear_usage_usd
                / 32.0,
            known_usd_per_1000_varied_fetches: estimate(&fetch_usage, 0)?
                .known_gross_linear_usage_usd
                / 16.0,
            resource_monthly_usd,
        });
    }
    require(
        raw.iter().all(|row| {
            matches!(row["backend"].as_str(), Some("cloudflare-local" | "git"))
                && row["sample"]
                    .as_i64()
                    .is_some_and(|s| (-1..samples as i64).contains(&s))
        }),
        "unexpected backend or sample",
    )?;
    let mut identity_config = config.clone();
    for key in ["host_binary", "host_binary_sha256"] {
        identity_config.as_object_mut().unwrap().remove(key);
    }
    for key in ["endpoint", "repository_prefix", "run_id", "worker_source"] {
        identity_config["cloudflare_local"]
            .as_object_mut()
            .ok_or_else(|| invalid("invalid Cloudflare config"))?
            .remove(key);
    }
    let mut identity_fixture = fixture.clone();
    identity_fixture
        .as_object_mut()
        .ok_or_else(|| invalid("invalid fixture"))?
        .remove("objects");
    let machine = field(report, "machine")?;
    let mut machine_identity = json!({});
    for key in [
        "os",
        "arch",
        "cpu",
        "logical_cpus",
        "kernel",
        "git",
        "rustc",
    ] {
        machine_identity[key] = field(machine, key)?.clone();
    }
    let identity = json!({"config":identity_config,"fixture":identity_fixture,"machine":machine_identity,"response_cache_bytes":RESPONSE_BYTES,"response_cache_entries":RESPONSE_ENTRIES});
    let known_monthly_fixed_workload_usd = Distribution::of(
        trials
            .iter()
            .map(|t| t.known_monthly_fixed_workload_usd)
            .collect(),
    )?;
    let known_monthly_whole_trace_usd = Distribution::of(
        trials
            .iter()
            .map(|t| t.known_monthly_whole_trace_usd)
            .collect(),
    )?;
    let unmeasured_billing_dimensions =
        missing.ok_or_else(|| invalid("no measured cost trials"))?;
    Ok(ProfileCost {
        profile: format!("{}mib-{view}", mib as u64),
        identity,
        source_report_sha256: crate::hash(&serde_json::to_vec(report).map_err(io::Error::other)?),
        host_binary_sha256: text(config, "host_binary_sha256")?.to_owned(),
        worker_source_sha256: text(field(cf, "worker_source")?, "sha256")?.to_owned(),
        trials,
        total_estimate_usd: unmeasured_billing_dimensions
            .is_empty()
            .then_some(known_monthly_fixed_workload_usd.mean),
        known_monthly_fixed_workload_usd,
        known_monthly_whole_trace_usd,
        unmeasured_billing_dimensions,
        guards: guards
            .into_iter()
            .map(|(key, values)| Ok((key, Distribution::of(values)?)))
            .collect::<io::Result<_>>()?,
        latency: latencies
            .into_iter()
            .map(|(key, (host, git, paired))| {
                Ok((
                    key,
                    LatencyGuard {
                        host_ms: Distribution::of(host)?,
                        git_ms: Distribution::of(git)?,
                        paired_host_to_git: Distribution::of(paired)?,
                    },
                ))
            })
            .collect::<io::Result<_>>()?,
    })
}

/// Validate four complete hosting-bench reports and score their fixed workload.
/// Legacy, incomplete, mixed-build and differently configured traces fail closed.
pub fn benchmark_cost(reports: &[Value]) -> io::Result<BenchmarkCost> {
    require(
        reports.len() == 4,
        "requires exactly four 4/16 MiB cold/warm reports",
    )?;
    let mut profiles = BTreeMap::new();
    let mut build = None;
    for report in reports {
        let profile = profile_cost(report)?;
        let current = (
            profile.host_binary_sha256.clone(),
            profile.worker_source_sha256.clone(),
        );
        if let Some(previous) = &build {
            require(*previous == current, "suite mixes host or Worker builds")?;
        } else {
            build = Some(current);
        }
        require(
            profiles.insert(profile.profile.clone(), profile).is_none(),
            "duplicate profile",
        )?;
    }
    require(
        profiles.keys().map(String::as_str).collect::<BTreeSet<_>>()
            == ["4mib-cold", "4mib-warm", "16mib-cold", "16mib-warm"]
                .into_iter()
                .collect(),
        "missing required profile",
    )?;
    let known = profiles
        .values()
        .map(|p| p.known_monthly_fixed_workload_usd.mean)
        .sum();
    Ok(BenchmarkCost { schema_version: 1, pricing_verified_date: cost::PRICING_VERIFIED_DATE.to_owned(), objective: "Equal-weight sum of four profiles; each repeats exactly 32 timed pushes + 16 timed varied fetches 1000 times, retaining observed R2 peak once for 30 days; gross linear known resource USD".into(), monthly_workload_repetitions: REPEATS, conservative_trial_min_usd: profiles.values().map(|p| p.known_monthly_fixed_workload_usd.min).sum(), conservative_trial_max_usd: profiles.values().map(|p| p.known_monthly_fixed_workload_usd.max).sum(), total_estimate_usd: profiles.values().all(|p| p.total_estimate_usd.is_some()).then_some(known), known_monthly_fixed_workload_usd: known, profiles, limitations: vec!["Unknown Worker CPU, DO duration and monthly SQLite storage remain unknown; native codec compute is outside this model.".into(), "Local workerd wall time is a latency guard, never billed compute or the cost objective.".into(), "Whole-trace costs separately retain clone, fixture publication and verification operations; these do not change the fixed 32:16 weights.".into(), "No account allowance, baseline subscription or invoice rounding affects the score; measured current SQLite bytes are only a storage guard.".into()] })
}

/// Compare already validated suites. Latency failures are inconclusive and must
/// be rerun independently; they cannot turn a dollar improvement into acceptance.
pub fn compare_cost(
    baseline: &BenchmarkCost,
    candidate: &BenchmarkCost,
) -> io::Result<CostComparison> {
    require(
        baseline.schema_version == 1
            && candidate.schema_version == 1
            && baseline.pricing_verified_date == candidate.pricing_verified_date
            && baseline.objective == candidate.objective
            && baseline.monthly_workload_repetitions == REPEATS
            && candidate.monthly_workload_repetitions == REPEATS,
        "objective or pricing version differs",
    )?;
    require(
        baseline.profiles.len() == 4 && baseline.profiles.keys().eq(candidate.profiles.keys()),
        "suite profiles differ",
    )?;
    require(
        baseline.known_monthly_fixed_workload_usd > 0.0,
        "zero baseline objective",
    )?;
    let mut reasons = Vec::new();
    let mut latency_failed = false;
    let mut resource_failed = false;
    let mut changed_build = false;
    for (name, old) in &baseline.profiles {
        let new = &candidate.profiles[name];
        changed_build |= old.host_binary_sha256 != new.host_binary_sha256
            || old.worker_source_sha256 != new.worker_source_sha256;
        require(
            old.identity == new.identity
                && old.unmeasured_billing_dimensions == new.unmeasured_billing_dimensions,
            "fixture/configuration/cache/measurement availability differs",
        )?;
        require(
            old.guards.keys().eq(new.guards.keys()) && old.latency.keys().eq(new.latency.keys()),
            "guard measurements differ",
        )?;
        if new.known_monthly_fixed_workload_usd.mean
            > old.known_monthly_fixed_workload_usd.mean * 1.01
        {
            resource_failed = true;
            reasons.push(format!(
                "{name}: known fixed workload cost regressed by more than 1%"
            ));
        }
        for (key, value) in &old.guards {
            if new.guards[key].mean > value.mean * 1.01 + 1e-12 {
                resource_failed = true;
                reasons.push(format!("{name}: {key} regressed by more than 1%"));
            }
        }
        for (workload, old_latency) in &old.latency {
            let new_latency = &new.latency[workload];
            if new_latency.paired_host_to_git.median > old_latency.paired_host_to_git.median * 1.10
                || new_latency.paired_host_to_git.p95 > old_latency.paired_host_to_git.p95 * 1.25
            {
                latency_failed = true;
                reasons.push(format!("{name}/{workload}: paired Git-normalized latency exceeded median +10% or p95 +25%; independent rerun required"));
            }
        }
    }
    let reduction = 1.0
        - candidate.known_monthly_fixed_workload_usd / baseline.known_monthly_fixed_workload_usd;
    let dollar_win = reduction + 1e-12 >= MINIMUM_IMPROVEMENT
        && candidate.conservative_trial_max_usd
            <= baseline.conservative_trial_min_usd * (1.0 - MINIMUM_IMPROVEMENT) + 1e-12;
    if !dollar_win {
        reasons.push("Known dollars did not improve by at least 1% with non-overlapping trial cost envelopes; wall-clock changes do not establish improvement.".into());
    }
    let decision = if latency_failed {
        Decision::Inconclusive
    } else if dollar_win && !resource_failed {
        Decision::Improved
    } else {
        Decision::Rejected
    };
    if decision == Decision::Improved {
        reasons.push("Known resource dollars improved by at least 1% across separated trial envelopes and all guards passed; full hosting cost remains unknown.".into());
    }
    Ok(CostComparison {
        eligible_rejected_candidate_for_plateau: decision == Decision::Rejected
            && !resource_failed
            && changed_build,
        decision,
        known_cost_reduction_fraction: reduction,
        reasons,
        plateau_requires_independent_rejected_candidates: PLATEAU_REJECTED_CANDIDATES,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(requests: u64, puts: u64, gets: u64, reads: u64) -> Value {
        serde_json::to_value(UsageCounters {
            worker_requests: requests,
            worker_service_requests: requests,
            r2_class_a: puts,
            r2_class_b: gets,
            r2_read_bytes: reads,
            do_requests: requests,
            do_rows_read: Some(requests),
            do_rows_written: Some(puts),
            ..UsageCounters::default()
        })
        .unwrap()
    }
    fn service(calls: Value) -> Value {
        let mut result = calls;
        result["r2_live_objects"] = json!(2);
        result["r2_live_bytes"] = json!(1000);
        result["r2_peak_bytes"] = json!(2000);
        result["do_sqlite_bytes"] = json!(20480);
        result["counter_scope"] = json!("test receipts");
        result["counter_epoch"] = json!("worker-1");
        result["accounting_complete"] = json!(true);
        result["accounting_errors"] = json!([]);
        result["by_phase"] = json!({"stats":usage(0, 0, 0, 0)});
        result["operations"] = json!({"manifest_get":{"attempts":1,"successes":1,"failures":0,"read_bytes":0,"written_bytes":0}});
        result
    }
    fn report(mib: u64, cold: bool, puts: u64) -> Value {
        let fixture = json!({"base":"base", "tip":"tip", "append_tips":(0..30).map(|n| format!("tip-{n}")).collect::<Vec<_>>(), "fetch_probes":(0..16).map(|n|json!({"want":format!("want-{n}"),"have":null,"reference":format!("refs/heads/probe-{n}")})).collect::<Vec<_>>(), "working_tree_bytes":mib*1024*1024,"files":mib*16,"bytes_per_file":65536});
        let inventory = json!({"logical_bytes":1000,"objects":2,"generation":33,"refs":17,"active_manifest_pack_and_index_bytes":1000,"active_manifest_pack_count":1});
        let mut raw = Vec::new();
        for sample in -1..2 {
            for backend in ["cloudflare-local", "git"] {
                for key in workload_names() {
                    let fetch = key.starts_with("random_fetch_");
                    let index = key
                        .strip_prefix("random_fetch_")
                        .map(|v| v.parse::<usize>().unwrap());
                    let mut row = json!({"backend":backend,"sample":sample,"warmup":sample<0,"backend_validation_complete":true,"workload":if fetch {"random_fetch"}else{&key},"wall_ms":20.0});
                    if let Some(n) = index {
                        row["probe"] = fixture["fetch_probes"][n].clone();
                        row["probe"]["index"] = json!(n);
                        row["client_received_pack_bytes"] = json!(100);
                    }
                    if backend == "cloudflare-local" {
                        let before = service(usage(0, 0, 0, 0));
                        let delta = usage(
                            if fetch { 2 } else { 5 },
                            if fetch { 0 } else { puts },
                            u64::from(cold),
                            if cold { 100 } else { 0 },
                        );
                        row["cloudflare_services_before"] = before;
                        row["cloudflare_services_after"] = service(delta.clone());
                        row["cloudflare_workload_delta"] = delta;
                        row["counter_integrity"] = json!(true);
                        if fetch {
                            row["server_cache_after_fetch"] = json!({"budget_bytes":RESPONSE_BYTES,"entry_limit":RESPONSE_ENTRIES,"resident_bytes":100,"entries":1});
                            row["http_response_body_bytes"] = json!(110);
                        }
                        if key == "initial_push" {
                            row["random_fetch_storage_before"] =
                                json!({"authoritative_services":inventory});
                            row["random_fetch_storage_after"] =
                                json!({"authoritative_services":inventory});
                            row["final_object_stats"] = json!({"cloudflare_services":service(usage(300, 80, 100, 10000)),"objects":{"unique_objects":10,"reachable_objects":10,"unreachable_objects":0},"cache_budget_bytes":RESPONSE_BYTES,"cache_entry_limit":RESPONSE_ENTRIES,"cache_bytes":100,"cache_entries":1});
                        }
                    }
                    raw.push(row);
                }
            }
        }
        json!({"schema_version":3,"complete":true,"config":{"fixture_mib":mib,"samples":2,"push_generations_per_run":32,"random_fetches_per_run":16,"warmups_per_backend":1,"protocol":0,"debug_assertions":false,"reference":[],"http_post_buffer":268435456,"reference_push_maintenance":"bitmap","reference_random_fetch_preparation":"bitmap","order":"AB BA","cache_workloads":"fixed","random_fetch_setup":"fixed","storage_checks":"fixed","host_binary_sha256":"test-host","cloudflare_local":{"view_cache":if cold{"cold"}else{"warm"},"monthly_workload_repetitions":1000,"cache_model":"declared","cost_trace":"fixed","execution_model":"local-only","worker_source":{"sha256":"test-worker"}}},"machine":{"os":"linux","arch":"x86_64","cpu":"test","logical_cpus":4,"kernel":"test","git":"git test","rustc":"rustc test"},"fixture":fixture,"raw":raw})
    }
    fn suite(puts: u64) -> Vec<Value> {
        [
            report(4, true, puts),
            report(4, false, puts),
            report(16, true, puts),
            report(16, false, puts),
        ]
        .into()
    }

    #[test]
    fn fixed_mix_uses_linear_prices_and_unknown_compute_stays_unknown() {
        let reports = suite(2);
        let score = benchmark_cost(&reports).unwrap();
        let profile = &score.profiles["4mib-cold"];
        assert!(score.total_estimate_usd.is_none());
        assert_eq!(profile.trials[0].fixed_workload.r2_class_a, 64);
        assert_eq!(profile.trials[0].fixed_workload.r2_class_b, 48);
        let direct = estimate(&profile.trials[0].fixed_workload, 2000).unwrap();
        assert_eq!(
            profile.known_monthly_fixed_workload_usd.mean,
            direct.known_gross_linear_usage_usd
        );
        assert_ne!(
            profile.known_monthly_fixed_workload_usd.mean,
            direct.known_rounded_usage_usd
        );
        assert!(
            profile
                .unmeasured_billing_dimensions
                .iter()
                .any(|s| s == "Worker CPU-ms")
        );
    }

    #[test]
    fn fewer_paid_operations_win_but_faster_wall_time_alone_does_not() {
        let baseline = benchmark_cost(&suite(2)).unwrap();
        let cheaper = benchmark_cost(&suite(1)).unwrap();
        assert_eq!(
            compare_cost(&baseline, &cheaper).unwrap().decision,
            Decision::Improved
        );
        let mut faster_reports = suite(2);
        for report in &mut faster_reports {
            for row in report["raw"].as_array_mut().unwrap() {
                if row["backend"] == "cloudflare-local" {
                    row["wall_ms"] = json!(10.0);
                }
            }
        }
        let comparison =
            compare_cost(&baseline, &benchmark_cost(&faster_reports).unwrap()).unwrap();
        assert_eq!(comparison.decision, Decision::Rejected);
        assert!(!comparison.eligible_rejected_candidate_for_plateau);
        let mut independent = benchmark_cost(&faster_reports).unwrap();
        for profile in independent.profiles.values_mut() {
            profile.host_binary_sha256 = "independent-faster-build".into();
        }
        assert!(
            compare_cost(&baseline, &independent)
                .unwrap()
                .eligible_rejected_candidate_for_plateau
        );
    }

    #[test]
    fn missing_data_incomplete_accounting_and_changed_cache_fail_closed() {
        for mutation in [0, 1, 2, 3, 4] {
            let mut reports = suite(2);
            let rows = reports[0]["raw"].as_array_mut().unwrap();
            match mutation {
                0 => {
                    rows[0]["cloudflare_workload_delta"]
                        .as_object_mut()
                        .unwrap()
                        .remove("r2_class_a");
                }
                1 => rows[0]["cloudflare_services_after"]["accounting_complete"] = json!(false),
                2 => {
                    rows.remove(0);
                }
                3 => {
                    let row = rows
                        .iter_mut()
                        .find(|r| {
                            r["backend"] == "cloudflare-local" && r["workload"] == "random_fetch"
                        })
                        .unwrap();
                    row["server_cache_after_fetch"]["budget_bytes"] = json!(RESPONSE_BYTES * 2);
                }
                _ => rows[0]["cloudflare_services_after"]["counter_epoch"] = json!("restarted"),
            }
            assert!(benchmark_cost(&reports).is_err(), "mutation {mutation}");
        }
        assert!(benchmark_cost(&suite(2)[..3]).is_err());
    }

    #[test]
    fn fixture_and_measurement_masks_must_match_and_latency_noise_is_inconclusive() {
        let baseline = benchmark_cost(&suite(2)).unwrap();
        let mut candidate = benchmark_cost(&suite(1)).unwrap();
        candidate.profiles.get_mut("4mib-cold").unwrap().identity["fixture"]["base"] =
            json!("different");
        assert!(compare_cost(&baseline, &candidate).is_err());
        candidate = benchmark_cost(&suite(1)).unwrap();
        candidate
            .profiles
            .get_mut("4mib-cold")
            .unwrap()
            .unmeasured_billing_dimensions
            .clear();
        assert!(compare_cost(&baseline, &candidate).is_err());
        candidate = benchmark_cost(&suite(1)).unwrap();
        candidate
            .profiles
            .get_mut("4mib-cold")
            .unwrap()
            .latency
            .get_mut("random_fetch")
            .unwrap()
            .paired_host_to_git
            .p95 *= 2.0;
        let comparison = compare_cost(&baseline, &candidate).unwrap();
        assert_eq!(comparison.decision, Decision::Inconclusive);
        assert!(!comparison.eligible_rejected_candidate_for_plateau);
    }
}
