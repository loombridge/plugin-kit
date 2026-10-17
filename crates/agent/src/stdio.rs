//! The stdio transport: JSON-RPC messages, one per line, on standard input,
//! and replies, one per line, on standard output. A program's command-line
//! tool serves the agent interface this way, with no window, on any
//! platform.

use std::io::{self, BufRead, Read, Write};

use serde_json::Value;

use crate::jsonrpc::{self, code};
use crate::server::{Outcome, Program, STDIO_SESSION, Server};

/// The longest message read, in bytes. A longer line is refused.
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;

/// Serves the agent interface on `input` and `output` until `input` ends.
///
/// Headless there's no person to answer a question, so a call that would
/// ask is refused by the agent's permissions before it gets here; one that
/// still does (a server not made headless) is declined.
///
/// # Errors
///
/// An I/O error reading `input` or writing `output`.
pub fn serve<P: Program>(
    server: &mut Server<P>,
    host: &mut P::Host,
    input: impl BufRead,
    output: impl Write,
) -> io::Result<()> {
    serve_until(server, host, input, output, |_| false)
}

/// Serves the agent interface as [`serve`] does, and stops after a reply
/// when `stop` says so, such as when an agent ran the program's quit
/// command.
///
/// # Errors
///
/// An I/O error reading `input` or writing `output`.
pub fn serve_until<P: Program>(
    server: &mut Server<P>,
    host: &mut P::Host,
    input: impl BufRead,
    mut output: impl Write,
    mut stop: impl FnMut(&mut P::Host) -> bool,
) -> io::Result<()> {
    server.open_session(Some(STDIO_SESSION));
    let mut lines = input;
    let mut line = String::new();
    loop {
        line.clear();
        let read = lines
            .by_ref()
            .take(MAX_MESSAGE as u64 + 1)
            .read_line(&mut line)?;
        if read == 0 {
            return Ok(());
        }
        if line.len() > MAX_MESSAGE && !line.ends_with('\n') {
            // Skip the rest of the line.
            let mut rest = Vec::new();
            lines.read_until(b'\n', &mut rest)?;
            write_line(
                &mut output,
                &jsonrpc::error_response(
                    Value::Null,
                    code::INVALID_REQUEST,
                    format!("a message can be at most {MAX_MESSAGE} bytes"),
                ),
            )?;
            continue;
        }
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(text) {
            Err(error) => Some(jsonrpc::error_response(
                Value::Null,
                code::PARSE_ERROR,
                format!("the message isn't JSON: {error}"),
            )),
            Ok(message) => match server.handle(host, STDIO_SESSION, message) {
                Outcome::Reply(reply) => reply,
                Outcome::Ask(question) => server.resolve(host, question.id, false),
                // Headless, no person's gesture is ever under way, so
                // nothing waits for one to end; run it now if anything did.
                // A call that waits for the host gets its answer first.
                Outcome::Wait(ticket) => {
                    if server.deferred().contains(&ticket)
                        && let Some(result) = server.program_mut().await_deferred(ticket)
                    {
                        server.finish_deferred(ticket, |_| result);
                    }
                    server
                        .resume(host)
                        .filter(|(resumed, _)| *resumed == ticket)
                        .and_then(|(_, outcome)| match outcome {
                            Outcome::Reply(reply) => reply,
                            Outcome::Ask(question) => server.resolve(host, question.id, false),
                            Outcome::Wait(_) => None,
                        })
                }
            },
        };
        if let Some(reply) = reply {
            write_line(&mut output, &reply)?;
        }
        if stop(host) {
            return Ok(());
        }
    }
}

fn write_line(output: &mut impl Write, message: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *output, message)?;
    output.write_all(b"\n")?;
    output.flush()
}
