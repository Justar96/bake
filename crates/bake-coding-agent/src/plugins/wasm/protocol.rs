//! The helper's control protocol: length-delimited frames over the helper's
//! standard input and output, versioned separately from the WIT contract.
//!
//! A frame is a kind byte, a little-endian `u32` payload length, and the
//! payload. Each direction accepts only its own kinds, and each kind has a
//! payload cap the reader checks before it allocates. Payloads decode
//! totally: a short, malformed, or trailing byte is an error, never a
//! panic. Strings are a `u32` length and UTF-8 bytes.
//!
//! The parent sends `Load` once, then `Call`s, one at a time, and finally
//! `Shutdown`. The helper first sends `Hello`; it answers `Load` with
//! `Compiling` (only when it starts compiling) and `Loaded`, and each `Call`
//! with `Started` and `Reply`. `Fatal` reports a protocol error just before
//! the helper exits.

use std::io;

/// The protocol version both sides must speak.
pub(crate) const PROTOCOL_VERSION: u32 = 1;

/// The longest `Call` payload: the request header and the call's id, name,
/// and arguments.
pub(crate) const MAX_CALL_FRAME: u32 = (super::MAX_ARGS_BYTES + 8 * 1024) as u32;

/// Bytes a `Load` payload may carry beyond the component.
const LOAD_OVERHEAD: u32 = 256;

/// The longest `Reply` payload: a reason, the call's log lines, and the
/// error text, each bounded by the helper.
pub(crate) const MAX_REPLY_FRAME: u32 = 256 * 1024;

/// The longest text in a `Fatal` or failed `Loaded` frame.
pub(crate) const MAX_ERROR_TEXT: usize = 4 * 1024;

pub(crate) const HEADER_LEN: usize = 5;

/// A frame kind. Parent-to-helper kinds are below `0x80`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Load = 0x01,
    Call = 0x02,
    Shutdown = 0x03,
    Hello = 0x81,
    Compiling = 0x82,
    Loaded = 0x83,
    Started = 0x84,
    Reply = 0x85,
    Fatal = 0x86,
}

impl Kind {
    fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0x01 => Self::Load,
            0x02 => Self::Call,
            0x03 => Self::Shutdown,
            0x81 => Self::Hello,
            0x82 => Self::Compiling,
            0x83 => Self::Loaded,
            0x84 => Self::Started,
            0x85 => Self::Reply,
            0x86 => Self::Fatal,
            _ => return None,
        })
    }
}

/// The payload cap of each kind the helper accepts; `None` refuses it.
pub(crate) fn helper_cap(kind: Kind, max_component: u32) -> Option<u32> {
    match kind {
        Kind::Load => Some(max_component.saturating_add(LOAD_OVERHEAD)),
        Kind::Call => Some(MAX_CALL_FRAME),
        Kind::Shutdown => Some(0),
        _ => None,
    }
}

/// The payload cap of each kind the parent accepts; `None` refuses it.
pub(crate) fn parent_cap(kind: Kind) -> Option<u32> {
    match kind {
        Kind::Hello => Some(1024),
        Kind::Compiling => Some(0),
        Kind::Loaded => Some(MAX_ERROR_TEXT as u32 + 64),
        Kind::Started => Some(16),
        Kind::Reply => Some(MAX_REPLY_FRAME),
        Kind::Fatal => Some(MAX_ERROR_TEXT as u32 + 16),
        _ => None,
    }
}

/// Why a frame could not be read or decoded.
#[derive(Debug)]
pub(crate) enum FrameError {
    /// The stream ended inside a frame.
    Truncated,
    /// The kind byte is unknown or not accepted in this direction.
    Kind(u8),
    /// The declared length is over the kind's cap; nothing was allocated.
    Oversized { kind: Kind, len: u32, cap: u32 },
    /// The payload does not decode, or has trailing bytes.
    Malformed(&'static str),
    /// Reading failed.
    Io(io::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => f.write_str("the stream ended inside a frame"),
            Self::Kind(byte) => write!(f, "unexpected frame kind {byte:#04x}"),
            Self::Oversized { kind, len, cap } => {
                write!(
                    f,
                    "{kind:?} frame of {len} bytes is over its {cap}-byte cap"
                )
            }
            Self::Malformed(what) => write!(f, "malformed frame: {what}"),
            Self::Io(error) => write!(f, "reading a frame failed: {error}"),
        }
    }
}

pub(crate) fn check_header(
    header: [u8; HEADER_LEN],
    cap: impl Fn(Kind) -> Option<u32>,
) -> Result<(Kind, usize), FrameError> {
    let kind = Kind::from_byte(header[0]).ok_or(FrameError::Kind(header[0]))?;
    let cap = cap(kind).ok_or(FrameError::Kind(header[0]))?;
    let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]);
    if len > cap {
        return Err(FrameError::Oversized { kind, len, cap });
    }
    // `len <= cap`, a `u32`, always fits a `usize` on supported targets.
    let len = usize::try_from(len).map_err(|_| FrameError::Oversized { kind, len, cap })?;
    Ok((kind, len))
}

/// Encodes one frame.
pub(crate) fn frame(kind: Kind, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER_LEN + payload.len());
    bytes.push(kind as u8);
    // Every encoder bounds its payload far below `u32::MAX`.
    bytes.extend_from_slice(
        &u32::try_from(payload.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    bytes.extend_from_slice(payload);
    bytes
}

/// Reads one frame from a blocking reader: `Ok(None)` at a clean end of
/// stream between frames.
pub(crate) fn read_frame_sync(
    reader: &mut impl io::Read,
    cap: impl Fn(Kind) -> Option<u32>,
) -> Result<Option<(Kind, Vec<u8>)>, FrameError> {
    let mut header = [0u8; HEADER_LEN];
    let mut filled = 0;
    while filled < HEADER_LEN {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(FrameError::Io(error)),
        }
    }
    let (kind, len) = check_header(header, cap)?;
    let mut payload = vec![0u8; len];
    reader
        .read_exact(&mut payload)
        .map_err(|error| match error.kind() {
            io::ErrorKind::UnexpectedEof => FrameError::Truncated,
            _ => FrameError::Io(error),
        })?;
    Ok(Some((kind, payload)))
}

/// A payload under construction.
#[derive(Default)]
pub(crate) struct Enc(pub Vec<u8>);

impl Enc {
    pub fn u8(mut self, value: u8) -> Self {
        self.0.push(value);
        self
    }

    pub fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn u64(mut self, value: u64) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn bytes(mut self, value: &[u8]) -> Self {
        // Every field is bounded far below `u32::MAX` by its producer.
        self = self.u32(u32::try_from(value.len()).unwrap_or(u32::MAX));
        self.0.extend_from_slice(value);
        self
    }

    pub fn str(self, value: &str) -> Self {
        self.bytes(value.as_bytes())
    }
}

/// A payload being decoded; every read checks the remaining length first.
pub(crate) struct Dec<'a>(pub &'a [u8]);

impl<'a> Dec<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], FrameError> {
        if self.0.len() < count {
            return Err(FrameError::Malformed("short payload"));
        }
        let (head, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(head)
    }

    pub fn u8(&mut self) -> Result<u8, FrameError> {
        Ok(self.take(1)?[0])
    }

    pub fn bool(&mut self) -> Result<bool, FrameError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(FrameError::Malformed("invalid boolean")),
        }
    }

    pub fn u32(&mut self) -> Result<u32, FrameError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn u64(&mut self) -> Result<u64, FrameError> {
        let bytes = self.take(8)?;
        let mut array = [0u8; 8];
        array.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(array))
    }

    pub fn bytes(&mut self) -> Result<&'a [u8], FrameError> {
        let len = usize::try_from(self.u32()?).map_err(|_| FrameError::Malformed("length"))?;
        self.take(len)
    }

    pub fn str(&mut self) -> Result<&'a str, FrameError> {
        std::str::from_utf8(self.bytes()?).map_err(|_| FrameError::Malformed("invalid UTF-8"))
    }

    /// Refuses trailing bytes.
    pub fn end(self) -> Result<(), FrameError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(FrameError::Malformed("trailing bytes"))
        }
    }
}

/// How the helper bounds its own memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitMechanism {
    /// A hard `RLIMIT_AS` the helper set on itself (Linux and macOS): an
    /// address-space bound, not a resident or committed-memory bound.
    AddressSpace,
    /// A Job object's per-process committed-memory limit, set by the parent
    /// before the helper ran (Windows).
    JobCommit,
}

/// The memory limit a helper established and verified before it read any
/// input, as its `Hello` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryLimit {
    /// The mechanism.
    pub mechanism: LimitMechanism,
    /// The usage the budget was added to, in bytes: the helper's address
    /// space, or its committed memory, when it set up the limit.
    pub baseline: u64,
    /// The limit in force, in bytes: `baseline` plus the budget on Unix,
    /// the Job's limit on Windows.
    pub limit: u64,
    /// The budget the parent asked for, in bytes.
    pub budget: u64,
}

/// The helper's first frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hello {
    pub protocol: u32,
    pub build: String,
    pub generation: u64,
    pub limit: MemoryLimit,
}

impl Hello {
    pub fn encode(&self) -> Vec<u8> {
        Enc::default()
            .u32(self.protocol)
            .str(&self.build)
            .u64(self.generation)
            .u8(match self.limit.mechanism {
                LimitMechanism::AddressSpace => 1,
                LimitMechanism::JobCommit => 2,
            })
            .u64(self.limit.baseline)
            .u64(self.limit.limit)
            .u64(self.limit.budget)
            .0
    }

    pub fn decode(payload: &[u8]) -> Result<Self, FrameError> {
        let mut dec = Dec(payload);
        let protocol = dec.u32()?;
        let build = dec.str()?.to_owned();
        let generation = dec.u64()?;
        let mechanism = match dec.u8()? {
            1 => LimitMechanism::AddressSpace,
            2 => LimitMechanism::JobCommit,
            _ => return Err(FrameError::Malformed("limit mechanism")),
        };
        let limit = MemoryLimit {
            mechanism,
            baseline: dec.u64()?,
            limit: dec.u64()?,
            budget: dec.u64()?,
        };
        dec.end()?;
        Ok(Self {
            protocol,
            build,
            generation,
            limit,
        })
    }
}

/// `Load`: the API version from the manifest and the component bytes.
pub(crate) fn encode_load(api: &str, component: &[u8]) -> Vec<u8> {
    Enc::default().str(api).bytes(component).0
}

pub(crate) fn decode_load(payload: &[u8]) -> Result<(&str, &[u8]), FrameError> {
    let mut dec = Dec(payload);
    let api = dec.str()?;
    let component = dec.bytes()?;
    dec.end()?;
    Ok((api, component))
}

/// Why a component was not loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadFailure {
    /// It is not a valid component, or its shape is refused: an import
    /// outside the allowlist, a `bake:plugin` version the host does not
    /// support, or a missing or extra export. Found before compiling.
    Invalid,
    /// Compiling failed.
    Compile,
    /// It compiled, but its imports or export types do not match the
    /// host's bindings.
    Link,
}

/// `Loaded`: `Ok` with the compile time in microseconds, or the failure.
pub(crate) fn encode_loaded(result: &Result<u64, (LoadFailure, String)>) -> Vec<u8> {
    match result {
        Ok(micros) => Enc::default().u8(0).u64(*micros).0,
        Err((failure, message)) => {
            Enc::default()
                .u8(match failure {
                    LoadFailure::Invalid => 1,
                    LoadFailure::Compile => 2,
                    LoadFailure::Link => 3,
                })
                .str(bounded(message, MAX_ERROR_TEXT))
                .0
        }
    }
}

pub(crate) fn decode_loaded(
    payload: &[u8],
) -> Result<Result<u64, (LoadFailure, String)>, FrameError> {
    let mut dec = Dec(payload);
    let result = match dec.u8()? {
        0 => Ok(dec.u64()?),
        tag => {
            let failure = match tag {
                1 => LoadFailure::Invalid,
                2 => LoadFailure::Compile,
                3 => LoadFailure::Link,
                _ => return Err(FrameError::Malformed("load result")),
            };
            Err((failure, dec.str()?.to_owned()))
        }
    };
    dec.end()?;
    Ok(result)
}

/// `Call`: one tool call for the generation's instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallRequest {
    pub id: u64,
    pub generation: u64,
    pub tool_call_id: String,
    pub tool_name: String,
    pub args_json: String,
}

impl CallRequest {
    pub fn encode(&self) -> Vec<u8> {
        Enc::default()
            .u64(self.id)
            .u64(self.generation)
            .str(&self.tool_call_id)
            .str(&self.tool_name)
            .str(&self.args_json)
            .0
    }

    pub fn decode(payload: &[u8]) -> Result<Self, FrameError> {
        let mut dec = Dec(payload);
        let request = Self {
            id: dec.u64()?,
            generation: dec.u64()?,
            tool_call_id: dec.str()?.to_owned(),
            tool_name: dec.str()?.to_owned(),
            args_json: dec.str()?.to_owned(),
        };
        dec.end()?;
        Ok(request)
    }
}

/// `Started`: the helper is running the call with this id.
pub(crate) fn encode_started(id: u64) -> Vec<u8> {
    Enc::default().u64(id).0
}

pub(crate) fn decode_started(payload: &[u8]) -> Result<u64, FrameError> {
    let mut dec = Dec(payload);
    let id = dec.u64()?;
    dec.end()?;
    Ok(id)
}

/// Why a guest call failed inside the helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapKind {
    /// The call used up its CPU fuel.
    OutOfFuel,
    /// The guest asked for more linear memory than the store's total.
    Memory,
    /// The guest asked for more table elements than the store's total.
    Table,
    /// The component needs more instances, memories, or tables than allowed.
    Count,
    /// The guest passed or returned more data than one copy may carry.
    Copy,
    /// The guest drew more random bytes than its quota.
    Random,
    /// The call missed the helper's deadline.
    Deadline,
    /// The guest returned a value outside the contract, such as an
    /// oversized reason.
    InvalidReturn,
    /// Any other trap, such as a panic or `unreachable`.
    Other,
}

impl TrapKind {
    fn to_byte(self) -> u8 {
        match self {
            Self::OutOfFuel => 1,
            Self::Memory => 2,
            Self::Table => 3,
            Self::Count => 4,
            Self::Copy => 5,
            Self::Random => 6,
            Self::Deadline => 7,
            Self::InvalidReturn => 8,
            Self::Other => 9,
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::OutOfFuel,
            2 => Self::Memory,
            3 => Self::Table,
            4 => Self::Count,
            5 => Self::Copy,
            6 => Self::Random,
            7 => Self::Deadline,
            8 => Self::InvalidReturn,
            9 => Self::Other,
            _ => return None,
        })
    }
}

/// The guest's answer, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Continue,
    Block { reason: String, terminate: bool },
    GuestError(String),
    Trap(TrapKind, String),
}

/// A staged log line's level, as WIT `log-level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// `debug`.
    Debug,
    /// `info`.
    Info,
    /// `warn`.
    Warn,
    /// `error`.
    Error,
}

/// One line the guest logged during a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Its level.
    pub level: LogLevel,
    /// The text, at most 4 KiB, cut on a character boundary.
    pub text: String,
}

/// `Reply`: the outcome of one call and what it staged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reply {
    pub id: u64,
    pub generation: u64,
    /// Which instance of the generation answered, from 1; a trap discards
    /// the instance and the next call creates another.
    pub instance: u32,
    pub outcome: Outcome,
    pub logs: Vec<LogLine>,
    pub dropped_logs: u32,
}

impl Reply {
    pub fn encode(&self) -> Vec<u8> {
        let mut enc = Enc::default()
            .u64(self.id)
            .u64(self.generation)
            .u32(self.instance);
        enc = match &self.outcome {
            Outcome::Continue => enc.u8(0),
            Outcome::Block { reason, terminate } => enc.u8(1).str(reason).u8(u8::from(*terminate)),
            Outcome::GuestError(message) => enc.u8(2).str(message),
            Outcome::Trap(kind, message) => enc.u8(3).u8(kind.to_byte()).str(message),
        };
        // Every log list is bounded by the helper's staging quota.
        enc = enc.u32(u32::try_from(self.logs.len()).unwrap_or(u32::MAX));
        for line in &self.logs {
            enc = enc
                .u8(match line.level {
                    LogLevel::Debug => 0,
                    LogLevel::Info => 1,
                    LogLevel::Warn => 2,
                    LogLevel::Error => 3,
                })
                .str(&line.text);
        }
        enc.u32(self.dropped_logs).0
    }

    pub fn decode(payload: &[u8]) -> Result<Self, FrameError> {
        let mut dec = Dec(payload);
        let id = dec.u64()?;
        let generation = dec.u64()?;
        let instance = dec.u32()?;
        let outcome = match dec.u8()? {
            0 => Outcome::Continue,
            1 => Outcome::Block {
                reason: dec.str()?.to_owned(),
                terminate: dec.bool()?,
            },
            2 => Outcome::GuestError(dec.str()?.to_owned()),
            3 => {
                let kind =
                    TrapKind::from_byte(dec.u8()?).ok_or(FrameError::Malformed("trap kind"))?;
                Outcome::Trap(kind, dec.str()?.to_owned())
            }
            _ => return Err(FrameError::Malformed("outcome")),
        };
        let count = dec.u32()?;
        // Each line takes at least five bytes, so the count is bounded by
        // the payload before anything is reserved.
        if usize::try_from(count).map_or(true, |count| count > dec.0.len() / 5) {
            return Err(FrameError::Malformed("log count"));
        }
        let mut logs = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let level = match dec.u8()? {
                0 => LogLevel::Debug,
                1 => LogLevel::Info,
                2 => LogLevel::Warn,
                3 => LogLevel::Error,
                _ => return Err(FrameError::Malformed("log level")),
            };
            logs.push(LogLine {
                level,
                text: dec.str()?.to_owned(),
            });
        }
        let dropped_logs = dec.u32()?;
        dec.end()?;
        Ok(Self {
            id,
            generation,
            instance,
            outcome,
            logs,
            dropped_logs,
        })
    }
}

/// `Fatal`: the helper's exit code and the protocol error.
pub(crate) fn encode_fatal(code: u8, message: &str) -> Vec<u8> {
    Enc::default()
        .u8(code)
        .str(bounded(message, MAX_ERROR_TEXT))
        .0
}

pub(crate) fn decode_fatal(payload: &[u8]) -> Result<(u8, String), FrameError> {
    let mut dec = Dec(payload);
    let code = dec.u8()?;
    let message = dec.str()?.to_owned();
    dec.end()?;
    Ok((code, message))
}

/// `text` cut to at most `max` bytes on a character boundary.
pub(crate) fn bounded(text: &str, max: usize) -> &str {
    super::super::truncate(text, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(bytes: &[u8]) -> Result<Option<(Kind, Vec<u8>)>, FrameError> {
        read_frame_sync(&mut &bytes[..], |kind| helper_cap(kind, 1024))
    }

    #[test]
    fn frames_round_trip_and_end_cleanly() {
        let call = CallRequest {
            id: 7,
            generation: 3,
            tool_call_id: "c1".into(),
            tool_name: "write".into(),
            args_json: "{\"path\":\"é\"}".into(),
        };
        let mut stream = frame(Kind::Call, &call.encode());
        stream.extend(frame(Kind::Shutdown, &[]));
        let mut reader = &stream[..];
        let cap = |kind| helper_cap(kind, 1024);
        let (kind, payload) = read_frame_sync(&mut reader, cap).unwrap().unwrap();
        assert_eq!(kind, Kind::Call);
        assert_eq!(CallRequest::decode(&payload).unwrap(), call);
        let (kind, payload) = read_frame_sync(&mut reader, cap).unwrap().unwrap();
        assert_eq!((kind, payload.len()), (Kind::Shutdown, 0));
        assert!(read_frame_sync(&mut reader, cap).unwrap().is_none());
    }

    #[test]
    fn caps_and_kinds_are_checked_before_allocating() {
        // A Call declaring 4 GiB is refused from its header alone.
        let mut header = vec![Kind::Call as u8];
        header.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            read(&header),
            Err(FrameError::Oversized {
                kind: Kind::Call,
                len: u32::MAX,
                ..
            })
        ));
        // A Load over the component cap, a Shutdown with a payload.
        let load = frame(Kind::Load, &vec![0; 1024 + LOAD_OVERHEAD as usize + 1]);
        assert!(matches!(
            read(&load),
            Err(FrameError::Oversized {
                kind: Kind::Load,
                ..
            })
        ));
        assert!(matches!(
            read(&frame(Kind::Shutdown, &[0])),
            Err(FrameError::Oversized { .. })
        ));
        // Helper-bound kinds are refused by the parent and the reverse.
        assert!(matches!(
            read(&frame(Kind::Reply, &[])),
            Err(FrameError::Kind(0x85))
        ));
        assert!(matches!(
            read(&[0x7f, 0, 0, 0, 0]),
            Err(FrameError::Kind(0x7f))
        ));
        assert!(parent_cap(Kind::Call).is_none());
        // A header or payload cut short.
        assert!(matches!(
            read(&[Kind::Call as u8, 4]),
            Err(FrameError::Truncated)
        ));
        assert!(matches!(
            read(&[Kind::Call as u8, 4, 0, 0, 0, 1]),
            Err(FrameError::Truncated)
        ));
    }

    #[test]
    fn payloads_refuse_short_trailing_and_invalid_bytes() {
        let call = CallRequest {
            id: 1,
            generation: 1,
            tool_call_id: String::new(),
            tool_name: "edit".into(),
            args_json: "{}".into(),
        }
        .encode();
        let mut trailing = call.clone();
        trailing.push(0);
        assert!(matches!(
            CallRequest::decode(&trailing),
            Err(FrameError::Malformed("trailing bytes"))
        ));
        assert!(matches!(
            CallRequest::decode(&call[..call.len() - 1]),
            Err(FrameError::Malformed("short payload"))
        ));
        let invalid = Enc::default()
            .u64(1)
            .u64(1)
            .bytes(&[0xff])
            .str("")
            .str("")
            .0;
        assert!(matches!(
            CallRequest::decode(&invalid),
            Err(FrameError::Malformed("invalid UTF-8"))
        ));
        // A length prefix larger than the rest of the payload.
        let lying = Enc::default().u64(1).u64(1).u32(u32::MAX).0;
        assert!(CallRequest::decode(&lying).is_err());
        // A log count no payload could hold is refused before reserving.
        let reply = Enc::default().u64(1).u64(1).u32(1).u8(0).u32(u32::MAX).0;
        assert!(matches!(
            Reply::decode(&reply),
            Err(FrameError::Malformed("log count"))
        ));
        assert!(matches!(
            Reply::decode(
                &Enc::default()
                    .u64(1)
                    .u64(1)
                    .u32(1)
                    .u8(1)
                    .str("r")
                    .u8(2)
                    .u32(0)
                    .u32(0)
                    .0
            ),
            Err(FrameError::Malformed("invalid boolean"))
        ));
    }

    #[test]
    fn replies_and_hellos_round_trip() {
        let reply = Reply {
            id: 9,
            generation: 2,
            instance: 1,
            outcome: Outcome::Block {
                reason: "no".into(),
                terminate: true,
            },
            logs: vec![LogLine {
                level: LogLevel::Warn,
                text: "line".into(),
            }],
            dropped_logs: 3,
        };
        assert_eq!(Reply::decode(&reply.encode()).unwrap(), reply);
        let trap = Reply {
            outcome: Outcome::Trap(TrapKind::Copy, "copy".into()),
            logs: Vec::new(),
            ..reply
        };
        assert_eq!(Reply::decode(&trap.encode()).unwrap(), trap);
        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            build: "b".into(),
            generation: 4,
            limit: MemoryLimit {
                mechanism: LimitMechanism::JobCommit,
                baseline: 1,
                limit: 2,
                budget: 3,
            },
        };
        assert_eq!(Hello::decode(&hello.encode()).unwrap(), hello);
        let loaded = Err((LoadFailure::Link, "x".repeat(MAX_ERROR_TEXT + 10)));
        let decoded = decode_loaded(&encode_loaded(&loaded)).unwrap();
        assert_eq!(decoded.unwrap_err().1.len(), MAX_ERROR_TEXT);
    }
}
