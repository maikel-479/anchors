//! Stdio protocol tests for the LSP server.
//!
//! Spawns the real `canc` binary, runs the LSP startup flow
//! (initialize -> initialized -> didOpen -> semanticTokens/full),
//! then applies edits (FULL and incremental didChange) and asserts
//! the server's delta-decoded semantic tokens match the tags' true
//! positions in the NEW text — computed independently by searching the
//! new text for the tag substrings, not by reusing the server's scan.
//! This guards the `5<` dedent scenario where Zed showed colours pinned
//! to pre-edit columns.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::Duration;

const RECV_TIMEOUT: Duration = Duration::from_secs(10);

static DIR_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

struct TestClient {
    child: Child,
    stdin: std::process::ChildStdin,
    rx: Receiver<Value>,
    next_id: i64,
    diagnostics_count: usize,
    refresh_count: usize,
    _root_dir: PathBuf,
    uri: String,
}

impl TestClient {
    fn spawn(name: &str) -> Self {
        let bin = std::env::var("CARGO_BIN_EXE_canc")
            .unwrap_or_else(|_| "target/debug/canc".to_string());
        let counter = DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let root_dir = std::env::temp_dir().join(format!(
            "canc-srv-test-{}-{}-{}",
            name,
            std::process::id(),
            counter
        ));
        let _ = std::fs::create_dir_all(&root_dir);
        let uri = format!("file://{}/test.rs", root_dir.display());
        let mut child = Command::new(bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn canc server");
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let (tx, rx) = channel::<Value>();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut content_length: Option<usize> = None;
                // Read headers until empty line.
                loop {
                    let mut header = String::new();
                    match reader.read_line(&mut header) {
                        Ok(0) => return, // EOF
                        Ok(_) => {}
                        Err(_) => return,
                    }
                    let trimmed = header.trim_end_matches(['\r', '\n']);
                    if trimmed.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = trimmed.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("Content-Length") {
                            content_length = v.trim().parse().ok();
                        }
                    }
                }
                let len = match content_length {
                    Some(n) => n,
                    None => return,
                };
                let mut buf = vec![0u8; len];
                if reader.read_exact(&mut buf).is_err() {
                    return;
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if tx.send(value).is_err() {
                    return;
                }
            }
        });
        let mut client = Self {
            child,
            stdin,
            rx,
            next_id: 1,
            diagnostics_count: 0,
            refresh_count: 0,
            _root_dir: root_dir,
            uri,
        };
        client.initialize();
        client
    }

    fn send(&mut self, msg: Value) {
        let body = serde_json::to_string(&msg).expect("serialize msg");
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body)
            .expect("write to server stdin");
        self.stdin.flush().expect("flush");
    }

    fn send_notification(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn handle_incoming(&mut self, msg: Value) {
        // Server -> client request: has both method and id.
        if let (Some(method), Some(id)) = (msg.get("method").and_then(|m| m.as_str()), msg.get("id")) {
            let method = method.to_string();
            if method == "workspace/semanticTokens/refresh" {
                self.refresh_count += 1;
            }
            let resp = json!({"jsonrpc": "2.0", "id": id, "result": null});
            self.send(resp);
            return;
        }
        // Server -> client notification.
        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            if method == "textDocument/publishDiagnostics" {
                self.diagnostics_count += 1;
            }
        }
    }

    fn recv_response(&mut self, expect_id: i64) -> Value {
        loop {
            match self.rx.recv_timeout(RECV_TIMEOUT) {
                Ok(msg) => {
                    if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
                        if id == expect_id {
                            return msg;
                        }
                    }
                    // Server->client request or notification, or unrelated response.
                    if msg.get("method").is_some() {
                        self.handle_incoming(msg);
                    }
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => panic!("timeout waiting for response id {expect_id}"),
                Err(RecvTimeoutError::Disconnected) => panic!("server closed stdout"),
            }
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.recv_response(id)
    }

    fn initialize(&mut self) {
        let root_uri = format!("file://{}", self._root_dir.display());
        let resp = self.request(
            "initialize",
            json!({
                "processId": null,
                "rootUri": root_uri,
                "capabilities": {},
                "initializationOptions": {}
            }),
        );
        assert!(resp.get("result").is_some(), "initialize result missing: {resp}");
        self.send_notification("initialized", json!({}));
    }

    fn did_open(&mut self, text: &str) {
        let before = self.diagnostics_count;
        self.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": self.uri,
                    "languageId": "rust",
                    "version": 1,
                    "text": text
                }
            }),
        );
        self.wait_for_diagnostics(before);
    }

    fn did_change_full(&mut self, text: &str) {
        let before = self.diagnostics_count;
        self.send_notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": self.uri, "version": 2},
                "contentChanges": [{"text": text}]
            }),
        );
        self.wait_for_diagnostics(before);
    }

    fn did_change_incremental(&mut self, changes: Value) {
        let before = self.diagnostics_count;
        self.send_notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": self.uri, "version": 3},
                "contentChanges": changes
            }),
        );
        self.wait_for_diagnostics(before);
    }

    /// Wait for the server to process a text-change notification.
    /// The server always publishes diagnostics after didOpen/didChange,
    /// so a diagnostics notification is a reliable processing barrier.
    fn wait_for_diagnostics(&mut self, before: usize) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if self.diagnostics_count > before {
                return;
            }
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(msg) => {
                    if msg.get("method").is_some() {
                        self.handle_incoming(msg);
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("server closed stdout while waiting for diagnostics")
                }
            }
        }
        panic!(
            "timed out waiting for publishDiagnostics (before={before}, now={})",
            self.diagnostics_count
        );
    }

    fn semantic_tokens(&mut self) -> Vec<(u32, u32, u32, u32)> {
        let resp = self.request(
            "textDocument/semanticTokens/full",
            json!({"textDocument": {"uri": self.uri}}),
        );
        let data = resp
            .get("result")
            .and_then(|r| r.get("data"))
            .and_then(|d| d.as_array())
            .expect("semantic tokens data array");
        let data: Vec<u32> = data
            .iter()
            .map(|v| v.as_u64().expect("token u32") as u32)
            .collect();
        decode_tokens(&data)
    }
}

impl Drop for TestClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self._root_dir);
    }
}

/// Decode LSP delta-encoded semantic tokens into absolute
/// (line, char, length, type) tuples.
fn decode_tokens(data: &[u32]) -> Vec<(u32, u32, u32, u32)> {
    assert_eq!(data.len() % 5, 0, "token data len must be multiple of 5: {data:?}");
    let mut out = Vec::new();
    let mut line = 0u32;
    let mut col = 0u32;
    for chunk in data.chunks_exact(5) {
        let delta_line = chunk[0];
        let delta_start = chunk[1];
        let length = chunk[2];
        let token_type = chunk[3];
        // chunk[4] is modifier bitset (ignored for position checks)
        line += delta_line;
        if delta_line == 0 {
            col += delta_start;
        } else {
            col = delta_start;
        }
        out.push((line, col, length, token_type));
    }
    out
}

/// Independently compute expected token positions by searching each line
/// of `text` for the literal substrings. All test text is ASCII, so the
/// byte offset within a line equals the LSP UTF-16 column (tabs are one
/// UTF-16 unit each, matching the 5-char shift of a `5<` dedent).
fn expected_independent(text: &str) -> Vec<(u32, u32, u32, u32)> {
    // (needle, token_type) — type 0 = tag (anchors), 1 = inline span.
    let needles: &[(&str, u32)] = &[
        ("TODO:", 0),
        ("NOTE:", 0),
        ("**block**", 1),
        ("`code span`", 1),
    ];
    let mut out = Vec::new();
    for (line_idx, line) in text.split('\n').enumerate() {
        for (needle, ty) in needles {
            if let Some(byte_idx) = line.find(needle) {
                out.push((
                    line_idx as u32,
                    byte_idx as u32,
                    needle.len() as u32,
                    *ty,
                ));
            }
        }
    }
    out.sort_by_key(|t| (t.0, t.1));
    out
}

fn assert_tokens_match_text(actual: &[(u32, u32, u32, u32)], text: &str, context: &str) {
    let expected = expected_independent(text);
    assert_eq!(
        actual,
        expected,
        "{context}: token (line,char,len,type) mismatch\ntext:\n{text}\nexpected: {expected:?}\nactual:   {actual:?}"
    );
    // Sanity: every actual token must cover its needle verbatim in the new text.
    for (line, col, len, _ty) in actual {
        let line_text = text.split('\n').nth(*line as usize).expect("line exists");
        let slice = &line_text[*col as usize..*col as usize + *len as usize];
        let ok = ["TODO:", "NOTE:", "**block**", "`code span`"].contains(&slice);
        assert!(ok, "{context}: token covers {slice:?} at line {line} col {col}, not a tag/span");
    }
}

const INDENTED: &str = "\t\t\t\t\tlet x = 1; // TODO: inline todo after code\n\t\t\t\t\t/* NOTE: **block** note */\n\t\t\t\t\t// `code span`\n";
const DEDENTED: &str = "let x = 1; // TODO: inline todo after code\n/* NOTE: **block** note */\n// `code span`\n";

#[test]
fn full_dedent_5tabs_keeps_offsets_correct() {
    let mut client = TestClient::spawn("full");
    client.did_open(INDENTED);

    let before = client.semantic_tokens();
    assert_tokens_match_text(&before, INDENTED, "initial (indented)");

    client.did_change_full(DEDENTED);
    let after = client.semantic_tokens();
    assert_tokens_match_text(&after, DEDENTED, "after FULL didChange dedent");

    // Explicit 5-char shift check: every token keeps its line but moves -5 cols.
    assert_eq!(before.len(), after.len(), "token count changed across FULL dedent");
    for (b, a) in before.iter().zip(after.iter()) {
        assert_eq!(b.0, a.0, "line must not change: before {b:?} after {a:?}");
        assert_eq!(a.1 + 5, b.1, "col must shift by 5: before {b:?} after {a:?}");
        assert_eq!(b.2, a.2, "length must not change: before {b:?} after {a:?}");
        assert_eq!(b.3, a.3, "type must not change: before {b:?} after {a:?}");
    }
    // No stale/pre-edit positions remain.
    let stale: HashSet<_> = before.iter().collect();
    for t in &after {
        assert!(!stale.contains(t), "stale pre-edit token still present: {t:?}");
    }
}

#[test]
fn incremental_dedent_5tabs_keeps_offsets_correct() {
    let mut client = TestClient::spawn("incr");
    client.did_open(INDENTED);
    let before = client.semantic_tokens();
    assert_tokens_match_text(&before, INDENTED, "initial (indented)");

    // Model Zed's incremental `5<`: remove 5 leading chars (tabs) per line.
    let changes = json!([
        {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 5}}, "text": ""},
        {"range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 5}}, "text": ""},
        {"range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 5}}, "text": ""}
    ]);
    client.did_change_incremental(changes);

    let after = client.semantic_tokens();
    assert_tokens_match_text(&after, DEDENTED, "after incremental dedent");

    assert_eq!(before.len(), after.len());
    for (b, a) in before.iter().zip(after.iter()) {
        assert_eq!(b.0, a.0, "line changed: before {b:?} after {a:?}");
        assert_eq!(a.1 + 5, b.1, "col shift != 5: before {b:?} after {a:?}");
        assert_eq!(b.2, a.2);
        assert_eq!(b.3, a.3);
    }
}

#[test]
fn insert_before_tags_shifts_tokens_exactly() {
    let mut client = TestClient::spawn("shift");
    client.did_open(DEDENTED);
    let before = client.semantic_tokens();
    assert_tokens_match_text(&before, DEDENTED, "initial (dedented)");

    // Insert 2 spaces before the TODO on the same line: only that line's
    // tokens shift (+2), later lines unaffected.
    let changes = json!([
        {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "text": "  "}
    ]);
    client.did_change_incremental(changes);
    let with_prefix = "  let x = 1; // TODO: inline todo after code\n/* NOTE: **block** note */\n// `code span`\n";
    let after_prefix = client.semantic_tokens();
    assert_tokens_match_text(&after_prefix, with_prefix, "after same-line prefix insert");
    for (b, a) in before.iter().zip(after_prefix.iter()) {
        if b.0 == 0 {
            assert_eq!(a.1, b.1 + 2, "line-0 token must shift +2: before {b:?} after {a:?}");
        } else {
            assert_eq!(b, a, "later-line token must not move: before {b:?} after {a:?}");
        }
    }

    // Insert a whole new line at the top: every token's line shifts +1.
    let changes2 = json!([
        {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "text": "// hi\n"}
    ]);
    client.did_change_incremental(changes2);
    let with_line = "// hi\n  let x = 1; // TODO: inline todo after code\n/* NOTE: **block** note */\n// `code span`\n";
    let after_line = client.semantic_tokens();
    assert_tokens_match_text(&after_line, with_line, "after new-line insert");
    for (b, a) in after_prefix.iter().zip(after_line.iter()) {
        assert_eq!(a.0, b.0 + 1, "line must shift +1: before {b:?} after {a:?}");
        assert_eq!(a.1, b.1, "col must not change on new-line insert: before {b:?} after {a:?}");
        assert_eq!(a.2, b.2);
        assert_eq!(a.3, b.3);
    }
}

#[test]
fn no_workspace_semantic_tokens_refresh_spam() {
    // workspace/semanticTokens/refresh forces Zed to re-decode ALL servers'
    // cached raw tokens against the current snapshot. A secondary server
    // doing this on every edit can misalign a primary server's (e.g. clangd)
    // highlights if its raw data is for an older buffer state. The server
    // must not push refresh; Zed re-requests tokens via its own pull cycle.
    let mut client = TestClient::spawn("refresh");
    client.did_open(INDENTED);
    client.did_change_full(DEDENTED);
    client.did_change_incremental(json!([
        {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "text": " "}
    ]));
    // Drain any stray server->client requests briefly.
    let deadline = std::time::Instant::now() + Duration::from_millis(300);
    while std::time::Instant::now() < deadline {
        match client.rx.recv_timeout(Duration::from_millis(50)) {
            Ok(msg) => {
                if msg.get("method").is_some() {
                    client.handle_incoming(msg);
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    assert_eq!(
        client.refresh_count, 0,
        "server must not send workspace/semanticTokens/refresh"
    );
}
