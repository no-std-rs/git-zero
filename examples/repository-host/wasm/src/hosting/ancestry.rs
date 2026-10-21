//! External-catalog ancestry policy; the host supplies paged parent edges.
use super::{Shared, code, format, id_at, input, kind};
use core::cell::UnsafeCell;
use git_zero::{Error, Result, http::receive::Ancestry};

static ANCESTRY: Shared<Option<Ancestry>> = Shared(UnsafeCell::new(None));

fn session() -> Result<&'static mut Ancestry> {
    // SAFETY: Synchronous exclusive ABI call; no borrow escapes to the host.
    unsafe { (&mut *ANCESTRY.0.get()).as_mut() }.ok_or(Error::InvalidRequest)
}

#[unsafe(no_mangle)]
pub extern "C" fn host_ancestry_init(object_format: u32, visits: u64) -> i32 {
    let value = format(object_format).map(|format| {
        // SAFETY: The exclusive call replaces state without retained borrows.
        unsafe {
            *ANCESTRY.0.get() = Some(Ancestry::new(format, visits));
        }
    });
    value.map_or_else(code, |()| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn host_ancestry_start(length: usize, object_format: u32) -> i32 {
    let value = (|| {
        let format = format(object_format)?;
        if length != format.id_length() {
            return Err(Error::InvalidRequest);
        }
        session()?.start(id_at(input(length)?, 0, format)?)
    })();
    value.map_or_else(code, |()| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn host_ancestry_visit(length: usize, object_format: u32, object_kind: u32) -> i32 {
    let value = (|| {
        let format = format(object_format)?;
        if length != format.id_length() {
            return Err(Error::InvalidRequest);
        }
        session()?.visit(id_at(input(length)?, 0, format)?, kind(object_kind)?)
    })();
    value.map_or_else(code, i32::from)
}
