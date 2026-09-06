use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

async fn request_bytes(socket: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        socket.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
        assert!(bytes.len() < 8192);
    }
    String::from_utf8(bytes).unwrap()
}
async fn serve(response: &'static [u8]) -> (url::Url, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = url::Url::parse(&format!(
        "http://{}/rest/stream.view?t=private",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = request_bytes(&mut socket).await;
        socket.write_all(response).await.unwrap();
        socket.shutdown().await.unwrap();
        request
    });
    (url, task)
}
fn request(url: url::Url) -> HttpRequest {
    HttpRequest {
        url,
        max_bytes: 1024,
        range: None,
        if_range: None,
        json_body: None,
    }
}

#[tokio::test]
async fn json_post_preserves_query_and_body_with_bounded_response() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = url::Url::parse(&format!(
        "http://{}/proxy/rest/getTranscodeDecision.view?apiKey=private&mediaId=opaque",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let body = br#"{"name":"Hummingbird","maxTranscodingAudioBitrate":192000}"#;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let header = request_bytes(&mut socket).await;
        assert!(header.starts_with(
            "POST /proxy/rest/getTranscodeDecision.view?apiKey=private&mediaId=opaque HTTP/1.1\r\n"
        ));
        assert!(
            header
                .to_ascii_lowercase()
                .contains("content-type: application/json\r\n")
        );
        assert!(!header.to_ascii_lowercase().contains("range:"));
        let mut received = vec![0; body.len()];
        socket.read_exact(&mut received).await.unwrap();
        assert_eq!(received, body);
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .await
            .unwrap();
    });
    let mut input = request(url);
    input.json_body = Some(body.to_vec());
    assert_eq!(
        NetworkTransport::new()
            .unwrap()
            .execute(input)
            .await
            .unwrap()
            .body,
        b"{}"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn invalid_post_requests_are_rejected_before_connecting() {
    let transport = NetworkTransport::new().unwrap();
    let url = url::Url::parse("http://127.0.0.1:1/").unwrap();
    let mut oversized = request(url.clone());
    oversized.json_body = Some(vec![b' '; MAX_JSON_REQUEST_BYTES + 1]);
    assert_eq!(
        transport.execute(oversized).await.err().unwrap().kind,
        BackendErrorKind::ResourceLimit
    );
    let mut ranged = request(url.clone());
    ranged.json_body = Some(b"{}".to_vec());
    ranged.range = Some(ByteRange {
        start: 0,
        end: None,
    });
    assert_eq!(
        transport.execute(ranged).await.err().unwrap().kind,
        BackendErrorKind::MalformedResponse
    );
    let mut stream = request(url);
    stream.json_body = Some(b"{}".to_vec());
    assert_eq!(
        transport.open(stream).await.err().unwrap().kind,
        BackendErrorKind::MalformedResponse
    );
}
#[tokio::test]
async fn actual_range_headers_and_truncation_are_checked() {
    let (url, server) = serve(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 4\r\nContent-Range: bytes 4-7/8\r\nETag: \"v1\"\r\nConnection: close\r\n\r\nefgh").await;
    let range = ByteRange {
        start: 4,
        end: None,
    };
    let mut input = request(url);
    input.range = Some(range);
    input.if_range = Some("\"v1\"".into());
    let transport = NetworkTransport::new().unwrap();
    let mut stream = transport.open(input).await.unwrap();
    stream
        .head
        .validate_range(range, Some(8), Some("\"v1\""))
        .unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.body.next_chunk().await.unwrap() {
        bytes.extend_from_slice(&chunk);
    }
    assert_eq!(bytes, b"efgh");
    let sent = server.await.unwrap().to_ascii_lowercase();
    assert!(sent.contains("range: bytes=4-\r\n"));
    assert!(sent.contains("if-range: \"v1\"\r\n"));
    assert!(sent.contains("accept-encoding: identity\r\n"));

    let (url, server) =
        serve(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort").await;
    let mut stream = transport.open(request(url)).await.unwrap();
    loop {
        match stream.body.next_chunk().await {
            Ok(Some(_)) => {}
            Err(error) => {
                assert_eq!(error.kind, BackendErrorKind::Network);
                break;
            }
            Ok(None) => panic!("truncated body was accepted as complete"),
        }
    }
    server.await.unwrap();
}
#[tokio::test]
async fn chunked_length_limits_and_http_error_classification() {
    let transport = NetworkTransport::new().unwrap();
    let (url, server) = serve(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nabcd\r\n4\r\nefgh\r\n0\r\n\r\n").await;
    let mut input = request(url);
    input.max_bytes = 6;
    let error = transport.execute(input).await.err().unwrap();
    assert_eq!(error.kind, BackendErrorKind::ResourceLimit);
    server.await.unwrap();
    let (url, server) = serve(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nRetry-After: 12\r\nConnection: close\r\n\r\n").await;
    let error = transport.open(request(url)).await.err().unwrap();
    assert_eq!(error.kind, BackendErrorKind::RateLimited);
    assert_eq!(error.retry_after_ms, Some(12000));
    server.await.unwrap();
}
#[tokio::test]
async fn redirects_cannot_send_authentication_to_another_origin() {
    let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = url::Url::parse(&format!(
        "http://{}/rest/stream.view?t=private",
        origin.local_addr().unwrap()
    ))
    .unwrap();
    let target = destination.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = origin.accept().await.unwrap();
        request_bytes(&mut socket).await;
        socket.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://{target}/?t=private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    });
    let error = NetworkTransport::new()
        .unwrap()
        .open(request(url))
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, BackendErrorKind::MalformedResponse);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), destination.accept())
            .await
            .is_err()
    );
    server.await.unwrap();
}

#[test]
fn only_credential_free_bandcamp_cdn_redirects_cross_origins() {
    let origin =
        url::Url::parse("https://bandcamp.com/api/subsonic/rest/stream.view?u=user&t=token&s=salt")
            .unwrap();
    let cdn = url::Url::parse("https://t4.bcbits.com/stream/file.mp3?token=media").unwrap();
    assert!(redirect_allowed(std::slice::from_ref(&origin), &cdn));

    for target in [
        "http://t4.bcbits.com/stream/file.mp3",
        "https://evil.example/stream/file.mp3",
        "https://notbcbits.com/stream/file.mp3",
        "https://t4.bcbits.com/stream/file.mp3?u=user&t=token",
        "https://user:password@t4.bcbits.com/stream/file.mp3",
    ] {
        let target = url::Url::parse(target).unwrap();
        assert!(!redirect_allowed(std::slice::from_ref(&origin), &target));
    }
}
#[tokio::test]
async fn stalled_media_read_times_out_and_dropping_it_closes_the_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = url::Url::parse(&format!(
        "http://{}/rest/stream.view",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        request_bytes(&mut socket).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
            .await
            .unwrap();
        let mut buffer = [0];
        // A dropped stalled body must not keep a connection worker alive.
        let closed = tokio::time::timeout(Duration::from_secs(3), socket.read(&mut buffer))
            .await
            .unwrap();
        assert!(matches!(closed, Ok(0) | Err(_)));
    });
    let transport = NetworkTransport::with_read_timeout(Duration::from_millis(200)).unwrap();
    let mut stream = transport.open(request(url)).await.unwrap();
    let error = tokio::time::timeout(Duration::from_secs(2), stream.body.next_chunk())
        .await
        .unwrap()
        .err()
        .unwrap();
    assert_eq!(error.kind, BackendErrorKind::Network);
    drop(stream);
    server.await.unwrap();
}

#[tokio::test]
async fn same_origin_redirects_work_without_copying_authentication_into_referrers() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = url::Url::parse(&format!(
        "http://{}/rest/stream.view?t=private",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        request_bytes(&mut first).await;
        first.write_all(b"HTTP/1.1 302 Found\r\nLocation: /proxy/audio?t=private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        first.shutdown().await.unwrap();
        let (mut second, _) = listener.accept().await.unwrap();
        let request = request_bytes(&mut second).await;
        second
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndata")
            .await
            .unwrap();
        request
    });
    let response = NetworkTransport::new()
        .unwrap()
        .execute(request(url))
        .await
        .unwrap();
    assert_eq!(response.body, b"data");
    let sent = server.await.unwrap();
    assert!(sent.starts_with("GET /proxy/audio?t=private "));
    assert!(!sent.to_ascii_lowercase().contains("referer:"));
}
