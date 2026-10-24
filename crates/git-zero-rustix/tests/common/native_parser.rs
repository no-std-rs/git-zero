//! Socket-free native HTTP target shared by ordinary Cargo and libFuzzer.
use git_zero::Error;
use git_zero_rustix::{ChunkedLimits, RequestPolicy, read_request_from_chunked};

pub fn exercise(data: &[u8]) {
    if data.len() < 4 || data.len() > 4096 {
        return;
    }
    let wire = &data[4..];
    let width = 1 + usize::from(data[0]) % 127;
    let capacity = [0, 1, 16, 128, 1024, 4096][usize::from(data[1]) % 6];
    let mut policy = RequestPolicy::default();
    if data[2] & 1 != 0 {
        policy.request.max_wire_bytes = Some(usize::from(data[3]) * 16);
    }
    if data[2] & 2 != 0 {
        policy.request.max_body_bytes = Some(usize::from(data[3]));
    }
    if data[2] & 4 != 0 {
        policy.headers.max_bytes = Some(usize::from(data[3]) * 8);
    }
    if data[2] & 8 != 0 {
        policy.headers.max_fields = Some(usize::from(data[3]) % 16);
    }
    let chunks = ChunkedLimits {
        max_chunks: 1 + usize::from(data[3]),
        max_line_bytes: 1 + usize::from(data[0]),
    };
    let mut storage = [0xa7; 4096];
    let mut position = 0;
    let mut calls = 0;
    let result = read_request_from_chunked(
        &mut |output| {
            calls += 1;
            assert!(
                calls <= wire.len() + 1,
                "parser repeated a read without consuming input"
            );
            let count = width.min(output.len()).min(wire.len() - position);
            output[..count].copy_from_slice(&wire[position..position + count]);
            position += count;
            Ok(count)
        },
        &mut storage[..capacity],
        policy,
        chunks,
    );
    if let Ok(request) = result {
        assert!(request.path.starts_with('/'));
        assert!(!request.path.bytes().any(|byte| byte <= 32 || byte == 127));
        if let Some(limit) = policy.request.max_body_bytes {
            assert!(request.body.len() <= limit);
        }
        assert!(request.body.len() <= capacity);
    }
    assert!(storage[capacity..].iter().all(|&byte| byte == 0xa7));
    // Every failure leaves the same storage reusable for an unrelated request.
    let clean = b"POST /clean HTTP/1.1\r\nContent-Length: 4\r\n\r\n0000";
    let mut remaining = clean.as_slice();
    let request = read_request_from_chunked(
        &mut |output| {
            let count = remaining.len().min(output.len());
            output[..count].copy_from_slice(&remaining[..count]);
            remaining = &remaining[count..];
            Ok(count)
        },
        &mut storage,
        RequestPolicy::default(),
        ChunkedLimits::default(),
    )
    .unwrap();
    assert_eq!(request.path, "/clean");
    assert_eq!(request.body, b"0000");
    // Transport errors are terminal and must never be converted to empty EOF.
    assert_eq!(
        read_request_from_chunked(
            &mut |_| Err(Error::Backend),
            &mut storage,
            RequestPolicy::default(),
            chunks
        )
        .err()
        .unwrap(),
        Error::Backend
    );
}
