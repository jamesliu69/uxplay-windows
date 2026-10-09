use airplay_session::SessionServer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn read_response(stream: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp).await.expect("read");
        assert!(n > 0, "server closed connection");
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8(buf[..pos].to_vec()).expect("utf8 head");
            let cl: usize = head
                .lines()
                .skip(1)
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.trim().parse().ok())
                .unwrap_or(0);
            if buf.len() >= pos + 4 + cl {
                return String::from_utf8_lossy(&buf[..pos + 4 + cl]).into_owned();
            }
        }
    }
}

#[tokio::test]
async fn options_and_announce_echo_cseq() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        SessionServer::default()
            .serve_listener(listener, tx)
            .await
            .unwrap();
    });

    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n")
        .await
        .unwrap();
    let resp = read_response(&mut client).await;
    assert!(resp.contains("200"), "OPTIONS: {resp}");
    assert!(resp.contains("CSeq: 1"), "OPTIONS: {resp}");

    client
        .write_all(
            b"ANNOUNCE rtsp://127.0.0.1/stream RTSP/1.0\r\nCSeq: 2\r\nContent-Length: 0\r\n\r\n",
        )
        .await
        .unwrap();
    let resp = read_response(&mut client).await;
    assert!(resp.contains("200"), "ANNOUNCE: {resp}");
    assert!(resp.contains("CSeq: 2"), "ANNOUNCE: {resp}");
}
