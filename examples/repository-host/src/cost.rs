//! Measured service usage. Dollar estimates are introduced with the host.
use serde::{Deserialize, Serialize};

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
