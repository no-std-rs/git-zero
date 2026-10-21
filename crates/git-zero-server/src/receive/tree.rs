//! Strict receive tree policy, including platform aliases of special files.
use git_zero_core::{Error, Result};

/// Additional policies to apply to the designated blob after resolving its ID.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BlobChecks(u8);
impl BlobChecks {
    /// Bit 0 selects gitmodules config checks, bit 1 attributes size/line checks.
    pub const fn bits(self) -> u8 {
        self.0
    }
}

/// Add strict receive tree policy after structural link validation. Historical regular-file
/// permissions and attributes symlinks remain warnings. Returned checks apply
/// even when the target blob was published by an earlier request.
pub fn check_tree_entry(name: &[u8], mode: u32, zero_padded: bool) -> Result<BlobChecks> {
    if name.len() > 4096 {
        return Err(Error::InvalidFileName);
    }
    if zero_padded {
        return Err(Error::InvalidObject);
    }
    let symlink = mode & 0o170000 == 0o120000;
    let blob = mode & 0o170000 == 0o100000;
    let mut modules = hfs(name, b"gitmodules");
    if hfs(name, b"git") {
        return Err(Error::InvalidFileName);
    }
    // Windows path components after a backslash are checked by native fsck too.
    let mut part = name;
    loop {
        if ntfs_git(part) {
            return Err(Error::InvalidFileName);
        }
        modules |= ntfs_special(part, b"gitmodules", b"gi7eba");
        match part.iter().position(|&b| b == b'\\') {
            Some(end) => part = &part[end + 1..],
            None => break,
        }
    }
    let attributes = !symlink
        && (hfs(name, b"gitattributes") || ntfs_special(name, b"gitattributes", b"gi7d29"));
    if (modules || attributes) && !blob {
        return Err(Error::InvalidObject);
    }
    Ok(BlobChecks(u8::from(modules) | (u8::from(attributes) << 1)))
}

fn ntfs_git(name: &[u8]) -> bool {
    let rest = if name
        .get(..4)
        .is_some_and(|s| s.eq_ignore_ascii_case(b".git"))
    {
        &name[4..]
    } else if name
        .get(..5)
        .is_some_and(|s| s.eq_ignore_ascii_case(b"git~1"))
    {
        &name[5..]
    } else {
        return false;
    };
    for &byte in rest {
        if matches!(byte, b'/' | b'\\' | b':') {
            return true;
        }
        if byte != b'.' && byte != b' ' {
            return false;
        }
    }
    true
}

fn ntfs_special(name: &[u8], base: &[u8], fallback: &[u8; 6]) -> bool {
    let tail = if name.first() == Some(&b'.')
        && name
            .get(1..base.len() + 1)
            .is_some_and(|s| s.eq_ignore_ascii_case(base))
    {
        base.len() + 1
    } else if name
        .get(..6)
        .is_some_and(|s| s.eq_ignore_ascii_case(&base[..6]))
        && name.get(6) == Some(&b'~')
        && name.get(7).is_some_and(|b| matches!(b, b'1'..=b'4'))
    {
        8
    } else {
        if name.len() < 8 {
            return false;
        }
        let mut digits = false;
        let mut index = 0;
        while index < 8 {
            let byte = name[index];
            if digits {
                if !byte.is_ascii_digit() {
                    return false;
                }
            } else if byte == b'~' {
                index += 1;
                if index >= 8 || !matches!(name[index], b'1'..=b'9') {
                    return false;
                }
                digits = true;
            } else if index >= 6 || byte.to_ascii_lowercase() != fallback[index] {
                return false;
            }
            index += 1;
        }
        8
    };
    for &byte in &name[tail..] {
        if byte == b':' {
            return true;
        }
        if byte != b' ' && byte != b'.' {
            return false;
        }
    }
    true
}

fn hfs(mut name: &[u8], base: &[u8]) -> bool {
    if next_hfs(&mut name) != u32::from(b'.') {
        return false;
    }
    for &byte in base {
        let c = next_hfs(&mut name);
        if c > 127 || (c as u8).to_ascii_lowercase() != byte {
            return false;
        }
    }
    matches!(next_hfs(&mut name), 0 | 47)
}

fn next_hfs(name: &mut &[u8]) -> u32 {
    loop {
        let Some(&first) = name.first() else {
            return 0;
        };
        let width = match first {
            0..=127 => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return 0,
        };
        let Some(bytes) = name.get(..width) else {
            return 0;
        };
        let Ok(text) = core::str::from_utf8(bytes) else {
            return 0;
        };
        let Some(c) = text.chars().next() else {
            return 0;
        };
        *name = &name[width..];
        let value = u32::from(c);
        if !matches!(value, 0x200c..=0x200f | 0x202a..=0x202e | 0x206a..=0x206f | 0xfeff) {
            return value;
        }
    }
}
