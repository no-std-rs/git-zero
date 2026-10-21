//! One request's canonical output, reconstruction work and temporary bodies.
use super::{Frame, Shared, code, output};
use core::cell::UnsafeCell;
use git_zero::Error;

struct Budget {
    limits: [u64; 3],
    remaining: [u64; 3],
    failed: bool,
}

impl Budget {
    const fn new(limits: [u64; 3]) -> Self {
        Self {
            limits,
            remaining: limits,
            failed: false,
        }
    }
}

// Defaults also protect an embedding that does not select custom limits.
static BUDGET: Shared<Budget> = Shared(UnsafeCell::new(Budget::new([1 << 40, 16 << 40, 64 << 30])));

#[unsafe(no_mangle)]
pub extern "C" fn host_budget_init(canonical: u64, work: u64, temporary: u64) -> i32 {
    // SAFETY: This exclusive synchronous call starts one fresh HTTP operation.
    unsafe {
        *BUDGET.0.get() = Budget::new([canonical, work, temporary]);
    }
    0
}

/// Kinds: 0 canonical bytes, 1 representation/decoding work, 2 temporary bytes.
/// Reserve complete bodies before writing them. Exhaustion is terminal until
/// the next request, even if a caller retries a smaller operation.
#[unsafe(no_mangle)]
pub extern "C" fn host_budget_charge(kind: u32, bytes: u64) -> i32 {
    // SAFETY: The request owns its Wasm instance and never reenters this ABI.
    let budget = unsafe { &mut *BUDGET.0.get() };
    if budget.failed {
        return code(Error::LimitExceeded);
    }
    let Some(remaining) = budget.remaining.get_mut(kind as usize) else {
        return code(Error::InvalidRequest);
    };
    match remaining.checked_sub(bytes) {
        Some(next) => {
            *remaining = next;
            0
        }
        None => {
            budget.failed = true;
            code(Error::LimitExceeded)
        }
    }
}

/// Tag 78, exhausted flag@12, canonical/work/temporary usage@16/24/32.
#[unsafe(no_mangle)]
pub extern "C" fn host_budget_usage() -> i32 {
    // SAFETY: Read-only during this exclusive call; OUTPUT is a disjoint arena.
    let budget = unsafe { &*BUDGET.0.get() };
    let mut frame = Frame::new(output());
    frame.word(12, u32::from(budget.failed));
    for (index, (&limit, &remaining)) in budget.limits.iter().zip(&budget.remaining).enumerate() {
        frame.wide(16 + index * 8, limit - remaining);
    }
    frame.finish(78, 0, 0)
}
