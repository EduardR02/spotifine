use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;

use renderer_engine::protocol::{BrowseResponse, Request, MAX_REQUEST_BYTES};

#[derive(Debug)]
pub enum Input {
    Request(Request),
    Invalid { request_id: String, error: String },
    Eof,
}

#[derive(Clone)]
pub struct ProtocolWriter {
    inner: Arc<Mutex<BufWriter<Box<dyn Write + Send>>>>,
}

impl ProtocolWriter {
    pub fn capture_stdout() -> io::Result<Self> {
        let output = protocol_output()?;
        Ok(Self {
            inner: Arc::new(Mutex::new(BufWriter::new(output))),
        })
    }

    pub fn send<T: Serialize>(&self, value: &T) -> Result<(), String> {
        let mut output = self
            .inner
            .lock()
            .map_err(|_| "protocol output lock was poisoned".to_owned())?;
        serde_json::to_writer(&mut *output, value)
            .map_err(|error| format!("could not serialize protocol message: {error}"))?;
        output
            .write_all(b"\n")
            .and_then(|_| output.flush())
            .map_err(|error| format!("could not write protocol message: {error}"))
    }

    /// Sends a typed `browse_*` response: `data` carries the payload on
    /// success, error text only on failure. `kind` must match the command
    /// name so the UI can route the response.
    pub fn send_browse<T: Serialize>(
        &self,
        request_id: &str,
        kind: &'static str,
        result: &Result<T, String>,
    ) -> Result<(), String> {
        let (ok, error, data) = match result {
            Ok(data) => (true, None, Some(data)),
            Err(error) => (false, Some(error.as_str()), None),
        };
        self.send(&BrowseResponse { kind, request_id, ok, error, data })
    }

    /// Sends an `edit_*` response for a void edit: `ok`/`error` only, with no
    /// `data` payload on success (the UI routes these like browse responses
    /// but has nothing to parse).
    pub fn send_edit(
        &self,
        request_id: &str,
        kind: &'static str,
        result: &Result<(), String>,
    ) -> Result<(), String> {
        let (ok, error) = match result {
            Ok(()) => (true, None),
            Err(error) => (false, Some(error.as_str())),
        };
        self.send(&BrowseResponse::<()> { kind, request_id, ok, error, data: None })
    }
}

#[cfg(test)]
struct CaptureBuffer(Arc<Mutex<Vec<u8>>>);

#[cfg(test)]
impl Write for CaptureBuffer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("capture buffer lock was poisoned"))?
            .extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
impl ProtocolWriter {
    /// Creates a writer that appends serialized messages to a shared buffer so
    /// tests can inspect exactly what the engine would emit on stdout.
    pub fn capture() -> (Self, Arc<Mutex<Vec<u8>>>) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let writer: Box<dyn Write + Send> = Box::new(CaptureBuffer(Arc::clone(&buffer)));
        (
            Self {
                inner: Arc::new(Mutex::new(BufWriter::new(writer))),
            },
            buffer,
        )
    }
}

pub fn spawn_input_reader(sender: mpsc::UnboundedSender<Input>) {
    std::thread::Builder::new()
        .name("protocol-input".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(io::stdin().lock());
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        let _ = sender.send(Input::Eof);
                        break;
                    }
                    Ok(_) if line.trim().is_empty() => continue,
                    Ok(_) if line.len() > MAX_REQUEST_BYTES => {
                        if sender
                            .send(Input::Invalid {
                                request_id: String::new(),
                                error: format!(
                                    "request exceeds the {MAX_REQUEST_BYTES}-byte protocol limit"
                                ),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(_) => {
                        let input = decode_request(line.trim_end());
                        if sender.send(input).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Input::Invalid {
                            request_id: String::new(),
                            error: format!("could not read stdin: {error}"),
                        });
                        let _ = sender.send(Input::Eof);
                        break;
                    }
                }
            }
        })
        .expect("could not start protocol input thread");
}

fn decode_request(line: &str) -> Input {
    match serde_json::from_str::<Request>(line) {
        Ok(request) if request.request_id.is_empty() => Input::Invalid {
            request_id: String::new(),
            error: "request_id must not be empty".to_owned(),
        },
        Ok(request) => Input::Request(request),
        Err(_) => {
            // Valid requests take the direct typed path above, avoiding a
            // complete Value tree plus a second traversal. On failure, parse
            // through Value exactly as before so malformed JSON diagnostics
            // and request-id recovery stay byte-for-byte compatible.
            let value: Value = match serde_json::from_str(line) {
                Ok(value) => value,
                Err(error) => {
                    return Input::Invalid {
                        request_id: String::new(),
                        error: format!("invalid JSON: {error}"),
                    };
                }
            };
            let request_id = value
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            match serde_json::from_value::<Request>(value) {
                Ok(request) if request.request_id.is_empty() => Input::Invalid {
                    request_id,
                    error: "request_id must not be empty".to_owned(),
                },
                Ok(request) => Input::Request(request),
                Err(error) => Input::Invalid {
                    request_id,
                    error: format!("invalid request: {error}"),
                },
            }
        }
    }
}

#[cfg(windows)]
fn protocol_output() -> io::Result<Box<dyn Write + Send>> {
    use std::fs::File;
    use std::os::windows::io::{FromRawHandle, RawHandle};

    const STD_OUTPUT_HANDLE: u32 = -11_i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12_i32 as u32;
    const DUPLICATE_SAME_ACCESS: u32 = 2;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(kind: u32) -> isize;
        fn SetStdHandle(kind: u32, handle: isize) -> i32;
        fn GetCurrentProcess() -> isize;
        fn DuplicateHandle(
            source_process: isize,
            source_handle: isize,
            target_process: isize,
            target_handle: *mut isize,
            desired_access: u32,
            inherit_handle: i32,
            options: u32,
        ) -> i32;
    }

    let mut duplicate = 0_isize;
    unsafe {
        let process = GetCurrentProcess();
        let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        if stdout == 0 || stdout == -1 {
            return Err(io::Error::last_os_error());
        }
        if DuplicateHandle(
            process,
            stdout,
            process,
            &mut duplicate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let output = File::from_raw_handle(duplicate as RawHandle);

        // librespot-oauth and the rodio device lister contain println! calls. Keep
        // their text out of the protocol pipe while retaining our private duplicate.
        let stderr = GetStdHandle(STD_ERROR_HANDLE);
        if stderr == 0 || stderr == -1 || SetStdHandle(STD_OUTPUT_HANDLE, stderr) == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Box::new(output))
    }
}

#[cfg(not(windows))]
fn protocol_output() -> io::Result<Box<dyn Write + Send>> {
    Ok(Box::new(io::stdout()))
}

#[cfg(test)]
mod tests {
    use super::{Input, decode_request};
    use renderer_engine::protocol::Command;

    #[test]
    fn valid_requests_decode_directly() {
        match decode_request(r#"{"request_id":"request-7","type":"status"}"#) {
            Input::Request(request) => {
                assert_eq!(request.request_id, "request-7");
                assert!(matches!(request.command, Command::Status));
            }
            other => panic!("expected a valid request, got {other:?}"),
        }
    }

    #[test]
    fn invalid_requests_preserve_the_recoverable_request_id() {
        match decode_request(r#"{"request_id":"request-8","type":"seek"}"#) {
            Input::Invalid { request_id, error } => {
                assert_eq!(request_id, "request-8");
                assert!(error.contains("position_ms"));
            }
            other => panic!("expected an invalid request, got {other:?}"),
        }
    }
}
