use super::*;
pub(super) fn protocol_rejections(root: &Path, cases: &mut Vec<Value>) {
    use std::net::TcpStream;
    use tungstenite::{
        Message, WebSocket,
        client::{IntoClientRequest, client_with_config},
        protocol::frame::{
            Frame,
            coding::{Data, OpCode},
        },
    };
    fn register(repo: &str) -> Value {
        let body = json!({"codec_origin":"http://127.0.0.1:8840","service_socket":1}).to_string();
        let output = Command::new("curl")
            .args([
                "--silent",
                "--show-error",
                "--fail-with-body",
                "--http1.1",
                "--max-time",
                "10",
                "-H",
                "x-hosting-token: git-zero-local-services",
                "-H",
                &format!("x-hosting-repo: {repo}"),
                "-H",
                "x-hosting-phase: startup",
                "--data-binary",
                &body,
                "http://127.0.0.1:8839/_services/register",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn connect(
        repo: &str,
        session: &str,
        token: &str,
        protocol: &str,
    ) -> Result<WebSocket<TcpStream>, String> {
        let stream = TcpStream::connect("127.0.0.1:8839").unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream.set_nodelay(true).unwrap();
        let mut request = "ws://127.0.0.1:8839/_services/socket"
            .into_client_request()
            .unwrap();
        for (name, value) in [
            ("x-hosting-token", token),
            ("x-hosting-repo", repo),
            ("x-hosting-phase", "startup"),
            ("x-hosting-socket-session", session),
            ("sec-websocket-protocol", protocol),
        ] {
            request.headers_mut().insert(
                tungstenite::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        client_with_config(request, stream, None)
            .map(|v| v.0)
            .map_err(|e| e.to_string())
    }
    fn request(id: u32, metadata: &[u8], body: &[u8]) -> Vec<u8> {
        let mut bytes = b"GZSQ".to_vec();
        for x in [1, id, metadata.len() as u32, body.len() as u32] {
            bytes.extend(x.to_le_bytes());
        }
        bytes.extend(metadata);
        bytes.extend(body);
        bytes
    }
    fn closed(socket: &mut WebSocket<TcpStream>) -> String {
        for _ in 0..16 {
            match socket.read() {
                Ok(Message::Close(code)) => return format!("close {code:?}"),
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    panic!("peer did not close: {error}")
                }
                Err(error) => return format!("error {error}"),
                Ok(Message::Ping(_)) => socket.flush().unwrap(),
                Ok(_) => {}
            }
        }
        panic!("expected bounded protocol close");
    }
    let names = [
        "bad-token",
        "bad-session",
        "bad-subprotocol",
        "duplicate-active",
        "text",
        "bad-id",
        "unknown-route",
        "private-header",
        "bad-phase",
        "duplicate-metadata",
        "bad-length",
        "over-route-body",
        "over-frame-body",
        "orphan-continuation",
        "rsv",
        "oversized-control",
        "ping",
        "duplicate-id-after-ack",
        "request-before-ack",
        "wrong-ack",
        "registration-rotation",
    ];
    let mut results = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let repo = format!("socket-protocol-{index}");
        let registered = register(&repo);
        let session = registered["service_socket"]["session"].as_str().unwrap();
        let observer =
            HttpServices::new("http://127.0.0.1:8839", &repo, "git-zero-local-services").unwrap();
        let (raw_before, before) = usage(&observer);
        let mut expected_do = 0;
        let mut expected_worker = 1;
        let mut expected_put = 0;
        let outcome;
        if matches!(*name, "bad-token" | "bad-session" | "bad-subprotocol") {
            let connection = connect(
                &repo,
                if *name == "bad-session" {
                    "00000000000000000000000000000000"
                } else {
                    session
                },
                if *name == "bad-token" {
                    "incorrect-local-token"
                } else {
                    "git-zero-local-services"
                },
                if *name == "bad-subprotocol" {
                    "wrong-subprotocol"
                } else {
                    "git-zero-service-rpc.v1"
                },
            );
            assert!(connection.is_err());
            outcome = connection.err().unwrap();
        } else {
            let mut socket = connect(
                &repo,
                session,
                "git-zero-local-services",
                "git-zero-service-rpc.v1",
            )
            .unwrap();
            let basic =
                json!({"method":"POST","route":"blobs/read","phase":"startup","proof":null});
            let metadata = serde_json::to_vec(&basic).unwrap();
            match *name {
                "duplicate-active" => {
                    assert!(
                        connect(
                            &repo,
                            session,
                            "git-zero-local-services",
                            "git-zero-service-rpc.v1"
                        )
                        .is_err()
                    );
                    expected_worker = 2;
                    socket.close(None).unwrap();
                    outcome = "second authenticated Upgrade rejected before DO".to_owned();
                }
                "registration-rotation" => {
                    let new = register(&repo);
                    assert_ne!(
                        new["service_socket"]["session"],
                        registered["service_socket"]["session"]
                    );
                    expected_worker = 2;
                    // Cross-request passive close can be unavailable in the
                    // runtime. An actual old-session application RPC must be
                    // rejected by captured registration identity before DO I/O.
                    let body = b"stale registration must never publish a blob";
                    let header=serde_json::to_vec(&json!({"method":"PUT","route":format!("blob/chunks/{}",hash(body)),"phase":"startup","proof":null})).unwrap();
                    socket
                        .send(Message::Binary(request(1, &header, body).into()))
                        .unwrap();
                    outcome = closed(&mut socket);
                }
                "ping" => {
                    socket.send(Message::Ping(vec![1, 2, 3].into())).unwrap();
                    assert!(
                        matches!(socket.read().unwrap(),Message::Pong(v) if v.as_ref()==[1,2,3])
                    );
                    socket.close(None).unwrap();
                    outcome = "ping/pong is transport only".into();
                }
                "duplicate-id-after-ack" | "request-before-ack" | "wrong-ack" => {
                    let data = b"one actual PUT before invalid next message";
                    let header=serde_json::to_vec(&json!({"method":"PUT","route":format!("blob/chunks/{}",hash(data)),"phase":"startup","proof":null})).unwrap();
                    socket
                        .send(Message::Binary(request(1, &header, data).into()))
                        .unwrap();
                    let first = socket.read().unwrap().into_data();
                    assert!(first.starts_with(b"GZSR"));
                    if *name != "request-before-ack" {
                        let terminal = socket.read().unwrap().into_data();
                        assert!(terminal.starts_with(b"GZSE"));
                        let mut ack = b"GZSA".to_vec();
                        ack.extend(1u32.to_le_bytes());
                        ack.extend(if *name == "wrong-ack" { 999u32 } else { 1u32 }.to_le_bytes());
                        socket.send(Message::Binary(ack.into())).unwrap();
                    }
                    if *name != "wrong-ack" {
                        socket
                            .send(Message::Binary(
                                request(
                                    if *name == "duplicate-id-after-ack" {
                                        1
                                    } else {
                                        2
                                    },
                                    &header,
                                    data,
                                )
                                .into(),
                            ))
                            .unwrap();
                    }
                    expected_do = 1;
                    expected_put = 1;
                    outcome = closed(&mut socket);
                }
                "orphan-continuation" => {
                    socket
                        .send(Message::Frame(Frame::message(
                            vec![1, 2, 3],
                            OpCode::Data(Data::Continue),
                            true,
                        )))
                        .unwrap();
                    outcome = closed(&mut socket);
                }
                "rsv" => {
                    let mut bad = Frame::message(
                        request(1, &metadata, b"{}"),
                        OpCode::Data(Data::Binary),
                        true,
                    );
                    bad.header_mut().rsv1 = true;
                    socket.send(Message::Frame(bad)).unwrap();
                    outcome = closed(&mut socket);
                }
                "oversized-control" => {
                    socket
                        .send(Message::Frame(Frame::ping(vec![0; 126])))
                        .unwrap();
                    outcome = closed(&mut socket);
                }
                "text" => {
                    socket
                        .send(Message::Text("not a binary RPC".into()))
                        .unwrap();
                    outcome = closed(&mut socket);
                }
                _ => {
                    let mut value = basic;
                    let bytes=match *name {
                        "bad-id"=>request(2,&metadata,b"{}"),
                        "unknown-route"=>{value["route"]=json!("manifest/first-read");request(1,&serde_json::to_vec(&value).unwrap(),b"{}")},
                        "private-header"=>{value["x-git-zero-internal-catalog-inline"]=json!("1");request(1,&serde_json::to_vec(&value).unwrap(),b"{}")},
                        "bad-phase"=>{value["phase"]=json!("invented");request(1,&serde_json::to_vec(&value).unwrap(),b"{}")},
                        "duplicate-metadata"=>request(1,b"{\"method\":\"POST\",\"method\":\"PUT\",\"route\":\"blobs/read\",\"phase\":\"startup\",\"proof\":null}",b"{}"),
                        "bad-length"=>{let mut b=request(1,&metadata,b"{}");b[16..20].copy_from_slice(&999u32.to_le_bytes());b},
                        "over-route-body"=>request(1,&metadata,&vec![0;1_500_001]),
                        "over-frame-body"=>vec![0;20+4096+16_779_276+1],
                        _=>unreachable!()
                    };
                    socket.send(Message::Binary(bytes.into())).unwrap();
                    outcome = closed(&mut socket);
                }
            }
        }
        let (raw_after, after) = usage(&observer);
        let delta = after.checked_workload_delta(&before).unwrap();
        assert_eq!(delta.worker_requests, expected_worker, "{name}");
        assert_eq!(delta.do_requests, expected_do, "{name}");
        assert_eq!(delta.r2_class_a, expected_put, "{name}");
        assert_eq!(delta.r2_class_b, 0, "{name}");
        assert_eq!(delta.r2_deletes, 0, "{name}");
        assert_eq!(after.accounting_complete, Some(true));
        if expected_do == 0 {
            assert_eq!(delta.do_rows_written, Some(0));
            assert_eq!(delta.r2_written_bytes, 0);
        }
        results.push(json!({"name":name,"outcome":outcome,"before":raw_before,"after":raw_after,"actual_delta":delta,
            "no_retry_or_repeated_storage_dispatch":true}));
        write(
            root,
            "protocol-rejections.json",
            &json!({"kind":"actual pinned production workerd protocol controls; no benchmark","cases":results}),
        );
    }
    cases.push(
        json!({"name":"production-protocol-controls","case_count":results.len(),"cases":results}),
    );
}
