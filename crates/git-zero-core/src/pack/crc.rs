/// Standard reflected IEEE CRC-32 used by Git pack indexes (initial/final XOR).
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(bytes);
    crc.value()
}

/// Incremental wire checksum shared by pack readers and index writers.
pub(super) struct Crc32(u32);

impl Crc32 {
    pub(super) const fn new() -> Self {
        Self(u32::MAX)
    }
    pub(super) fn value(&self) -> u32 {
        !self.0
    }
    pub(super) fn update(&mut self, bytes: &[u8]) {
        const TABLE: [[u32; 256]; 8] = crc_tables();
        let mut crc = self.0;
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let low = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) ^ crc;
            crc = TABLE[7][usize::from(low as u8)]
                ^ TABLE[6][usize::from((low >> 8) as u8)]
                ^ TABLE[5][usize::from((low >> 16) as u8)]
                ^ TABLE[4][(low >> 24) as usize]
                ^ TABLE[3][usize::from(chunk[4])]
                ^ TABLE[2][usize::from(chunk[5])]
                ^ TABLE[1][usize::from(chunk[6])]
                ^ TABLE[0][usize::from(chunk[7])];
        }
        for &byte in chunks.remainder() {
            crc = (crc >> 8) ^ TABLE[0][usize::from(crc as u8 ^ byte)];
        }
        self.0 = crc;
    }
}

const fn crc_tables() -> [[u32; 256]; 8] {
    let mut table = [[0; 256]; 8];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = (value >> 1) ^ if value & 1 != 0 { 0xedb8_8320 } else { 0 };
            bit += 1;
        }
        table[0][index] = value;
        index += 1;
    }
    let mut slice = 1;
    while slice < 8 {
        index = 0;
        while index < 256 {
            let previous = table[slice - 1][index];
            table[slice][index] = (previous >> 8) ^ table[0][(previous & 255) as usize];
            index += 1;
        }
        slice += 1;
    }
    table
}
