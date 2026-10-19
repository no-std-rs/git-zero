//! Explicit Cloudflare Standard/Paid usage scenarios, never inferred CPU time.
//!
//! Rates verified 2026-10-17 against the three official pricing pages below.
//! R2 storage uses decimal GB and the average of 30 daily peak byte counts.
//! Included usage belongs to the account, not independently to each repository.
use crate::accounting::{ServiceUsage, UsageCounters};
use serde::{Deserialize, Serialize};
use std::io;

pub const PRICING_VERIFIED_DATE: &str = "2026-10-17";
pub const R2_PRICING_URL: &str = "https://developers.cloudflare.com/r2/pricing/";
pub const WORKERS_PRICING_URL: &str = "https://developers.cloudflare.com/workers/platform/pricing/";
pub const DURABLE_OBJECTS_PRICING_URL: &str =
    "https://developers.cloudflare.com/durable-objects/platform/pricing/";
const MILLION: f64 = 1_000_000.0;
const GB: f64 = 1_000_000_000.0;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MeteredUsage {
    pub r2_class_a_operations: u64,
    pub r2_class_b_operations: u64,
    pub worker_requests: u64,
    pub durable_object_requests: u64,
    /// None means unmeasured. Client RPC wall time is not Worker CPU time.
    pub worker_cpu_ms: Option<f64>,
    /// Active object duration, accounting for concurrency and hibernation.
    pub durable_object_gb_seconds: Option<f64>,
    /// Native storage call counts are not SQLite row billing measurements.
    pub durable_object_rows_read: Option<u64>,
    pub durable_object_rows_written: Option<u64>,
    pub durable_object_storage_gb_month: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "basis", rename_all = "snake_case")]
pub enum StorageProjection {
    /// Exactly 30 daily peaks; a missing day must not silently become zero.
    ThirtyDailyPeaks { daily_peak_bytes: Vec<u64> },
    /// A declared retention scenario, not an observed 30-day storage bill.
    ConstantDailyPeakUpperBound { bytes: u64 },
}

impl StorageProjection {
    fn gb_month(&self) -> io::Result<f64> {
        match self {
            Self::ThirtyDailyPeaks { daily_peak_bytes } => {
                if daily_peak_bytes.len() != 30 {
                    return Err(invalid(
                        "storage projection requires exactly 30 daily peaks",
                    ));
                }
                // Sum integers before division: per-day floating point sums
                // can cross an exact billing boundary by rounding error.
                let byte_days: u128 = daily_peak_bytes
                    .iter()
                    .map(|bytes| u128::from(*bytes))
                    .sum();
                Ok(byte_days as f64 / (GB * 30.0))
            }
            Self::ConstantDailyPeakUpperBound { bytes } => Ok(*bytes as f64 / GB),
        }
    }
}

/// Remaining account-wide monthly included usage. Zero is the conservative
/// exhausted-allowance scenario. This is not a repository-specific free tier.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AccountAllowances {
    pub r2_storage_gb_month: f64,
    pub r2_class_a_operations: u64,
    pub r2_class_b_operations: u64,
    pub worker_requests: u64,
    pub worker_cpu_ms: f64,
    pub durable_object_requests: u64,
    pub durable_object_gb_seconds: f64,
    pub durable_object_rows_read: u64,
    pub durable_object_rows_written: u64,
    pub durable_object_storage_gb_month: f64,
}

impl AccountAllowances {
    /// Unused R2 Standard free tier plus Workers/DO Paid included usage.
    /// A newly purchased Paid account also needs the separate $5 baseline.
    pub fn full_paid_account_included_usage() -> Self {
        Self {
            r2_storage_gb_month: 10.0,
            r2_class_a_operations: 1_000_000,
            r2_class_b_operations: 10_000_000,
            worker_requests: 10_000_000,
            worker_cpu_ms: 30_000_000.0,
            durable_object_requests: 1_000_000,
            durable_object_gb_seconds: 400_000.0,
            durable_object_rows_read: 25_000_000_000,
            durable_object_rows_written: 50_000_000,
            durable_object_storage_gb_month: 5.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MonthlyScenario {
    /// Repeat measured traffic this many times in one month. Storage is
    /// specified separately; repeated reads do not multiply retained bytes.
    pub workload_repetitions: u64,
    pub storage: StorageProjection,
    pub remaining_account_allowances: AccountAllowances,
    /// 0 for an existing Paid account, 5 for a new Standard/Paid subscription.
    pub paid_account_baseline_usd: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Charge {
    pub unit: &'static str,
    pub measured_or_projected_usage: f64,
    pub remaining_account_allowance: f64,
    pub billable_usage_before_rounding: f64,
    pub billing_unit: f64,
    pub rounded_billable_usage: f64,
    /// Linear resource share with exhausted allowances, before bill rounding.
    pub gross_linear_usd: f64,
    pub rounded_charge_usd: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MonthlyCostEstimate {
    pub pricing_verified_date: &'static str,
    pub currency: &'static str,
    pub pricing_sources: [&'static str; 3],
    pub scenario: MonthlyScenario,
    pub r2_storage: Charge,
    pub r2_class_a: Charge,
    pub r2_class_b: Charge,
    pub worker_requests: Charge,
    pub durable_object_requests: Charge,
    pub worker_cpu: Option<Charge>,
    pub durable_object_duration: Option<Charge>,
    pub durable_object_rows_read: Option<Charge>,
    pub durable_object_rows_written: Option<Charge>,
    pub durable_object_storage: Option<Charge>,
    pub known_gross_linear_usage_usd: f64,
    pub known_rounded_usage_usd: f64,
    /// Complete for the supplied Standard R2 storage/operation scenario only.
    pub r2_gross_linear_usd: f64,
    pub r2_rounded_usage_usd: f64,
    pub paid_account_baseline_usd: f64,
    pub known_subtotal_including_baseline_usd: f64,
    /// None whenever a Cloudflare billing dimension is unmeasured.
    pub total_estimate_usd: Option<f64>,
    pub unmeasured_billing_dimensions: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ServiceCostReport {
    pub topology: &'static str,
    pub counter_scope: String,
    pub counter_epoch: Option<String>,
    pub observed_workload: UsageCounters,
    pub excluded_observer_usage: Option<UsageCounters>,
    pub r2_current_objects: u64,
    pub r2_current_bytes: u64,
    pub r2_observed_peak_bytes: u64,
    pub observed_binding_failures: u64,
    pub exhausted_allowances_existing_paid_account: MonthlyCostEstimate,
    pub unused_allowances_new_paid_account: MonthlyCostEstimate,
    pub assumptions: Vec<&'static str>,
}

/// Convenience report for the actual local Worker gateway + native codec +
/// Worker service bridge topology. It does not pretend each Git RPC produces
/// the same invocation count in a future self-contained Wasm Git host.
pub fn estimate_service_usage(
    observed: &ServiceUsage,
    monthly_runs: u64,
) -> io::Result<ServiceCostReport> {
    let workload = observed.workload_counters()?;
    let metered = workload.metered_usage();
    let peak = observed.r2_peak_bytes.max(observed.r2_live_bytes);
    let mut scenario = MonthlyScenario {
        workload_repetitions: monthly_runs,
        storage: StorageProjection::ConstantDailyPeakUpperBound { bytes: peak },
        remaining_account_allowances: AccountAllowances::default(),
        paid_account_baseline_usd: 0.0,
    };
    let exhausted = estimate_monthly(&metered, &scenario)?;
    scenario.remaining_account_allowances = AccountAllowances::full_paid_account_included_usage();
    scenario.paid_account_baseline_usd = 5.0;
    let unused = estimate_monthly(&metered, &scenario)?;
    let failures = observed
        .operations
        .values()
        .try_fold(0_u64, |total, operation| {
            total
                .checked_add(operation.failures)
                .ok_or_else(|| invalid("binding failure count overflow"))
        })?;
    Ok(ServiceCostReport {
        topology: "local Worker gateway + native Git codec + Worker R2/Durable Object service bridge",
        counter_scope: observed.counter_scope.clone(),
        counter_epoch: observed.counter_epoch.clone(),
        observed_workload: workload,
        excluded_observer_usage: observed.by_phase.get("stats").cloned(),
        r2_current_objects: observed.r2_live_objects,
        r2_current_bytes: observed.r2_live_bytes,
        r2_observed_peak_bytes: peak,
        observed_binding_failures: failures,
        exhausted_allowances_existing_paid_account: exhausted,
        unused_allowances_new_paid_account: unused,
        assumptions: vec![
            "Local workerd runs incur no Cloudflare bill; these are production-rate scenarios, not invoices.",
            "Traffic replays scale all observed non-stats phases, including fixture population and verification when present.",
            "Storage holds the observed R2 peak continuously for 30 days; traffic repetitions do not multiply retained bytes or project future repository growth.",
            "The stats phase is measured observer overhead and is excluded from priced workload traffic.",
            "Request counts price the observed hybrid proxy topology, not a future standalone Worker Git implementation.",
            "R2 counters classify actual binding attempts; failed calls may receive different production billing treatment.",
            "Included usage is account-wide; the two allowance scenarios are alternatives and cannot be added together.",
            "Rounded charges assign whole billing units to this scenario; rounding slack already paid by the account can lower its incremental bill.",
            "Unmeasured Worker CPU, Durable Object active duration, and SQLite usage remain unknown; client elapsed time is never substituted.",
            "Native Git codec compute, taxes, and other Cloudflare products are outside this estimate.",
        ],
    })
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn nonnegative(value: f64) -> io::Result<f64> {
    if !value.is_finite() || value < 0.0 {
        Err(invalid(
            "usage, allowances and baseline must be finite and nonnegative",
        ))
    } else {
        Ok(value)
    }
}

fn charge(
    unit: &'static str,
    usage: f64,
    allowance: f64,
    billing_unit: f64,
    usd_per_unit: f64,
) -> io::Result<Charge> {
    nonnegative(usage)?;
    nonnegative(allowance)?;
    let billable = (usage - allowance).max(0.0);
    let rounded = if billing_unit == 0.0 {
        billable
    } else {
        (billable / billing_unit).ceil() * billing_unit
    };
    Ok(Charge {
        unit,
        measured_or_projected_usage: usage,
        remaining_account_allowance: allowance,
        billable_usage_before_rounding: billable,
        billing_unit,
        rounded_billable_usage: rounded,
        gross_linear_usd: usage * usd_per_unit,
        rounded_charge_usd: rounded * usd_per_unit,
    })
}

/// Estimate Standard R2 + Workers Paid + SQLite Durable Objects. This excludes
/// taxes, other Cloudflare products, and the external native Git codec's cost.
/// Delete operations and Standard R2 egress/retrieval have no usage charge.
pub fn estimate_monthly(
    observed: &MeteredUsage,
    scenario: &MonthlyScenario,
) -> io::Result<MonthlyCostEstimate> {
    nonnegative(scenario.paid_account_baseline_usd)?;
    let repetitions = scenario.workload_repetitions;
    let multiply = |count: u64| {
        count
            .checked_mul(repetitions)
            .map(|count| count as f64)
            .ok_or_else(|| invalid("projected operation count overflow"))
    };
    let optional = |quantity: Option<f64>, allowance, unit, billing_unit, rate| {
        quantity
            .map(|quantity| {
                charge(
                    unit,
                    nonnegative(quantity)? * repetitions as f64,
                    allowance,
                    billing_unit,
                    rate,
                )
            })
            .transpose()
    };
    let allowance = &scenario.remaining_account_allowances;
    let r2_storage = charge(
        "GB-month",
        scenario.storage.gb_month()?,
        allowance.r2_storage_gb_month,
        1.0,
        0.015,
    )?;
    let r2_class_a = charge(
        "operations",
        multiply(observed.r2_class_a_operations)?,
        allowance.r2_class_a_operations as f64,
        MILLION,
        4.50 / MILLION,
    )?;
    let r2_class_b = charge(
        "operations",
        multiply(observed.r2_class_b_operations)?,
        allowance.r2_class_b_operations as f64,
        MILLION,
        0.36 / MILLION,
    )?;
    // Workers pricing examples prorate requests and CPU; R2 and DO explicitly
    // round up to their billing units after subtracting included usage.
    let worker_requests = charge(
        "requests",
        multiply(observed.worker_requests)?,
        allowance.worker_requests as f64,
        0.0,
        0.30 / MILLION,
    )?;
    let durable_object_requests = charge(
        "requests",
        multiply(observed.durable_object_requests)?,
        allowance.durable_object_requests as f64,
        MILLION,
        0.15 / MILLION,
    )?;
    let worker_cpu = optional(
        observed.worker_cpu_ms,
        allowance.worker_cpu_ms,
        "CPU-ms",
        0.0,
        0.02 / MILLION,
    )?;
    let durable_object_duration = optional(
        observed.durable_object_gb_seconds,
        allowance.durable_object_gb_seconds,
        "GB-seconds",
        MILLION,
        12.50 / MILLION,
    )?;
    let durable_object_rows_read = optional(
        observed.durable_object_rows_read.map(|rows| rows as f64),
        allowance.durable_object_rows_read as f64,
        "rows",
        MILLION,
        0.001 / MILLION,
    )?;
    let durable_object_rows_written = optional(
        observed.durable_object_rows_written.map(|rows| rows as f64),
        allowance.durable_object_rows_written as f64,
        "rows",
        MILLION,
        1.0 / MILLION,
    )?;
    // Stored SQL bytes, like R2 storage, are a separately supplied monthly
    // retention observation, never multiplied by traffic repetitions.
    let durable_object_storage = observed
        .durable_object_storage_gb_month
        .map(|usage| {
            charge(
                "GB-month",
                usage,
                allowance.durable_object_storage_gb_month,
                1.0,
                0.20,
            )
        })
        .transpose()?;
    let mut known_gross_linear_usage_usd = 0.0;
    let mut known_rounded_usage_usd = 0.0;
    for line in [
        Some(&r2_storage),
        Some(&r2_class_a),
        Some(&r2_class_b),
        Some(&worker_requests),
        Some(&durable_object_requests),
        worker_cpu.as_ref(),
        durable_object_duration.as_ref(),
        durable_object_rows_read.as_ref(),
        durable_object_rows_written.as_ref(),
        durable_object_storage.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        known_gross_linear_usage_usd += line.gross_linear_usd;
        known_rounded_usage_usd += line.rounded_charge_usd;
    }
    let unmeasured_billing_dimensions = [
        (worker_cpu.is_none(), "Worker CPU-ms"),
        (
            durable_object_duration.is_none(),
            "Durable Object GB-seconds",
        ),
        (
            durable_object_rows_read.is_none(),
            "Durable Object SQLite rows read",
        ),
        (
            durable_object_rows_written.is_none(),
            "Durable Object SQLite rows written",
        ),
        (
            durable_object_storage.is_none(),
            "Durable Object SQLite GB-month",
        ),
    ]
    .into_iter()
    .filter_map(|(missing, name)| missing.then_some(name))
    .collect::<Vec<_>>();
    let known_subtotal_including_baseline_usd =
        known_rounded_usage_usd + scenario.paid_account_baseline_usd;
    let r2_gross_linear_usd =
        r2_storage.gross_linear_usd + r2_class_a.gross_linear_usd + r2_class_b.gross_linear_usd;
    let r2_rounded_usage_usd = r2_storage.rounded_charge_usd
        + r2_class_a.rounded_charge_usd
        + r2_class_b.rounded_charge_usd;
    Ok(MonthlyCostEstimate {
        pricing_verified_date: PRICING_VERIFIED_DATE,
        currency: "USD",
        pricing_sources: [
            R2_PRICING_URL,
            WORKERS_PRICING_URL,
            DURABLE_OBJECTS_PRICING_URL,
        ],
        scenario: scenario.clone(),
        r2_storage,
        r2_class_a,
        r2_class_b,
        worker_requests,
        durable_object_requests,
        worker_cpu,
        durable_object_duration,
        durable_object_rows_read,
        durable_object_rows_written,
        durable_object_storage,
        known_gross_linear_usage_usd,
        known_rounded_usage_usd,
        r2_gross_linear_usd,
        r2_rounded_usage_usd,
        paid_account_baseline_usd: scenario.paid_account_baseline_usd,
        known_subtotal_including_baseline_usd,
        total_estimate_usd: unmeasured_billing_dimensions
            .is_empty()
            .then_some(known_subtotal_including_baseline_usd),
        unmeasured_billing_dimensions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenario() -> MonthlyScenario {
        MonthlyScenario {
            workload_repetitions: 1,
            storage: StorageProjection::ConstantDailyPeakUpperBound { bytes: 0 },
            remaining_account_allowances: AccountAllowances::default(),
            paid_account_baseline_usd: 0.0,
        }
    }

    #[test]
    fn billing_rounds_after_remaining_account_allowances() {
        let usage = MeteredUsage {
            r2_class_a_operations: 1_000_001,
            r2_class_b_operations: 10_000_001,
            durable_object_requests: 1_500_000,
            ..MeteredUsage::default()
        };
        let mut projected = scenario();
        projected.remaining_account_allowances =
            AccountAllowances::full_paid_account_included_usage();
        projected.storage = StorageProjection::ConstantDailyPeakUpperBound {
            bytes: 10_100_000_000,
        };
        let estimate = estimate_monthly(&usage, &projected).unwrap();
        assert_eq!(estimate.r2_storage.rounded_billable_usage, 1.0);
        assert_eq!(estimate.r2_storage.rounded_charge_usd, 0.015);
        assert_eq!(estimate.r2_class_a.rounded_charge_usd, 4.5);
        assert_eq!(estimate.r2_class_b.rounded_charge_usd, 0.36);
        assert_eq!(estimate.durable_object_requests.rounded_charge_usd, 0.15);
    }

    #[test]
    fn peaks_and_traffic_are_separate_and_unknown_compute_stays_unknown() {
        let usage = MeteredUsage {
            r2_class_b_operations: 2,
            ..MeteredUsage::default()
        };
        let mut projected = scenario();
        projected.workload_repetitions = 100;
        projected.storage = StorageProjection::ThirtyDailyPeaks {
            daily_peak_bytes: [vec![1_000_000_000; 5], vec![3_000_000_000; 25]].concat(),
        };
        let estimate = estimate_monthly(&usage, &projected).unwrap();
        assert!((estimate.r2_storage.measured_or_projected_usage - 8.0 / 3.0).abs() < 1e-12);
        assert_eq!(estimate.r2_class_b.measured_or_projected_usage, 200.0);
        assert!(estimate.worker_cpu.is_none());
        assert!(estimate.durable_object_duration.is_none());
        assert!(estimate.total_estimate_usd.is_none());
        assert_eq!(estimate.unmeasured_billing_dimensions.len(), 5);
    }

    #[test]
    fn zero_usage_costs_nothing_and_unknown_is_not_free() {
        let estimate = estimate_monthly(&MeteredUsage::default(), &scenario()).unwrap();
        assert_eq!(estimate.known_rounded_usage_usd, 0.0);
        assert!(estimate.total_estimate_usd.is_none());
        assert_eq!(estimate.r2_class_a.rounded_billable_usage, 0.0);
    }

    #[test]
    fn invalid_periods_overflow_and_unphysical_measurements_are_rejected() {
        let usage = MeteredUsage::default();
        let mut projected = scenario();
        projected.storage = StorageProjection::ThirtyDailyPeaks {
            daily_peak_bytes: vec![0; 29],
        };
        assert!(estimate_monthly(&usage, &projected).is_err());
        projected = scenario();
        projected.workload_repetitions = 2;
        assert!(
            estimate_monthly(
                &MeteredUsage {
                    worker_requests: u64::MAX,
                    ..usage.clone()
                },
                &projected
            )
            .is_err()
        );
        assert!(
            estimate_monthly(
                &MeteredUsage {
                    worker_cpu_ms: Some(f64::NAN),
                    ..usage
                },
                &scenario()
            )
            .is_err()
        );
    }

    #[test]
    fn service_report_excludes_observers_and_keeps_free_tier_account_wide() {
        let observed = ServiceUsage {
            calls: UsageCounters {
                worker_requests: 7,
                worker_service_requests: 5,
                worker_observer_requests: 2,
                r2_class_a: 2,
                r2_class_b: 5,
                ..UsageCounters::default()
            },
            by_phase: std::collections::BTreeMap::from([(
                "stats".into(),
                UsageCounters {
                    worker_requests: 2,
                    worker_observer_requests: 2,
                    r2_class_a: 1,
                    r2_class_b: 2,
                    ..UsageCounters::default()
                },
            )]),
            r2_live_bytes: 1024,
            r2_peak_bytes: 2048,
            ..ServiceUsage::default()
        };
        let report = estimate_service_usage(&observed, 1000).unwrap();
        assert_eq!(report.observed_workload.worker_requests, 5);
        assert_eq!(report.observed_workload.worker_observer_requests, 0);
        let exhausted = report.exhausted_allowances_existing_paid_account;
        assert_eq!(exhausted.r2_class_a.measured_or_projected_usage, 1000.0);
        assert_eq!(exhausted.r2_class_b.measured_or_projected_usage, 3000.0);
        assert_eq!(
            exhausted.r2_storage.measured_or_projected_usage,
            2048.0 / GB
        );
        assert!(exhausted.r2_rounded_usage_usd > exhausted.r2_gross_linear_usd);
        assert_eq!(
            report
                .unused_allowances_new_paid_account
                .r2_rounded_usage_usd,
            0.0
        );
        assert_eq!(
            report
                .unused_allowances_new_paid_account
                .paid_account_baseline_usd,
            5.0
        );
        assert!(
            report
                .unused_allowances_new_paid_account
                .total_estimate_usd
                .is_none()
        );
        let incomplete = ServiceUsage {
            accounting_complete: Some(false),
            ..observed
        };
        assert!(estimate_service_usage(&incomplete, 1000).is_err());
    }
}
