//! The AI Assistant panel in the real shell (egui_kittest), against a local mock of the Messages
//! API, and Create ▸ PDF from Office file (with a real LibreOffice when installed).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_engine::Session;
use pdfcraft_ui_egui::PdfCraftApp;
use pdfcraft_ui_egui::RightPanel;

/// Answers one request with `text` as the model's reply; returns the endpoint.
fn mock(text: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let body = serde_json::json!({ "content": [{ "type": "text", "text": text }], "usage": { "input_tokens": 9, "output_tokens": 9 } }).to_string();
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else { return };
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut buf = vec![0; len];
        reader.read_exact(&mut buf).unwrap();
        let mut stream = stream;
        let _ =
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    });
    url
}

fn two_pages() -> Vec<u8> {
    Session::new().create_from_text("t", "Lasku 42\nSumma 120 euroa\u{c}Eräpäivä 1.12.2026").unwrap().to_vec()
}

fn harness(endpoint: Option<String>, key: &str) -> Harness<'static, PdfCraftApp> {
    let key = key.to_string();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("lasku.pdf", None, two_pages()).unwrap();
        app.run_inline = true;
        app.ai_key = key.clone();
        app.ai_endpoint = endpoint.clone();
        app
    });
    h.run_steps(4);
    h.state_mut().right = Some(RightPanel::Assistant);
    h.run_steps(2);
    h
}

#[test]
fn asking_shows_the_answer_and_its_page_buttons_go_to_the_page() {
    let url = mock("Eräpäivä on 1.12.2026 [s. 2].");
    let mut h = harness(Some(url), "sk-test");
    h.get_by_label("Summarize");
    h.state_mut().views[0].go_to_page(0);
    h.get_by_label("Question").click();
    h.run_steps(1);
    h.get_by_label("Question").type_text("Milloin eräpäivä on?");
    h.run_steps(1);
    h.get_by_label("Ask").click();
    h.run_steps(4);
    let shows = |h: &Harness<'static, PdfCraftApp>, text: &str| {
        h.query_all_by(|n| n.label().is_some_and(|l| l == text) || n.value().is_some_and(|v| v == text)).next().is_some()
    };
    assert!(shows(&h, "Eräpäivä on 1.12.2026 [s. 2]."), "the answer is shown");
    assert!(shows(&h, "Milloin eräpäivä on?"), "the question is shown");
    h.get_by_label("p. 2").click();
    h.run_steps(3);
    assert_eq!(h.state().views[0].current, 1, "the page button goes to page 2");
}

#[test]
fn without_a_key_the_panel_offers_setting_it_up_and_sends_nothing() {
    if std::env::var_os("ANTHROPIC_API_KEY").is_some() {
        eprintln!("skipped: ANTHROPIC_API_KEY is set in this environment");
        return;
    }
    let mut h = harness(None, "");
    h.get_by_label("Set up the AI Assistant…").click();
    h.run_steps(3);
    assert!(h.query_by_label("Anthropic API key").is_some(), "Preferences opened at the AI Assistant section");
    assert!(h.query_by_label("Summarize").is_none());
}

#[test]
fn a_refused_request_shows_the_error_in_the_panel() {
    // Plain HTTP to another computer is refused before anything is sent.
    let mut h = harness(Some("http://example.com".into()), "sk-test");
    h.get_by_label("Summarize").click();
    h.run_steps(4);
    let shown = h.query_all_by(|n| n.label().is_some_and(|l| l.contains("https://")) || n.value().is_some_and(|v| v.contains("https://"))).count();
    assert!(shown > 0, "the endpoint error is shown");
}

#[test]
fn errors_with_details_are_shown_in_the_interface_language() {
    // Nothing listens on this port once the listener is dropped: the request fails to connect.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut h = harness(Some(format!("http://127.0.0.1:{port}")), "sk-test");
    h.state_mut().set_option("language", "fi").unwrap();
    h.run_steps(2);
    h.get_by_label("Tee yhteenveto").click();
    h.run_steps(4);
    let shown = |h: &Harness<'static, PdfCraftApp>, s: &str| {
        h.query_all_by(|n| n.label().is_some_and(|l| l.contains(s)) || n.value().is_some_and(|v| v.contains(s))).count()
    };
    assert!(shown(&h, "palveluun ei saatu yhteyttä:") > 0, "the network error is in Finnish");
    assert_eq!(shown(&h, "could not reach"), 0);
}

#[test]
fn create_pdf_from_an_office_file_opens_a_new_tab() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-office-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let rtf = dir.join("Tarjous.rtf");
    std::fs::write(&rtf, br"{\rtf1\ansi\deff0{\fonttbl{\f0 Helvetica;}}\f0\fs24 Tarjous 7: 950 euroa\par}").unwrap();
    let mut app = PdfCraftApp::new();
    app.set_option("language", "en").unwrap();
    app.run_inline = true;
    app.convert_office_file(rtf);
    if pdfcraft_engine::office::find_office().is_none() {
        let toast = app.toast.clone().unwrap().0;
        assert!(toast.contains("LibreOffice"), "{toast}");
        return;
    }
    assert_eq!(app.views.len(), 1, "{:?}", app.toast);
    let id = app.active_ids().unwrap().1;
    assert_eq!(app.session.get(id).unwrap().name, "Tarjous.pdf");
}
