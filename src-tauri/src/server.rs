//! Server mode: the app's commands over loopback HTTP, for scripts and agents.
//!
//! A fork build that finds `fork-server.json` in the app's data directory
//! listens on `127.0.0.1:<port>` and answers:
//!
//! - `GET /version`: `{"version":"1.10.0","fork":true}`.
//! - `POST /invoke/<command>` with a JSON object of the command's arguments
//!   in camelCase, the same names the interface passes. The answer is the
//!   command's own result as JSON, or HTTP 422 with its error
//!   (`{"kind":…,"message":…}`).
//!
//! A request runs the same registered command the window runs, through the
//! main webview, so an edit made here takes the one edit path: Library
//! Protection and the read-only refusals apply, and the window hears
//! `library:changed` and redraws.
//!
//! # Why every request carries the token
//!
//! Loopback is not private to this app: a page in any browser on the machine
//! can post to `127.0.0.1`. The `Authorization` header is one a page cannot
//! send cross-origin without a preflight this server never answers, and the
//! token is one the page does not have.
//!
//! # What is not served
//!
//! Plugin commands (dialogs, the opener, the updater) and the update
//! commands. They open windows, launch programs or replace the app, and
//! nothing a library script needs is among them.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::ipc::{CallbackFn, InvokeBody, InvokeError, InvokeResponse, InvokeResponseBody};
use tauri::webview::InvokeRequest;
use tauri::{AppHandle, Manager};

const CONFIG_FILE: &str = "fork-server.json";
const MIN_TOKEN_LEN: usize = 24;
const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// A full-library import or analysis pass is one command, so this is long.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(600);

const REFUSED_COMMANDS: &[&str] =
    &["test_eval_result", "check_for_update", "ready_update", "download_update", "restart_to_update"];

#[derive(Deserialize)]
struct Config {
    port: u16,
    token: String,
}

fn config_path() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("rbxport").join(CONFIG_FILE))
}

fn config() -> Option<Config> {
    let path = config_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<Config>(&text) {
        Ok(config) if config.token.len() >= MIN_TOKEN_LEN => Some(config),
        Ok(_) => {
            tracing::error!(path = %path.display(), "server mode is off: the token is shorter than {MIN_TOKEN_LEN} characters");
            None
        }
        Err(e) => {
            tracing::error!(path = %path.display(), error = %e, "server mode is off: its config could not be read");
            None
        }
    }
}

/// Starts listening, on a thread of its own, in a fork build that is
/// configured to.
pub fn start(app: &AppHandle) {
    if !crate::fork::BUILD {
        return;
    }
    let Some(config) = config() else { return };
    let listener = match TcpListener::bind(("127.0.0.1", config.port)) {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!(port = config.port, error = %e, "server mode could not open its port");
            return;
        }
    };
    tracing::info!(port = config.port, "server mode listening on loopback");
    let app = app.clone();
    let version = app.package_info().version.to_string();
    let spawned = std::thread::Builder::new().name("server-mode".into()).spawn(move || {
        serve(&listener, &config.token, &version, move |command, args| run_command(&app, command, args));
    });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "server mode's thread could not start");
    }
}

/// What a command answered: its JSON result, or the error it rejected with.
type Outcome = Result<Value, Reply>;

/// One HTTP answer.
#[derive(Debug, PartialEq)]
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    fn new(status: u16, message: &str) -> Self {
        Self { status, body: json!({ "kind": "server", "message": message }) }
    }
}

fn serve<F>(listener: &TcpListener, token: &str, version: &str, run: F)
where
    F: Fn(&str, Value) -> Outcome + Clone + Send + 'static,
{
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (token, version, run) = (token.to_owned(), version.to_owned(), run.clone());
        let spawned = std::thread::Builder::new().name("server-mode-client".into()).spawn(move || {
            if let Err(e) = session(stream, &token, &version, &run) {
                tracing::debug!(error = %e, "a server mode connection ended early");
            }
        });
        if let Err(e) = spawned {
            tracing::error!(error = %e, "a server mode connection could not be served");
        }
    }
}

/// One request per connection: read it, answer it, hang up.
fn session(stream: TcpStream, token: &str, version: &str, run: &dyn Fn(&str, Value) -> Outcome) -> std::io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let reply = match read_request(&mut BufReader::new(stream)) {
        Ok(request) => answer(&request, token, version, run),
        Err(reply) => reply,
    };
    let body = reply.body.to_string();
    write!(
        writer,
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        reply.status,
        reason(reply.status),
        body.len(),
        body
    )?;
    writer.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        504 => "Gateway Timeout",
        _ => "Internal Server Error",
    }
}

#[derive(Debug, PartialEq)]
struct Request {
    method: String,
    path: String,
    authorization: Option<String>,
    body: Vec<u8>,
}

fn read_request(reader: &mut impl BufRead) -> Result<Request, Reply> {
    let bad = |_| Reply::new(400, "The request could not be read.");
    let mut head = reader.take(MAX_HEAD_BYTES as u64);
    let mut line = String::new();
    head.read_line(&mut line).map_err(bad)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return Err(Reply::new(400, "The request line is not HTTP."));
    };
    let (method, path) = (method.to_owned(), path.to_owned());
    let mut authorization = None;
    let mut length = 0_usize;
    loop {
        line.clear();
        if head.read_line(&mut line).map_err(bad)? == 0 {
            return Err(Reply::new(400, "The request headers are incomplete or too long."));
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else { continue };
        let value = value.trim();
        if name.eq_ignore_ascii_case("authorization") {
            authorization = Some(value.to_owned());
        } else if name.eq_ignore_ascii_case("content-length") {
            length = value.parse().map_err(|_| Reply::new(400, "Content-Length is not a number."))?;
        }
    }
    if length > MAX_BODY_BYTES {
        return Err(Reply::new(413, "The request body is larger than 16 MiB."));
    }
    let mut body = vec![0; length];
    head.into_inner().read_exact(&mut body).map_err(bad)?;
    Ok(Request { method, path, authorization, body })
}

fn answer(request: &Request, token: &str, version: &str, run: &dyn Fn(&str, Value) -> Outcome) -> Reply {
    let presented = request.authorization.as_deref().and_then(|value| value.strip_prefix("Bearer "));
    if presented != Some(token) {
        return Reply::new(401, "Send the token from fork-server.json as 'Authorization: Bearer <token>'.");
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/version") => Reply { status: 200, body: json!({ "version": version, "fork": true }) },
        ("POST", path) => match path.strip_prefix("/invoke/") {
            Some(command) if !command.is_empty() => invoke(command, &request.body, run),
            _ => Reply::new(404, "Post to /invoke/<command>."),
        },
        _ => Reply::new(404, "Use GET /version or POST /invoke/<command>."),
    }
}

fn invoke(command: &str, body: &[u8], run: &dyn Fn(&str, Value) -> Outcome) -> Reply {
    if command.starts_with("plugin:") || REFUSED_COMMANDS.contains(&command) {
        return Reply::new(403, "Server mode does not serve this command.");
    }
    let args = if body.is_empty() { Ok(json!({})) } else { serde_json::from_slice::<Value>(body) };
    match args {
        Ok(args) if args.is_object() => match run(command, args) {
            Ok(value) => Reply { status: 200, body: value },
            Err(reply) => reply,
        },
        _ => Reply::new(400, "The body must be a JSON object of the command's arguments."),
    }
}

/// Runs one registered command through the main webview and waits for it.
fn run_command<R: tauri::Runtime>(app: &AppHandle<R>, command: &str, args: Value) -> Outcome {
    let window = app
        .get_webview_window(crate::MAIN_WINDOW)
        .ok_or_else(|| Reply::new(500, "The main window is not open."))?;
    let url = window.url().map_err(|e| Reply::new(500, &format!("The main window has no address: {e}")))?;
    let request = InvokeRequest {
        cmd: command.to_owned(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url,
        body: InvokeBody::Json(args),
        headers: tauri::http::HeaderMap::default(),
        invoke_key: app.invoke_key().to_owned(),
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    window.as_ref().clone().on_message(
        request,
        Box::new(move |_webview, _command, response, _callback, _error| {
            // The receiver is gone only after the timeout below answered.
            let _ = sender.send(response);
        }),
    );
    match receiver.recv_timeout(COMMAND_TIMEOUT) {
        Ok(InvokeResponse::Ok(InvokeResponseBody::Json(text))) => {
            serde_json::from_str(&text).map_err(|e| Reply::new(500, &format!("The command's answer was not JSON: {e}")))
        }
        Ok(InvokeResponse::Ok(InvokeResponseBody::Raw(bytes))) => Ok(json!({ "rawBytes": bytes.len() })),
        Ok(InvokeResponse::Err(InvokeError(error))) => Err(Reply { status: 422, body: error }),
        Err(_) => Err(Reply::new(504, "The command did not answer in ten minutes.")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::needless_pass_by_value)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef01234567";

    /// A stand-in for the app: echoes what it was asked to run.
    fn echo(command: &str, args: Value) -> Outcome {
        if command == "fails" {
            return Err(Reply { status: 422, body: json!({ "kind": "notFound", "message": "No such track." }) });
        }
        Ok(json!({ "command": command, "args": args }))
    }

    /// Sends raw bytes to a live listener and returns the whole response.
    fn exchange(raw: &str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || serve(&listener, TOKEN, "1.10.0", echo));
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(raw.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    fn body_of(response: &str) -> Value {
        serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
    }

    #[tauri::command]
    fn greet(name: String) -> String {
        format!("hello {name}")
    }

    #[tauri::command]
    fn refuse() -> crate::AppResult<()> {
        Err(crate::AppError::new(crate::ErrorKind::Internal, "No."))
    }

    fn app() -> tauri::App<tauri::test::MockRuntime> {
        let app = tauri::test::mock_builder()
            .invoke_handler(tauri::generate_handler![greet, refuse])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        tauri::WebviewWindowBuilder::new(&app, crate::MAIN_WINDOW, tauri::WebviewUrl::default()).build().unwrap();
        app
    }

    #[test]
    fn a_registered_command_runs_and_answers() {
        let app = app();
        assert_eq!(run_command(app.handle(), "greet", json!({ "name": "T7" })), Ok(json!("hello T7")));
    }

    #[test]
    fn a_registered_commands_error_comes_back_as_422() {
        let app = app();
        let reply = run_command(app.handle(), "refuse", json!({})).unwrap_err();
        assert_eq!(reply.status, 422);
        assert_eq!(reply.body["message"], json!("No."));
    }

    #[test]
    fn an_unregistered_command_is_an_error_not_a_hang() {
        let app = app();
        assert_eq!(run_command(app.handle(), "no_such_command", json!({})).unwrap_err().status, 422);
    }

    #[test]
    fn version_answers_with_the_token() {
        let response = exchange(&format!("GET /version HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\n\r\n"));
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
        assert_eq!(body_of(&response), json!({ "version": "1.10.0", "fork": true }));
    }

    #[test]
    fn a_request_without_the_token_is_refused() {
        let missing = exchange("GET /version HTTP/1.1\r\n\r\n");
        assert!(missing.starts_with("HTTP/1.1 401 "), "{missing}");
        let wrong = exchange("POST /invoke/set_track_field HTTP/1.1\r\nAuthorization: Bearer nope\r\nContent-Length: 2\r\n\r\n{}");
        assert!(wrong.starts_with("HTTP/1.1 401 "), "{wrong}");
    }

    #[test]
    fn invoke_passes_the_command_and_its_arguments() {
        let body = r#"{"tracks":["12"],"field":"genre","value":"Electro"}"#;
        let response = exchange(&format!(
            "POST /invoke/set_track_field HTTP/1.1\r\nauthorization: Bearer {TOKEN}\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        ));
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
        assert_eq!(
            body_of(&response),
            json!({ "command": "set_track_field", "args": { "tracks": ["12"], "field": "genre", "value": "Electro" } })
        );
    }

    #[test]
    fn an_empty_body_is_an_empty_argument_object() {
        let response = exchange(&format!("POST /invoke/library_summary HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\n\r\n"));
        assert_eq!(body_of(&response), json!({ "command": "library_summary", "args": {} }));
    }

    #[test]
    fn a_command_error_is_a_422_with_the_commands_own_error() {
        let response = exchange(&format!("POST /invoke/fails HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\n\r\n"));
        assert!(response.starts_with("HTTP/1.1 422 "), "{response}");
        assert_eq!(body_of(&response), json!({ "kind": "notFound", "message": "No such track." }));
    }

    #[test]
    fn plugin_and_update_commands_are_not_served() {
        for command in ["plugin:opener|open_path", "download_update", "restart_to_update"] {
            let response = exchange(&format!("POST /invoke/{command} HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\n\r\n"));
            assert!(response.starts_with("HTTP/1.1 403 "), "{command}: {response}");
        }
    }

    #[test]
    fn a_body_that_is_not_an_object_is_refused() {
        let response =
            exchange(&format!("POST /invoke/x HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: 5\r\n\r\n[1,2]"));
        assert!(response.starts_with("HTTP/1.1 400 "), "{response}");
    }

    #[test]
    fn an_oversized_body_is_refused_before_it_is_read() {
        let response = exchange(&format!(
            "POST /invoke/x HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        ));
        assert!(response.starts_with("HTTP/1.1 413 "), "{response}");
    }
}
