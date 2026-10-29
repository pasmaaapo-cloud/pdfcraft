//! End-to-end tests of the CraftStudio additions: the AI Assistant tools (against a local mock of
//! the Messages API) and Create PDF from an Office file (with a real LibreOffice when installed).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc;

use pdfcraft_automation::{Automation, Content, ToolError};
use serde_json::{Value, json};

fn workdir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-craftstudio-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ok(a: &mut Automation, tool: &str, args: Value) -> Value {
    match a.call(tool, &args) {
        Ok(mut c) => match c.remove(0) {
            Content::Json(v) => v,
            other => panic!("{tool}: expected JSON, got {other:?}"),
        },
        Err(e) => panic!("{tool} {args}: {e}"),
    }
}

/// A one-request HTTP server on this computer answering `status` and `body`; the request's
/// headers and JSON body come back over the channel.
fn mock_messages_api(status: u16, body: Value) -> (String, mpsc::Receiver<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else { return };
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut head = String::new();
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
            head.push_str(&line);
        }
        let mut buf = vec![0; len];
        reader.read_exact(&mut buf).unwrap();
        let _ = tx.send((head, serde_json::from_slice(&buf).unwrap()));
        let body = body.to_string();
        let mut stream = stream;
        let _ = write!(
            stream,
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
    });
    (url, rx)
}

fn settings(endpoint: &str) -> pdfcraft_ai::Settings {
    pdfcraft_ai::Settings { api_key: "sk-test-key".into(), model: "claude-haiku-5-5".into(), endpoint: endpoint.into() }
}

fn invoice(a: &mut Automation) -> u64 {
    ok(a, "doc_create", json!({ "from": "text", "text": "Lasku 42\nSumma 120 euroa\n\u{c}Eräpäivä 1.12.2026" }))["doc"].as_u64().unwrap()
}

#[test]
fn ai_ask_sends_the_pages_and_returns_cited_pages() {
    let dir = workdir("ai-ask");
    let answer =
        json!({ "content": [{ "type": "text", "text": "Eräpäivä on 1.12.2026 [s. 2]." }], "usage": { "input_tokens": 50, "output_tokens": 12 } });
    let (url, seen) = mock_messages_api(200, answer);
    let mut a = Automation::new().with_root(&dir).unwrap().with_ai_settings(settings(&url));
    let doc = invoice(&mut a);
    let r = ok(&mut a, "ai_ask", json!({ "doc": doc, "question": "Milloin eräpäivä on?" }));
    assert_eq!(r["text"], "Eräpäivä on 1.12.2026 [s. 2].");
    assert_eq!((r["cited_pages"].clone(), r["pages_sent"].clone(), r["truncated"].clone()), (json!([2]), json!([1, 2]), json!(false)));
    assert_eq!((r["model"].clone(), r["input_tokens"].clone()), (json!("claude-haiku-5-5"), json!(50)));
    let (head, body) = seen.recv().unwrap();
    assert!(head.starts_with("POST /v1/messages "), "{head}");
    assert!(head.to_ascii_lowercase().contains("x-api-key: sk-test-key"), "{head}");
    let content = body["messages"][0]["content"].as_str().unwrap();
    assert!(
        content.contains("<page number=\"2\">") && content.contains("Eräpäivä 1.12.2026") && content.contains("Milloin eräpäivä on?"),
        "{content}"
    );
    assert!(!ok(&mut a, "ai_status", json!({})).to_string().contains("sk-test-key"), "status never shows the key");
}

#[test]
fn ai_tools_refuse_before_sending_anything() {
    let dir = workdir("ai-refuse");
    // A port nothing listens on: any attempt to send would fail with a network error instead.
    let mut a = Automation::new().with_root(&dir).unwrap().with_ai_settings(settings("http://127.0.0.1:9"));
    let doc = invoice(&mut a);
    assert!(matches!(a.call("ai_ask", &json!({ "doc": doc, "question": "  " })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(a.call("ai_ask", &json!({ "doc": doc })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(a.call("ai_summarize", &json!({ "doc": doc, "pages": [7] })), Err(ToolError::InvalidArgs(_))));
    let blank = ok(&mut a, "doc_create", json!({ "from": "blank" }))["doc"].as_u64().unwrap();
    assert!(matches!(a.call("ai_summarize", &json!({ "doc": blank })), Err(ToolError::Failed(m)) if m.contains("no text")));
    // An endpoint that is plain HTTP to another computer is never used.
    let mut far = Automation::new().with_root(&dir).unwrap().with_ai_settings(settings("http://example.com"));
    let doc = invoice(&mut far);
    assert!(matches!(far.call("ai_summarize", &json!({ "doc": doc })), Err(ToolError::Failed(m)) if m.contains("https://")));
}

#[test]
fn ai_errors_from_the_service_are_named() {
    let dir = workdir("ai-401");
    let (url, _seen) = mock_messages_api(401, json!({ "error": { "type": "authentication_error", "message": "invalid x-api-key" } }));
    let mut a = Automation::new().with_root(&dir).unwrap().with_ai_settings(settings(&url));
    let doc = invoice(&mut a);
    assert!(
        matches!(a.call("ai_summarize", &json!({ "doc": doc, "language": "Finnish" })), Err(ToolError::Failed(m)) if m.contains("API key was refused"))
    );
}

#[test]
fn ai_status_reports_the_setup_without_the_key() {
    let mut a = Automation::new().with_ai_settings(settings("https://api.anthropic.com"));
    let s = ok(&mut a, "ai_status", json!({}));
    assert_eq!((s["configured"].clone(), s["model"].clone()), (json!(true), json!("claude-haiku-5-5")));
}

#[test]
fn doc_create_from_office_converts_with_libreoffice() {
    let dir = workdir("office");
    std::fs::write(dir.join("notes.txt.docx"), b"not really a docx").unwrap();
    let mut a = Automation::new().with_root(&dir).unwrap();
    assert!(matches!(a.call("doc_create", &json!({ "from": "office", "path": "a.pdf" })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(a.call("doc_create", &json!({ "from": "office", "path": "../x.docx" })), Err(e) if e.to_string().contains("outside")));
    let Some(_) = pdfcraft_engine::office::find_office() else {
        let r = a.call("doc_create", &json!({ "from": "office", "path": "notes.txt.docx" }));
        assert!(matches!(&r, Err(ToolError::Failed(m)) if m.contains("LibreOffice")), "{r:?}");
        eprintln!("skipped the conversion: LibreOffice not installed");
        return;
    };
    std::fs::write(
        dir.join("Tarjous ä.rtf"),
        br"{\rtf1\ansi\deff0{\fonttbl{\f0 Helvetica;}}\f0\fs24 Tarjous 7: hinta 950 euroa, voimassa 30 p\u228?iv\u228?\u228?.\par}",
    )
    .unwrap();
    let r = ok(&mut a, "doc_create", json!({ "from": "office", "path": "Tarjous ä.rtf" }));
    assert_eq!(r["name"], "Tarjous ä.pdf");
    let doc = r["doc"].as_u64().unwrap();
    let text = ok(&mut a, "text_extract", json!({ "doc": doc }));
    assert!(text.to_string().contains("päivää"), "{text}");
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "tarjous.pdf" }));
    assert!(std::fs::read(dir.join("tarjous.pdf")).unwrap().starts_with(b"%PDF-"));
    // A damaged Office file never crashes: LibreOffice either refuses it or reads what it can
    // as text, and what comes back is a PDF that opened.
    match a.call("doc_create", &json!({ "from": "office", "path": "notes.txt.docx" })) {
        Ok(_) | Err(ToolError::Failed(_)) => {}
        other => panic!("{other:?}"),
    }
}
