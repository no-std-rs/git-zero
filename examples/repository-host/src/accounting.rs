//! Actual service-binding counters from the local Cloudflare bridge.
//! Counter lifetime is the reported Worker/DO instance, not a billing month.
use crate::cost::MeteredUsage;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OperationUsage {
    pub attempts: u64,
    pub successes: u64,
    pub failures: u64,
    pub read_bytes: u64,
    pub written_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UsageCounters {
    pub worker_requests: u64,
    pub worker_gateway_requests: u64,
    pub worker_service_requests: u64,
    pub worker_observer_requests: u64,
    pub r2_class_a: u64,
    pub r2_class_b: u64,
    pub r2_deletes: u64,
    pub r2_read_bytes: u64,
    pub r2_written_bytes: u64,
    pub do_requests: u64,
    pub do_rows_read: Option<u64>,
    pub do_rows_written: Option<u64>,
    pub worker_cpu_ms: Option<f64>,
    pub do_gb_seconds: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ServiceUsage {
    #[serde(flatten)]
    pub calls: UsageCounters,
    /// Inventory gauges refer to real namespaced R2 keys, not Git object IDs.
    pub r2_live_objects: u64,
    pub r2_live_bytes: u64,
    pub r2_peak_bytes: u64,
    pub by_phase: BTreeMap<String, UsageCounters>,
    pub operations: BTreeMap<String, OperationUsage>,
    pub counter_scope: String,
    /// Optional identity makes runtime restart detection explicit when offered.
    pub counter_epoch: Option<String>,
    /// A live DO can evict independently of the aggregation scope. This identity
    /// may change without resetting complete, receipt-backed aggregate usage.
    pub do_instance_epoch: Option<String>,
    /// Missing means a legacy snapshot. An explicitly incomplete receipt trace
    /// must never be interpreted as measured zero usage or a complete cost.
    pub accounting_complete: Option<bool>,
}

impl UsageCounters {
    /// Counter resets are errors, never silently interpreted as zero traffic.
    pub fn checked_sub(&self, earlier: &Self) -> io::Result<Self> {
        let subtract = |left: u64, right: u64| {
            left.checked_sub(right).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "service counters reset or scope changed",
                )
            })
        };
        let optional_count = |left: Option<u64>, right: Option<u64>| match (left, right) {
            (Some(left), Some(right)) => subtract(left, right).map(Some),
            _ => Ok(None),
        };
        let optional_measurement = |left: Option<f64>, right: Option<f64>| match (left, right) {
            (Some(left), Some(right))
                if left.is_finite() && right.is_finite() && right >= 0.0 && left >= right =>
            {
                Ok(Some(left - right))
            }
            (Some(_), Some(_)) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "measured service duration is invalid or reset",
            )),
            _ => Ok(None),
        };
        Ok(Self {
            worker_requests: subtract(self.worker_requests, earlier.worker_requests)?,
            worker_gateway_requests: subtract(
                self.worker_gateway_requests,
                earlier.worker_gateway_requests,
            )?,
            worker_service_requests: subtract(
                self.worker_service_requests,
                earlier.worker_service_requests,
            )?,
            worker_observer_requests: subtract(
                self.worker_observer_requests,
                earlier.worker_observer_requests,
            )?,
            r2_class_a: subtract(self.r2_class_a, earlier.r2_class_a)?,
            r2_class_b: subtract(self.r2_class_b, earlier.r2_class_b)?,
            r2_deletes: subtract(self.r2_deletes, earlier.r2_deletes)?,
            r2_read_bytes: subtract(self.r2_read_bytes, earlier.r2_read_bytes)?,
            r2_written_bytes: subtract(self.r2_written_bytes, earlier.r2_written_bytes)?,
            do_requests: subtract(self.do_requests, earlier.do_requests)?,
            do_rows_read: optional_count(self.do_rows_read, earlier.do_rows_read)?,
            do_rows_written: optional_count(self.do_rows_written, earlier.do_rows_written)?,
            worker_cpu_ms: optional_measurement(self.worker_cpu_ms, earlier.worker_cpu_ms)?,
            do_gb_seconds: optional_measurement(self.do_gb_seconds, earlier.do_gb_seconds)?,
        })
    }

    pub fn metered_usage(&self) -> MeteredUsage {
        MeteredUsage {
            r2_class_a_operations: self.r2_class_a,
            r2_class_b_operations: self.r2_class_b,
            worker_requests: self.worker_requests,
            durable_object_requests: self.do_requests,
            worker_cpu_ms: self.worker_cpu_ms,
            durable_object_gb_seconds: self.do_gb_seconds,
            durable_object_rows_read: self.do_rows_read,
            durable_object_rows_written: self.do_rows_written,
            // An SQL row's encoded value size does not measure billed storage.
            durable_object_storage_gb_month: None,
        }
    }
}

impl ServiceUsage {
    /// Actual hybrid topology traffic, excluding measured observer work.
    /// Warm native-view fetches may perform zero R2 reads; none are invented.
    pub fn workload_counters(&self) -> io::Result<UsageCounters> {
        if self.accounting_complete == Some(false) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "service accounting is incomplete",
            ));
        }
        match self.by_phase.get("stats") {
            Some(observer) => self.calls.checked_sub(observer),
            None => Ok(self.calls.clone()),
        }
    }

    pub fn workload_metered_usage(&self) -> io::Result<MeteredUsage> {
        Ok(self.workload_counters()?.metered_usage())
    }

    /// Keep inventory gauges separate from operation deltas. A runtime identity
    /// change invalidates a trace even when new counters happen to be larger.
    pub fn checked_workload_delta(&self, earlier: &Self) -> io::Result<UsageCounters> {
        if self.counter_scope != earlier.counter_scope
            || self.counter_epoch != earlier.counter_epoch
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "service counter scope changed",
            ));
        }
        if self.by_phase.contains_key("stats") != earlier.by_phase.contains_key("stats") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "service observer accounting availability changed",
            ));
        }
        self.workload_counters()?
            .checked_sub(&earlier.workload_counters()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_snapshot_keeps_physical_inventory_and_observer_work_separate() {
        let usage: ServiceUsage = serde_json::from_value(serde_json::json!({
            "worker_requests": 12, "worker_service_requests": 10, "worker_observer_requests": 2,
            "r2_class_a": 5, "r2_class_b": 7, "r2_deletes": 3,
            "r2_live_objects": 2, "r2_live_bytes": 512, "r2_peak_bytes": 1024,
            "do_requests": 6, "worker_cpu_ms": null, "do_gb_seconds": null,
            "by_phase": {"stats": {"worker_requests": 2, "worker_observer_requests": 2,
                "r2_class_a": 1, "r2_class_b": 2, "do_requests": 1}},
            "operations": {"r2_list": {"attempts": 1, "successes": 1,
                "failures": 0, "read_bytes": 0, "written_bytes": 0}},
            "counter_scope": "current Worker runtime / DO instance", "future_field": true
        }))
        .unwrap();
        assert_eq!(usage.r2_live_bytes, 512);
        assert_eq!(usage.operations["r2_list"].attempts, 1);
        let workload = usage.workload_metered_usage().unwrap();
        assert_eq!(workload.worker_requests, 10);
        assert_eq!(workload.r2_class_a_operations, 4);
        assert_eq!(workload.r2_class_b_operations, 5);
        assert_eq!(workload.durable_object_requests, 5);
        assert!(workload.worker_cpu_ms.is_none());
        assert!(workload.durable_object_gb_seconds.is_none());
    }

    #[test]
    fn a_runtime_reset_cannot_be_mistaken_for_negative_usage_or_free_work() {
        let before = ServiceUsage {
            calls: UsageCounters {
                r2_class_b: 9,
                ..UsageCounters::default()
            },
            counter_epoch: Some("first".into()),
            ..ServiceUsage::default()
        };
        let after = ServiceUsage {
            calls: UsageCounters {
                r2_class_b: 10,
                ..UsageCounters::default()
            },
            counter_epoch: Some("restarted".into()),
            ..ServiceUsage::default()
        };
        assert!(after.checked_workload_delta(&before).is_err());
        let identity_lost = ServiceUsage {
            counter_epoch: None,
            ..after
        };
        assert!(identity_lost.checked_workload_delta(&before).is_err());
        assert!(UsageCounters::default().checked_sub(&before.calls).is_err());
    }

    #[test]
    fn workload_deltas_require_matching_observer_phase_availability() {
        let mut before = ServiceUsage {
            calls: UsageCounters {
                worker_requests: 100,
                r2_class_b: 100,
                ..UsageCounters::default()
            },
            counter_scope: "worker ledger".into(),
            counter_epoch: Some("same-runtime".into()),
            ..ServiceUsage::default()
        };
        let mut after = ServiceUsage {
            calls: UsageCounters {
                worker_requests: 130,
                r2_class_b: 130,
                ..UsageCounters::default()
            },
            ..before.clone()
        };
        let legacy_delta = after.checked_workload_delta(&before).unwrap();
        assert_eq!(legacy_delta.worker_requests, 30);
        assert_eq!(legacy_delta.r2_class_b, 30);

        after.by_phase.insert(
            "stats".into(),
            UsageCounters {
                worker_requests: 20,
                r2_class_b: 20,
                ..UsageCounters::default()
            },
        );
        // All counters still increase, so subtraction alone cannot detect that
        // only the later snapshot excludes cumulative observer traffic.
        assert_eq!(
            after.checked_workload_delta(&before).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        before.by_phase.insert(
            "stats".into(),
            UsageCounters {
                worker_requests: 10,
                r2_class_b: 10,
                ..UsageCounters::default()
            },
        );
        let measured_delta = after.checked_workload_delta(&before).unwrap();
        assert_eq!(measured_delta.worker_requests, 20);
        assert_eq!(measured_delta.r2_class_b, 20);

        after.by_phase.clear();
        assert_eq!(
            after.checked_workload_delta(&before).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn incomplete_receipts_are_rejected_but_complete_do_evictions_are_supported() {
        let before = ServiceUsage {
            calls: UsageCounters {
                r2_class_b: 9,
                ..UsageCounters::default()
            },
            counter_epoch: Some("worker-ledger".into()),
            do_instance_epoch: Some("first-do".into()),
            accounting_complete: Some(true),
            ..ServiceUsage::default()
        };
        let mut after = ServiceUsage {
            calls: UsageCounters {
                r2_class_b: 10,
                ..UsageCounters::default()
            },
            do_instance_epoch: Some("recreated-do".into()),
            ..before.clone()
        };
        assert_eq!(after.checked_workload_delta(&before).unwrap().r2_class_b, 1);
        after.accounting_complete = Some(false);
        assert!(after.workload_counters().is_err());
        assert!(after.checked_workload_delta(&before).is_err());
        assert!(before.checked_workload_delta(&after).is_err());
        // Existing trace files lack this field and remain readable.
        assert!(ServiceUsage::default().workload_counters().is_ok());
    }
}
