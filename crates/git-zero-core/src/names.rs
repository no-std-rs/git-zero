use crate::{Error, Result};
use core::cmp::Ordering;

pub(crate) fn cmp_tree_names(
    left: &[u8],
    left_tree: bool,
    right: &[u8],
    right_tree: bool,
) -> Ordering {
    let terminator = |tree| if tree { b'/' } else { 0 };
    left.iter()
        .copied()
        .chain([terminator(left_tree)])
        .cmp(right.iter().copied().chain([terminator(right_tree)]))
}

pub(crate) fn validate_name(name: &[u8]) -> Result<()> {
    if name.is_empty()
        || matches!(name, b"." | b"..")
        || name.eq_ignore_ascii_case(b".git")
        || name.iter().any(|byte| matches!(byte, 0 | b'/' | b'\\'))
    {
        return Err(Error::InvalidFileName);
    }
    Ok(())
}
