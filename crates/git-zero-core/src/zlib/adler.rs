/// Streaming Adler-32. Reducing every 5552 bytes bounds both sums even for 0xff.
/// The pending count spans calls so one-byte inflation uses the same batching.
pub(super) struct Adler32 {
    s1: u32,
    s2: u32,
    pending: u16,
}

impl Adler32 {
    pub(super) fn new() -> Self {
        Self {
            s1: 1,
            s2: 0,
            pending: 0,
        }
    }

    #[inline]
    pub(super) fn update(&mut self, mut bytes: &[u8]) {
        let (mut s1, mut s2) = (self.s1, self.s2);
        while !bytes.is_empty() {
            let count = bytes.len().min(5552 - usize::from(self.pending));
            for &byte in &bytes[..count] {
                s1 += u32::from(byte);
                s2 += s1;
            }
            self.pending += count as u16;
            if self.pending == 5552 {
                s1 %= 65521;
                s2 %= 65521;
                self.pending = 0;
            }
            bytes = &bytes[count..];
        }
        self.s1 = s1;
        self.s2 = s2;
    }

    pub(super) fn value(&self) -> u32 {
        ((self.s2 % 65521) << 16) | (self.s1 % 65521)
    }
}
