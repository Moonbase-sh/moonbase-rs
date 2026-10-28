//! The bundled ureq transport, against a throwaway local HTTP server.

#![cfg(feature = "ureq-transport")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use moonbase::{Error, HttpRequest, HttpTransport, Method, UreqTransport};

/// Answers one connection with `response`, and hands back what it received.
fn serve_once(response: &'static str) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut head = String::new();
        let mut content_length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                content_length = value.trim().parse().unwrap();
            }
            head.push_str(&line);
            if line == "\r\n" {
                break;
            }
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).unwrap();
        let mut stream = stream;
        stream.write_all(response.as_bytes()).unwrap();
        head + &String::from_utf8(body).unwrap()
    });
    (address, handle)
}

fn request(method: Method, url: String, body: &str) -> HttpRequest {
    HttpRequest {
        method,
        url,
        headers: BTreeMap::from([
            ("User-Agent".to_owned(), "moonbase-rs/test".to_owned()),
            ("x-mb-client".to_owned(), "moonbase-rs".to_owned()),
            ("Content-Type".to_owned(), "text/plain".to_owned()),
        ]),
        connect_timeout: Duration::from_secs(2),
        request_timeout: Duration::from_secs(5),
        body: body.to_owned(),
    }
}

#[test]
fn sends_headers_and_body_and_returns_any_status_as_a_response() {
    let (address, server) = serve_once(
        "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 60\r\nContent-Length: 4\r\nConnection: close\r\n\r\nslow",
    );
    let response = UreqTransport::new()
        .send(&request(Method::Post, format!("{address}/api/client/licenses/x/validate?format=JWT"), "the.jwt.token"))
        .unwrap();

    assert_eq!(response.status, 429);
    assert_eq!(response.header("retry-after"), Some("60"));
    assert_eq!(response.body, "slow");

    let received = server.join().unwrap();
    assert!(received.starts_with("POST /api/client/licenses/x/validate?format=JWT HTTP/1.1\r\n"));
    let lower = received.to_ascii_lowercase();
    assert!(lower.contains("user-agent: moonbase-rs/test\r\n"), "{received}");
    assert!(lower.contains("x-mb-client: moonbase-rs\r\n"));
    assert!(received.ends_with("\r\n\r\nthe.jwt.token"));
}

#[test]
fn a_get_carries_no_body() {
    let (address, server) = serve_once("HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n");
    let response = UreqTransport::new().send(&request(Method::Get, format!("{address}/poll"), "")).unwrap();
    assert_eq!(response.status, 204);
    assert!(server.join().unwrap().starts_with("GET /poll HTTP/1.1\r\n"));
}

#[test]
fn no_response_at_all_is_an_api_error_with_status_zero() {
    // Bind and drop, so nothing listens on the port.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let error = UreqTransport::new().send(&request(Method::Get, format!("http://127.0.0.1:{port}/"), "")).unwrap_err();
    assert!(matches!(&error, Error::Api(api) if api.status_code() == 0), "{error:?}");
}

#[test]
fn the_request_timeout_bounds_a_server_that_never_answers() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let _hold = thread::spawn(move || {
        let connection = listener.accept();
        thread::sleep(Duration::from_secs(5));
        drop(connection);
    });

    let mut slow = request(Method::Get, address, "");
    slow.request_timeout = Duration::from_millis(300);
    let started = Instant::now();
    let error = UreqTransport::new().send(&slow).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(matches!(&error, Error::Api(api) if api.status_code() == 0), "{error:?}");
}
