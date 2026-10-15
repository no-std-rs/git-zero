use git_zero::Result;

pub fn collect(mut read: impl FnMut(&mut [u8]) -> Result<usize>, capacity: usize) -> Vec<u8> {
    let mut result = Vec::new();
    let mut storage = vec![0xa5; capacity + 2];
    loop {
        let count =
            crate::allocations::without_allocations(|| read(&mut storage[1..=capacity])).unwrap();
        assert!(count <= capacity);
        assert_eq!(storage[0], 0xa5);
        assert_eq!(storage[capacity + 1], 0xa5);
        if count == 0 {
            break;
        }
        // Model a host accepting one byte per write, retaining the unsent tail.
        for byte in &storage[1..1 + count] {
            result.push(*byte);
        }
    }
    assert_eq!(read(&mut []).unwrap(), 0);
    result
}
