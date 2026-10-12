//! Just enough of the D-Bus wire protocol to call rtkit: connect to a Unix
//! socket, authenticate with EXTERNAL, send method calls with simple
//! arguments, and read replies.
//!
//! A full D-Bus library would mean either `libdbus` (a C library every Linux
//! build would need headers for) or an async stack far larger than the three
//! calls rtkit needs. The protocol is specified at
//! <https://dbus.freedesktop.org/doc/dbus-specification.html>.
//!
//! Built on Linux, and on every Unix for its tests, which talk to a fake bus.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

const METHOD_CALL: u8 = 1;
const METHOD_RETURN: u8 = 2;
const ERROR: u8 = 3;

const FIELD_PATH: u8 = 1;
const FIELD_INTERFACE: u8 = 2;
const FIELD_MEMBER: u8 = 3;
const FIELD_ERROR_NAME: u8 = 4;
const FIELD_REPLY_SERIAL: u8 = 5;
const FIELD_DESTINATION: u8 = 6;
const FIELD_SIGNATURE: u8 = 8;

/// Messages larger than this are refused rather than read.
const MAX_MESSAGE: usize = 1 << 20;

/// Why a call failed.
#[derive(Debug)]
pub(crate) enum CallError {
    /// The socket failed.
    Io(io::Error),
    /// The other side replied with a D-Bus error.
    Remote {
        /// The error's name, such as `org.freedesktop.DBus.Error.ServiceUnknown`.
        name: String,
        /// Its message, if it sent one.
        message: String,
    },
    /// Something didn't follow the protocol.
    Protocol(&'static str),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "D-Bus socket error: {error}"),
            Self::Remote { name, message } => write!(f, "{name}: {message}"),
            Self::Protocol(what) => write!(f, "D-Bus protocol error: {what}"),
        }
    }
}

impl From<io::Error> for CallError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Builds a little-endian D-Bus message. Alignment counts from the start of
/// the buffer, which is the start of the message or of its body; both are
/// 8-byte aligned in the message, so the padding comes out the same.
#[derive(Debug, Default)]
pub(crate) struct Writer {
    pub(crate) buf: Vec<u8>,
}

impl Writer {
    fn pad(&mut self, align: usize) {
        while !self.buf.len().is_multiple_of(align) {
            self.buf.push(0);
        }
    }

    pub(crate) fn byte(&mut self, value: u8) {
        self.buf.push(value);
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.pad(4);
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    #[cfg(test)]
    pub(crate) fn i32(&mut self, value: i32) {
        self.pad(4);
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.pad(8);
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    #[cfg(test)]
    pub(crate) fn i64(&mut self, value: i64) {
        self.pad(8);
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    /// A string or object path.
    pub(crate) fn string(&mut self, value: &str) {
        self.u32(u32::try_from(value.len()).unwrap_or(u32::MAX));
        self.buf.extend_from_slice(value.as_bytes());
        self.buf.push(0);
    }

    pub(crate) fn signature(&mut self, value: &str) {
        self.byte(u8::try_from(value.len()).unwrap_or(u8::MAX));
        self.buf.extend_from_slice(value.as_bytes());
        self.buf.push(0);
    }

    /// One header field: a struct of the field code and a variant holding a
    /// string (`s`), object path (`o`), signature (`g`), or `u32` (`u`).
    fn field(&mut self, code: u8, value: &FieldValue<'_>) {
        self.pad(8);
        self.byte(code);
        match value {
            FieldValue::Str(kind, text) => {
                self.signature(kind);
                if *kind == "g" {
                    self.signature(text);
                } else {
                    self.string(text);
                }
            }
            FieldValue::U32(number) => {
                self.signature("u");
                self.u32(*number);
            }
        }
    }
}

enum FieldValue<'a> {
    Str(&'static str, &'a str),
    U32(u32),
}

/// What a message is about, apart from its body.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) kind: u8,
    pub(crate) serial: u32,
    pub(crate) path: Option<String>,
    pub(crate) interface: Option<String>,
    pub(crate) member: Option<String>,
    pub(crate) destination: Option<String>,
    pub(crate) error_name: Option<String>,
    pub(crate) reply_serial: Option<u32>,
    pub(crate) signature: String,
}

/// Encodes a whole message: the header described by `header`, then `body`
/// (already marshalled, starting at an 8-byte boundary).
pub(crate) fn encode(header: &Header, body: &[u8]) -> Vec<u8> {
    let mut w = Writer::default();
    w.byte(b'l');
    w.byte(header.kind);
    w.byte(0); // flags
    w.byte(1); // protocol version
    w.u32(u32::try_from(body.len()).unwrap_or(u32::MAX));
    w.u32(header.serial);
    let length_at = w.buf.len();
    w.u32(0);
    w.pad(8);
    let fields_start = w.buf.len();
    let strings = [
        (FIELD_PATH, "o", &header.path),
        (FIELD_INTERFACE, "s", &header.interface),
        (FIELD_MEMBER, "s", &header.member),
        (FIELD_ERROR_NAME, "s", &header.error_name),
        (FIELD_DESTINATION, "s", &header.destination),
    ];
    for (code, kind, value) in strings {
        if let Some(value) = value {
            w.field(code, &FieldValue::Str(kind, value));
        }
    }
    if let Some(serial) = header.reply_serial {
        w.field(FIELD_REPLY_SERIAL, &FieldValue::U32(serial));
    }
    if !header.signature.is_empty() {
        w.field(FIELD_SIGNATURE, &FieldValue::Str("g", &header.signature));
    }
    let fields_len = u32::try_from(w.buf.len() - fields_start).unwrap_or(u32::MAX);
    w.buf[length_at..length_at + 4].copy_from_slice(&fields_len.to_le_bytes());
    w.pad(8);
    w.buf.extend_from_slice(body);
    w.buf
}

/// Reads D-Bus values from a buffer in either byte order.
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    big_endian: bool,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8], big_endian: bool) -> Self {
        Self {
            buf,
            pos: 0,
            big_endian,
        }
    }

    fn at(buf: &'a [u8], pos: usize, big_endian: bool) -> Self {
        Self {
            buf,
            pos,
            big_endian,
        }
    }

    fn align(&mut self, align: usize) -> Result<(), CallError> {
        self.pos = self.pos.next_multiple_of(align);
        if self.pos > self.buf.len() {
            return Err(CallError::Protocol(
                "a value runs past the end of its message",
            ));
        }
        Ok(())
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], CallError> {
        let bytes = self
            .buf
            .get(self.pos..self.pos + N)
            .ok_or(CallError::Protocol(
                "a value runs past the end of its message",
            ))?;
        self.pos += N;
        Ok(bytes.try_into().unwrap_or([0; N]))
    }

    pub(crate) fn byte(&mut self) -> Result<u8, CallError> {
        Ok(self.take::<1>()?[0])
    }

    pub(crate) fn u32(&mut self) -> Result<u32, CallError> {
        self.align(4)?;
        let bytes = self.take::<4>()?;
        Ok(if self.big_endian {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    }

    pub(crate) fn i32(&mut self) -> Result<i32, CallError> {
        Ok(self.u32()?.cast_signed())
    }

    pub(crate) fn u64(&mut self) -> Result<u64, CallError> {
        self.align(8)?;
        let bytes = self.take::<8>()?;
        Ok(if self.big_endian {
            u64::from_be_bytes(bytes)
        } else {
            u64::from_le_bytes(bytes)
        })
    }

    pub(crate) fn i64(&mut self) -> Result<i64, CallError> {
        Ok(self.u64()?.cast_signed())
    }

    fn text(&mut self, len: usize) -> Result<String, CallError> {
        let bytes = self
            .buf
            .get(self.pos..self.pos + len)
            .ok_or(CallError::Protocol(
                "a string runs past the end of its message",
            ))?;
        self.pos += len + 1; // and the NUL
        String::from_utf8(bytes.to_vec()).map_err(|_| CallError::Protocol("a string isn't UTF-8"))
    }

    pub(crate) fn string(&mut self) -> Result<String, CallError> {
        let len = self.u32()? as usize;
        self.text(len)
    }

    pub(crate) fn signature(&mut self) -> Result<String, CallError> {
        let len = usize::from(self.byte()?);
        self.text(len)
    }
}

/// A message read from the bus.
#[derive(Debug)]
pub(crate) struct Message {
    pub(crate) header: Header,
    pub(crate) body: Vec<u8>,
    pub(crate) big_endian: bool,
}

impl Message {
    pub(crate) fn body(&self) -> Reader<'_> {
        Reader::new(&self.body, self.big_endian)
    }
}

/// Reads one whole message.
pub(crate) fn read_message(stream: &mut impl Read) -> Result<Message, CallError> {
    let mut fixed = [0u8; 16];
    stream.read_exact(&mut fixed)?;
    let big_endian = match fixed[0] {
        b'l' => false,
        b'B' => true,
        _ => return Err(CallError::Protocol("a message has no byte-order mark")),
    };
    let mut reader = Reader::at(&fixed, 4, big_endian);
    let body_len = reader.u32()? as usize;
    let serial = reader.u32()?;
    let fields_len = reader.u32()? as usize;
    let header_len = (16 + fields_len).next_multiple_of(8);
    let total = header_len + body_len;
    if total > MAX_MESSAGE {
        return Err(CallError::Protocol("a message is too large"));
    }
    let mut all = vec![0u8; total];
    all[..16].copy_from_slice(&fixed);
    stream.read_exact(&mut all[16..])?;

    let mut header = Header {
        kind: fixed[1],
        serial,
        ..Header::default()
    };
    let mut reader = Reader::at(&all[..16 + fields_len], 16, big_endian);
    while reader.pos < 16 + fields_len {
        reader.align(8)?;
        let code = reader.byte()?;
        let kind = reader.signature()?;
        match kind.as_str() {
            "s" | "o" => {
                let value = reader.string()?;
                match code {
                    FIELD_PATH => header.path = Some(value),
                    FIELD_INTERFACE => header.interface = Some(value),
                    FIELD_MEMBER => header.member = Some(value),
                    FIELD_ERROR_NAME => header.error_name = Some(value),
                    FIELD_DESTINATION => header.destination = Some(value),
                    _ => {}
                }
            }
            "g" => {
                let value = reader.signature()?;
                if code == FIELD_SIGNATURE {
                    header.signature = value;
                }
            }
            "u" => {
                let value = reader.u32()?;
                if code == FIELD_REPLY_SERIAL {
                    header.reply_serial = Some(value);
                }
            }
            _ => return Err(CallError::Protocol("a header field has an unexpected type")),
        }
    }
    Ok(Message {
        header,
        body: all.split_off(header_len),
        big_endian,
    })
}

/// The system bus socket: from `DBUS_SYSTEM_BUS_ADDRESS` if it names a Unix
/// path, otherwise the standard location.
pub(crate) fn system_bus_path() -> PathBuf {
    std::env::var("DBUS_SYSTEM_BUS_ADDRESS")
        .ok()
        .and_then(|address| unix_path(&address))
        .unwrap_or_else(|| PathBuf::from("/var/run/dbus/system_bus_socket"))
}

/// The first `unix:path=` in a D-Bus address list, unescaped.
fn unix_path(address: &str) -> Option<PathBuf> {
    address.split(';').find_map(|entry| {
        let pairs = entry.strip_prefix("unix:")?;
        let raw = pairs
            .split(',')
            .find_map(|pair| pair.strip_prefix("path="))?;
        let mut bytes = Vec::with_capacity(raw.len());
        let mut chars = raw.bytes();
        while let Some(b) = chars.next() {
            if b == b'%' {
                let hex = [chars.next()?, chars.next()?];
                let hex = std::str::from_utf8(&hex).ok()?;
                bytes.push(u8::from_str_radix(hex, 16).ok()?);
            } else {
                bytes.push(b);
            }
        }
        String::from_utf8(bytes).ok().map(PathBuf::from)
    })
}

/// A connection to a bus, authenticated and introduced with `Hello`.
pub(crate) struct Connection {
    stream: UnixStream,
    next_serial: u32,
}

impl Connection {
    /// Connects to the bus listening at `path`.
    pub(crate) fn open(path: &Path, timeout: Duration) -> Result<Self, CallError> {
        let stream = UnixStream::connect(path)?;
        Self::over(stream, timeout)
    }

    /// Authenticates over an already connected socket as this process's user.
    pub(crate) fn over(stream: UnixStream, timeout: Duration) -> Result<Self, CallError> {
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        // SAFETY: `getuid` always succeeds.
        let uid = unsafe { libc::getuid() };
        let mut connection = Self {
            stream,
            next_serial: 1,
        };
        connection.authenticate(uid)?;
        connection.call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
            "",
            &[],
        )?;
        Ok(connection)
    }

    fn authenticate(&mut self, uid: u32) -> Result<(), CallError> {
        let hex: String = uid
            .to_string()
            .bytes()
            .map(|b| format!("{b:02x}"))
            .collect();
        self.stream.write_all(b"\0")?;
        self.stream
            .write_all(format!("AUTH EXTERNAL {hex}\r\n").as_bytes())?;
        let line = self.read_line()?;
        if !line.starts_with("OK ") {
            return Err(CallError::Protocol(
                "the bus refused EXTERNAL authentication",
            ));
        }
        self.stream.write_all(b"BEGIN\r\n")?;
        Ok(())
    }

    /// Reads one line of the authentication exchange, byte by byte so that
    /// nothing after it is consumed.
    fn read_line(&mut self) -> Result<String, CallError> {
        let mut line = Vec::new();
        let mut byte = [0u8];
        while !line.ends_with(b"\r\n") {
            self.stream.read_exact(&mut byte)?;
            line.push(byte[0]);
            if line.len() > 512 {
                return Err(CallError::Protocol("an authentication line is too long"));
            }
        }
        line.truncate(line.len() - 2);
        String::from_utf8(line)
            .map_err(|_| CallError::Protocol("an authentication line isn't UTF-8"))
    }

    /// Calls a method and waits for its reply, skipping anything else the
    /// bus sends meanwhile (such as the `NameAcquired` signal).
    pub(crate) fn call(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        signature: &str,
        body: &[u8],
    ) -> Result<Message, CallError> {
        let serial = self.next_serial;
        self.next_serial += 1;
        let header = Header {
            kind: METHOD_CALL,
            serial,
            path: Some(path.to_owned()),
            interface: Some(interface.to_owned()),
            member: Some(member.to_owned()),
            destination: Some(destination.to_owned()),
            signature: signature.to_owned(),
            ..Header::default()
        };
        self.stream.write_all(&encode(&header, body))?;
        loop {
            let message = read_message(&mut self.stream)?;
            if message.header.reply_serial != Some(serial) {
                continue;
            }
            return match message.header.kind {
                METHOD_RETURN => Ok(message),
                ERROR => {
                    let text = if message.header.signature.starts_with('s') {
                        message.body().string().unwrap_or_default()
                    } else {
                        String::new()
                    };
                    Err(CallError::Remote {
                        name: message.header.error_name.unwrap_or_default(),
                        message: text,
                    })
                }
                _ => Err(CallError::Protocol(
                    "a reply is neither a return nor an error",
                )),
            };
        }
    }

    /// Reads a property through `org.freedesktop.DBus.Properties.Get`.
    pub(crate) fn property(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        name: &str,
    ) -> Result<Value, CallError> {
        let mut body = Writer::default();
        body.string(interface);
        body.string(name);
        let reply = self.call(
            destination,
            path,
            "org.freedesktop.DBus.Properties",
            "Get",
            "ss",
            &body.buf,
        )?;
        if reply.header.signature != "v" {
            return Err(CallError::Protocol("a property reply isn't a variant"));
        }
        let mut reader = reply.body();
        match reader.signature()?.as_str() {
            "i" => Ok(Value::I32(reader.i32()?)),
            "u" => Ok(Value::I64(i64::from(reader.u32()?))),
            "x" => Ok(Value::I64(reader.i64()?)),
            "t" => Ok(Value::I64(reader.u64()?.try_into().unwrap_or(i64::MAX))),
            _ => Err(CallError::Protocol("a property has an unexpected type")),
        }
    }
}

/// A numeric property value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Value {
    I32(i32),
    I64(i64),
}

impl Value {
    pub(crate) fn as_i64(self) -> i64 {
        match self {
            Self::I32(v) => i64::from(v),
            Self::I64(v) => v,
        }
    }
}

/// A fake bus for tests: it answers authentication, `Hello`, and whatever
/// `answer` says to each later call.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::thread::JoinHandle;

    /// What the fake bus replies to a call.
    pub(crate) enum Answer {
        Return {
            signature: &'static str,
            body: Vec<u8>,
        },
        Error {
            name: &'static str,
            message: &'static str,
        },
    }

    /// Starts a fake bus on one end of a socket pair and returns the other
    /// end, plus a handle that yields every call the bus received.
    pub(crate) fn bus(
        mut answer: impl FnMut(&Message) -> Answer + Send + 'static,
    ) -> (UnixStream, JoinHandle<Vec<Message>>) {
        let (client, mut server) = UnixStream::pair().unwrap();
        let handle = std::thread::spawn(move || {
            let mut received = Vec::new();
            let mut nul = [0u8];
            server.read_exact(&mut nul).unwrap();
            assert_eq!(nul[0], 0, "the client starts with a NUL byte");
            let auth = read_line(&mut server);
            assert!(auth.starts_with("AUTH EXTERNAL "), "{auth}");
            server
                .write_all(b"OK 0123456789abcdef0123456789abcdef\r\n")
                .unwrap();
            assert_eq!(read_line(&mut server), "BEGIN");
            let mut serial = 1000;
            while let Ok(message) = read_message(&mut server) {
                serial += 1;
                let reply_serial = Some(message.header.serial);
                let reply = if message.header.member.as_deref() == Some("Hello") {
                    // A signal first, which the client must skip.
                    let signal = Header {
                        kind: 4,
                        serial,
                        path: Some("/org/freedesktop/DBus".into()),
                        interface: Some("org.freedesktop.DBus".into()),
                        member: Some("NameAcquired".into()),
                        signature: "s".into(),
                        ..Header::default()
                    };
                    let mut body = Writer::default();
                    body.string(":1.42");
                    server.write_all(&encode(&signal, &body.buf)).unwrap();
                    serial += 1;
                    let header = Header {
                        kind: METHOD_RETURN,
                        serial,
                        reply_serial,
                        signature: "s".into(),
                        ..Header::default()
                    };
                    encode(&header, &body.buf)
                } else {
                    match answer(&message) {
                        Answer::Return { signature, body } => encode(
                            &Header {
                                kind: METHOD_RETURN,
                                serial,
                                reply_serial,
                                signature: signature.into(),
                                ..Header::default()
                            },
                            &body,
                        ),
                        Answer::Error { name, message } => {
                            let mut body = Writer::default();
                            body.string(message);
                            encode(
                                &Header {
                                    kind: ERROR,
                                    serial,
                                    reply_serial,
                                    error_name: Some(name.into()),
                                    signature: "s".into(),
                                    ..Header::default()
                                },
                                &body.buf,
                            )
                        }
                    }
                };
                received.push(message);
                server.write_all(&reply).unwrap();
            }
            received
        });
        (client, handle)
    }

    fn read_line(stream: &mut UnixStream) -> String {
        let mut line = Vec::new();
        let mut byte = [0u8];
        while !line.ends_with(b"\r\n") {
            stream.read_exact(&mut byte).unwrap();
            line.push(byte[0]);
        }
        String::from_utf8(line[..line.len() - 2].to_vec()).unwrap()
    }

    /// A variant body holding `value`.
    pub(crate) fn variant_i32(value: i32) -> Vec<u8> {
        let mut w = Writer::default();
        w.signature("i");
        w.i32(value);
        w.buf
    }

    pub(crate) fn variant_i64(value: i64) -> Vec<u8> {
        let mut w = Writer::default();
        w.signature("x");
        w.i64(value);
        w.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_encodes_byte_for_byte() {
        let header = Header {
            kind: METHOD_CALL,
            serial: 1,
            path: Some("/org/freedesktop/DBus".into()),
            interface: Some("org.freedesktop.DBus".into()),
            member: Some("Hello".into()),
            destination: Some("org.freedesktop.DBus".into()),
            ..Header::default()
        };
        let bytes = encode(&header, &[]);

        let mut expected: Vec<u8> = vec![b'l', 1, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0];
        // Fields: path (24 + 22 bytes), interface, member, destination, each
        // starting on an 8-byte boundary; 109 bytes in all.
        expected.extend_from_slice(&109u32.to_le_bytes());
        let mut field = |code: u8, kind: u8, text: &str, pad_after: usize| {
            expected.extend_from_slice(&[code, 1, kind, 0]);
            expected.extend_from_slice(&(text.len() as u32).to_le_bytes());
            expected.extend_from_slice(text.as_bytes());
            expected.push(0);
            expected.extend(std::iter::repeat_n(0, pad_after));
        };
        field(1, b'o', "/org/freedesktop/DBus", 2);
        field(2, b's', "org.freedesktop.DBus", 3);
        field(3, b's', "Hello", 2);
        field(6, b's', "org.freedesktop.DBus", 3);
        assert_eq!(expected.len(), 128);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn arguments_are_aligned_to_their_size() {
        let mut w = Writer::default();
        w.byte(7);
        w.u64(0x0102_0304_0506_0708);
        w.u32(9);
        assert_eq!(
            w.buf,
            [7, 0, 0, 0, 0, 0, 0, 0, 8, 7, 6, 5, 4, 3, 2, 1, 9, 0, 0, 0]
        );
    }

    #[test]
    fn messages_round_trip() {
        let header = Header {
            kind: ERROR,
            serial: 77,
            error_name: Some("org.example.Error".into()),
            reply_serial: Some(5),
            signature: "s".into(),
            ..Header::default()
        };
        let mut body = Writer::default();
        body.string("nope");
        let bytes = encode(&header, &body.buf);
        let message = read_message(&mut bytes.as_slice()).unwrap();
        assert_eq!(message.header, header);
        assert_eq!(message.body().string().unwrap(), "nope");
    }

    #[test]
    fn big_endian_messages_are_read_too() {
        // A method return from a big-endian peer: serial 9, replying to 5,
        // with no body.
        let bytes = [
            b'B',
            METHOD_RETURN,
            0,
            1,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            9,
            0,
            0,
            0,
            8, //
            FIELD_REPLY_SERIAL,
            1,
            b'u',
            0,
            0,
            0,
            0,
            5,
        ];
        let message = read_message(&mut bytes.as_slice()).unwrap();
        assert!(message.big_endian);
        assert_eq!(message.header.kind, METHOD_RETURN);
        assert_eq!(message.header.serial, 9);
        assert_eq!(message.header.reply_serial, Some(5));
        assert!(message.body.is_empty());
    }

    #[test]
    fn a_truncated_message_is_an_error() {
        let bytes = encode(
            &Header {
                kind: METHOD_RETURN,
                serial: 1,
                ..Header::default()
            },
            &[1, 2, 3],
        );
        assert!(read_message(&mut &bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn bus_addresses_name_a_unix_path() {
        assert_eq!(
            unix_path("unix:path=/run/dbus/system_bus_socket"),
            Some(PathBuf::from("/run/dbus/system_bus_socket"))
        );
        assert_eq!(
            unix_path("tcp:host=x;unix:guid=1,path=/tmp/a%20b"),
            Some(PathBuf::from("/tmp/a b"))
        );
        assert_eq!(unix_path("unix:abstract=/tmp/x"), None);
    }

    #[test]
    fn a_connection_authenticates_says_hello_and_reads_properties() {
        let (client, server) = fake::bus(|message| match message.header.member.as_deref() {
            Some("Get") => fake::Answer::Return {
                signature: "v",
                body: fake::variant_i64(200_000),
            },
            _ => fake::Answer::Error {
                name: "org.freedesktop.DBus.Error.UnknownMethod",
                message: "no",
            },
        });
        let mut connection = Connection::over(client, Duration::from_secs(5)).unwrap();
        let value = connection
            .property("org.example", "/org/example", "org.example.I", "Limit")
            .unwrap();
        assert_eq!(value, Value::I64(200_000));
        let error = connection
            .call("org.example", "/", "org.example.I", "Nope", "", &[])
            .unwrap_err();
        assert!(matches!(&error, CallError::Remote { name, message }
            if name == "org.freedesktop.DBus.Error.UnknownMethod" && message == "no"));
        drop(connection);

        let calls = server.join().unwrap();
        assert_eq!(calls[0].header.member.as_deref(), Some("Hello"));
        assert_eq!(calls[1].header.member.as_deref(), Some("Get"));
        assert_eq!(calls[1].header.signature, "ss");
        let mut args = calls[1].body();
        assert_eq!(args.string().unwrap(), "org.example.I");
        assert_eq!(args.string().unwrap(), "Limit");
    }
}
