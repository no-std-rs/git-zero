//! Git-compatible historical committer timestamps.
// Git's tolerant historical committer-date extraction is intentionally separate
// from wire-number parsing: decimal prefixes and saturated overflow are accepted.
pub(super) fn commit_date(line: &[u8]) -> u64 {
    let Some(end) = line.iter().rposition(|&byte| byte == b'>') else {
        return 0;
    };
    let digits = line[end + 1..].trim_ascii_start();
    let negative = digits.first() == Some(&b'-');
    let digits = digits.strip_prefix(b"-").unwrap_or(digits);
    let value = digits
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .try_fold(0_u64, |value, &byte| {
            value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))
        });
    match value {
        Some(value) if negative => value.wrapping_neg(),
        Some(value) => value,
        None => u64::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn historical_dates_match_git_tolerant_decimal_prefixes() {
        for (date, expected) in [
            ("1700000200 +0000", 1700000200),
            ("1700000200suffix +0000", 1700000200),
            ("1700000200+0100", 1700000200),
            ("-1 +0000", u64::MAX),
            ("-2 +0000", u64::MAX - 1),
            ("+1700000200 +0000", 0),
            ("0x6553f1c8 +0000", 0),
            ("18446744073709551615 +0000", u64::MAX),
            ("18446744073709551616 +0000", u64::MAX),
            ("-18446744073709551616 +0000", u64::MAX),
            ("not-a-date", 0),
            ("  1700000200 +0000", 1700000200),
            ("\t1700000200 +0000", 1700000200),
            ("- +0000", 0),
            ("01700000200 +0000", 1700000200),
        ] {
            let mut line = [0; 256];
            line[0] = b'>';
            line[1..1 + date.len()].copy_from_slice(date.as_bytes());
            assert_eq!(commit_date(&line[..1 + date.len()]), expected, "{date:?}");
        }
    }
}
