//! The guest side of the engine: the `qjs_*` exports of `quickjs-wasi`'s
//! `quickjs.wasm` the driver calls, bounds-checked access to the guest's
//! linear memory, and the value helpers built on them.
//!
//! The helpers port the subset of `quickjs-wasi` 3.6.2's `dist/index.js`
//! (MIT, Copyright (c) 2026 Vercel, Inc.; see `NOTICE`) that Pi's `vm.ts`
//! uses: strings in and out (`writeString`, `newString`, `toString` with its
//! WTF-8 decoding), `callFunction`, `getProp`, and the exception accessors.
//! Every value is a pointer to a heap-allocated `JSValue` box in the guest;
//! each one the host receives it frees with `qjs_free_value`.

use std::fmt;

use wasmtime::{AsContext, AsContextMut, Instance, Memory, TypedFunc};

/// A failure while the host drives the guest.
#[derive(Debug)]
pub(crate) enum GuestError {
    /// Wasm execution trapped or a host import refused to continue; the
    /// store's state says why.
    Trap(wasmtime::Error),
    /// The guest handed the host something it cannot accept: an out-of-bounds
    /// pointer or length, or a malformed value.
    Protocol(String),
    /// The guest's allocator could not provide memory the host asked for.
    OutOfMemory(String),
    /// A string was longer than the caller accepts; it was not copied.
    TooLarge,
    /// The host's check of the output limits failed.
    OutputLimit(String),
    /// The script reported its end; the guest unwinds.
    Finished,
}

impl fmt::Display for GuestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Trap(error) => write!(formatter, "{error:#}"),
            Self::Protocol(message) | Self::OutOfMemory(message) | Self::OutputLimit(message) => {
                formatter.write_str(message)
            }
            Self::TooLarge => formatter.write_str("a string is longer than accepted"),
            Self::Finished => formatter.write_str("the script finished"),
        }
    }
}

impl From<wasmtime::Error> for GuestError {
    fn from(error: wasmtime::Error) -> Self {
        Self::Trap(error)
    }
}

pub(crate) type GuestResult<T> = Result<T, GuestError>;

/// The bytes `[ptr, ptr + len)` of `memory`, or a protocol error when any of
/// them lies outside it.
pub(crate) fn guest_slice(memory: &[u8], ptr: u32, len: u32) -> GuestResult<&[u8]> {
    let start = ptr as usize;
    start
        .checked_add(len as usize)
        .and_then(|end| memory.get(start..end))
        .ok_or_else(|| out_of_bounds(ptr, len, memory.len()))
}

/// The mutable bytes `[ptr, ptr + len)` of `memory`, bounds-checked.
pub(crate) fn guest_slice_mut(memory: &mut [u8], ptr: u32, len: u32) -> GuestResult<&mut [u8]> {
    let size = memory.len();
    let start = ptr as usize;
    start
        .checked_add(len as usize)
        .and_then(|end| memory.get_mut(start..end))
        .ok_or_else(|| out_of_bounds(ptr, len, size))
}

fn out_of_bounds(ptr: u32, len: u32, size: usize) -> GuestError {
    GuestError::Protocol(format!(
        "the guest passed {len} bytes at address {ptr}, outside its {size}-byte memory"
    ))
}

/// A little-endian `u32` at `ptr`, bounds-checked.
pub(crate) fn read_u32(memory: &[u8], ptr: u32) -> GuestResult<u32> {
    let bytes = guest_slice(memory, ptr, 4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Writes `bytes` at `ptr`, bounds-checked.
pub(crate) fn write_bytes(memory: &mut [u8], ptr: u32, bytes: &[u8]) -> GuestResult<()> {
    let len = u32::try_from(bytes.len())
        .map_err(|_| GuestError::Protocol("a write is too large".to_owned()))?;
    guest_slice_mut(memory, ptr, len)?.copy_from_slice(bytes);
    Ok(())
}

/// Decodes WTF-8, as `quickjs-wasi`'s `decodeWtf8`: QuickJS encodes a lone
/// surrogate as the 3-byte sequence `ED [A0-BF] [80-BF]`, which a Rust string
/// cannot hold, so each becomes one U+FFFD and the text keeps its UTF-16
/// length, which the output limits count. Other invalid bytes become U+FFFD
/// as `String::from_utf8_lossy` replaces them.
pub(crate) fn decode_wtf8(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    let mut decoded = String::with_capacity(bytes.len());
    let mut copied = 0;
    let mut index = 0;
    while index < bytes.len() {
        // 0xED never continues a sequence, so it always starts one here.
        let surrogate = bytes[index] == 0xed
            && bytes
                .get(index + 1)
                .is_some_and(|byte| (0xa0..=0xbf).contains(byte))
            && bytes
                .get(index + 2)
                .is_some_and(|byte| (0x80..=0xbf).contains(byte));
        if surrogate {
            decoded.push_str(&String::from_utf8_lossy(&bytes[copied..index]));
            decoded.push('\u{fffd}');
            index += 3;
            copied = index;
        } else {
            index += 1;
        }
    }
    decoded.push_str(&String::from_utf8_lossy(&bytes[copied..]));
    decoded
}

/// The exports the driver calls, checked against these signatures when the
/// engine compiles the module (`engine::EXPECTED_EXPORTS`).
#[derive(Clone)]
pub(crate) struct GuestExports {
    pub(crate) memory: Memory,
    pub(crate) initialize: TypedFunc<(), ()>,
    pub(crate) init: TypedFunc<(), i32>,
    pub(crate) init2: TypedFunc<u32, i32>,
    pub(crate) get_quickjs_version: TypedFunc<(), u32>,
    pub(crate) set_memory_limit: TypedFunc<u32, ()>,
    pub(crate) set_max_stack_size: TypedFunc<u32, ()>,
    pub(crate) set_interrupt_handler: TypedFunc<u32, ()>,
    pub(crate) eval: TypedFunc<(u32, u32, u32, u32), u32>,
    pub(crate) new_string: TypedFunc<(u32, u32), u32>,
    pub(crate) new_number: TypedFunc<f64, u32>,
    pub(crate) get_undefined: TypedFunc<(), u32>,
    pub(crate) get_true: TypedFunc<(), u32>,
    pub(crate) get_false: TypedFunc<(), u32>,
    pub(crate) get_float64: TypedFunc<u32, f64>,
    pub(crate) get_string_len: TypedFunc<(u32, u32), u32>,
    pub(crate) free_cstring: TypedFunc<u32, ()>,
    pub(crate) is_exception: TypedFunc<u32, i32>,
    pub(crate) is_undefined: TypedFunc<u32, i32>,
    pub(crate) is_string: TypedFunc<u32, i32>,
    pub(crate) get_bool: TypedFunc<u32, i32>,
    pub(crate) dup_value: TypedFunc<u32, u32>,
    pub(crate) free_value: TypedFunc<u32, ()>,
    pub(crate) get_prop_string: TypedFunc<(u32, u32), u32>,
    pub(crate) call: TypedFunc<(u32, u32, u32, u32), u32>,
    pub(crate) new_host_function: TypedFunc<(u32, u32, u32), u32>,
    pub(crate) is_job_pending: TypedFunc<(), i32>,
    pub(crate) execute_pending_job: TypedFunc<(), i32>,
    pub(crate) get_exception: TypedFunc<(), u32>,
    pub(crate) malloc: TypedFunc<u32, u32>,
    pub(crate) free: TypedFunc<u32, ()>,
}

impl GuestExports {
    pub(crate) fn new(instance: &Instance, mut store: impl AsContextMut) -> wasmtime::Result<Self> {
        let mut store = store.as_context_mut();
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| wasmtime::Error::msg("the module exports no memory"))?;
        macro_rules! func {
            ($name:literal) => {
                instance.get_typed_func(&mut store, $name)?
            };
        }
        Ok(Self {
            memory,
            initialize: func!("_initialize"),
            init: func!("qjs_init"),
            init2: func!("qjs_init2"),
            get_quickjs_version: func!("qjs_get_quickjs_version"),
            set_memory_limit: func!("qjs_set_memory_limit"),
            set_max_stack_size: func!("qjs_set_max_stack_size"),
            set_interrupt_handler: func!("qjs_set_interrupt_handler"),
            eval: func!("qjs_eval"),
            new_string: func!("qjs_new_string"),
            new_number: func!("qjs_new_number"),
            get_undefined: func!("qjs_get_undefined"),
            get_true: func!("qjs_get_true"),
            get_false: func!("qjs_get_false"),
            get_float64: func!("qjs_get_float64"),
            get_string_len: func!("qjs_get_string_len"),
            free_cstring: func!("qjs_free_cstring"),
            is_exception: func!("qjs_is_exception"),
            is_undefined: func!("qjs_is_undefined"),
            is_string: func!("qjs_is_string"),
            get_bool: func!("qjs_get_bool"),
            dup_value: func!("qjs_dup_value"),
            free_value: func!("qjs_free_value"),
            get_prop_string: func!("qjs_get_prop_string"),
            call: func!("qjs_call"),
            new_host_function: func!("qjs_new_host_function"),
            is_job_pending: func!("qjs_is_job_pending"),
            execute_pending_job: func!("qjs_execute_pending_job"),
            get_exception: func!("qjs_get_exception"),
            malloc: func!("wasm_malloc"),
            free: func!("wasm_free"),
        })
    }

    /// Copies `bytes` into a fresh guest allocation with a trailing NUL and
    /// returns its address; the caller frees it.
    pub(crate) fn write_cstring(
        &self,
        mut store: impl AsContextMut,
        bytes: &[u8],
    ) -> GuestResult<u32> {
        let len = u32::try_from(bytes.len())
            .ok()
            .and_then(|len| len.checked_add(1))
            .ok_or_else(|| {
                GuestError::OutOfMemory("a string is too large for the guest".to_owned())
            })?;
        let ptr = self.malloc.call(&mut store, len)?;
        if ptr == 0 {
            return Err(GuestError::OutOfMemory(format!(
                "the guest could not allocate {len} bytes"
            )));
        }
        let memory = self.memory.data_mut(&mut store);
        write_bytes(memory, ptr, bytes)?;
        // `malloc`'s result comes from the guest: checked like any guest pointer.
        let end = ptr.checked_add(len - 1).ok_or_else(|| {
            GuestError::Protocol(format!("the guest allocated {len} bytes at address {ptr}"))
        })?;
        write_bytes(memory, end, &[0])?;
        Ok(ptr)
    }

    /// A new guest string value.
    pub(crate) fn new_string_value(
        &self,
        mut store: impl AsContextMut,
        text: &str,
    ) -> GuestResult<u32> {
        let ptr = self.write_cstring(&mut store, text.as_bytes())?;
        // `write_cstring` checked the length fits.
        let value = self.new_string.call(&mut store, (ptr, text.len() as u32));
        self.free.call(&mut store, ptr)?;
        let value = value?;
        if self.is_exception.call(&mut store, value)? != 0 {
            self.free_value.call(&mut store, value)?;
            return Err(GuestError::OutOfMemory(format!(
                "the script's heap cannot hold a string of {} bytes",
                text.len()
            )));
        }
        Ok(value)
    }

    /// The bytes of `value` converted to a string, which runs the value's
    /// own conversion for non-strings, or `None` when that throws.
    /// `scratch` is a 4-byte guest allocation for the length. When
    /// `max_bytes` is set, a longer string is not copied: the result is
    /// `Err(TooLarge)`.
    pub(crate) fn string_bytes(
        &self,
        mut store: impl AsContextMut,
        scratch: u32,
        value: u32,
        max_bytes: Option<usize>,
    ) -> GuestResult<Option<Vec<u8>>> {
        let cstring = self.get_string_len.call(&mut store, (value, scratch))?;
        if cstring == 0 {
            return Ok(None);
        }
        let memory = self.memory.data(&store);
        let copied = read_u32(memory, scratch).and_then(|len| match max_bytes {
            Some(max) if len as usize > max => Err(GuestError::TooLarge),
            _ => guest_slice(memory, cstring, len).map(<[u8]>::to_vec),
        });
        self.free_cstring.call(&mut store, cstring)?;
        copied.map(Some)
    }

    /// `value` as a string; `Err(Protocol)` when the conversion throws.
    pub(crate) fn string(
        &self,
        store: impl AsContextMut,
        scratch: u32,
        value: u32,
    ) -> GuestResult<String> {
        self.string_bytes(store, scratch, value, None)?
            .map(|bytes| decode_wtf8(&bytes))
            .ok_or_else(|| {
                GuestError::Protocol("a value could not be converted to a string".to_owned())
            })
    }

    /// Calls `function` with `this` and `args` (borrowed) and returns the
    /// result, which may be an exception value.
    pub(crate) fn call_raw(
        &self,
        mut store: impl AsContextMut,
        function: u32,
        this: u32,
        args: &[u32],
    ) -> GuestResult<u32> {
        let mut argv = 0;
        if !args.is_empty() {
            let bytes: Vec<u8> = args.iter().flat_map(|arg| arg.to_le_bytes()).collect();
            let len = bytes.len() as u32;
            argv = self.malloc.call(&mut store, len)?;
            if argv == 0 {
                return Err(GuestError::OutOfMemory(format!(
                    "the guest could not allocate {len} bytes"
                )));
            }
            write_bytes(self.memory.data_mut(&mut store), argv, &bytes)?;
        }
        let result = self
            .call
            .call(&mut store, (function, this, args.len() as u32, argv));
        if argv != 0 {
            self.free.call(&mut store, argv)?;
        }
        Ok(result?)
    }

    /// Property `name` of `object`.
    pub(crate) fn get_prop(
        &self,
        mut store: impl AsContextMut,
        object: u32,
        name: &str,
    ) -> GuestResult<u32> {
        let ptr = self.write_cstring(&mut store, name.as_bytes())?;
        let value = self.get_prop_string.call(&mut store, (object, ptr));
        self.free.call(&mut store, ptr)?;
        Ok(value?)
    }

    /// Evaluates `code` as a global script named `filename`; the result may be
    /// an exception value.
    pub(crate) fn eval_raw(
        &self,
        mut store: impl AsContextMut,
        code: &str,
        filename: &str,
    ) -> GuestResult<u32> {
        let code_ptr = self.write_cstring(&mut store, code.as_bytes())?;
        let name_ptr = match self.write_cstring(&mut store, filename.as_bytes()) {
            Ok(ptr) => ptr,
            Err(error) => {
                self.free.call(&mut store, code_ptr)?;
                return Err(error);
            }
        };
        let value = self
            .eval
            .call(&mut store, (code_ptr, code.len() as u32, name_ptr, 0));
        self.free.call(&mut store, code_ptr)?;
        self.free.call(&mut store, name_ptr)?;
        Ok(value?)
    }

    /// The NUL-terminated string at `ptr`, at most `max` bytes long.
    pub(crate) fn read_c_string(
        &self,
        store: impl AsContext,
        ptr: u32,
        max: usize,
    ) -> GuestResult<String> {
        let memory = self.memory.data(&store);
        let start = ptr as usize;
        let tail = memory
            .get(start..)
            .ok_or_else(|| out_of_bounds(ptr, 1, memory.len()))?;
        let window = &tail[..tail.len().min(max)];
        let end = window.iter().position(|byte| *byte == 0).ok_or_else(|| {
            GuestError::Protocol(format!(
                "no string terminator within {max} bytes at address {ptr}"
            ))
        })?;
        Ok(decode_wtf8(&window[..end]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_every_range_against_the_memory() {
        let memory = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(guest_slice(&memory, 2, 3).ok(), Some(&memory[2..5]));
        assert_eq!(guest_slice(&memory, 8, 0).ok(), Some(&memory[8..8]));
        assert!(matches!(
            guest_slice(&memory, 7, 2),
            Err(GuestError::Protocol(_))
        ));
        assert!(matches!(
            guest_slice(&memory, 9, 0),
            Err(GuestError::Protocol(_))
        ));
        assert!(matches!(
            guest_slice(&memory, u32::MAX, u32::MAX),
            Err(GuestError::Protocol(_))
        ));
        assert_eq!(read_u32(&memory, 4).ok(), Some(0x0807_0605));
        assert!(read_u32(&memory, 5).is_err());
        let mut writable = memory;
        assert!(write_bytes(&mut writable, 6, &[0, 0, 0]).is_err());
        assert_eq!(writable, memory, "a refused write changes nothing");
        assert!(write_bytes(&mut writable, 6, &[9, 9]).is_ok());
        assert_eq!(&writable[6..], &[9, 9]);
    }

    #[test]
    fn decodes_lone_surrogates_as_replacement_characters() {
        assert_eq!(decode_wtf8("é😀".as_bytes()), "é😀");
        // One U+FFFD per lone surrogate, so UTF-16 lengths stay equal.
        assert_eq!(
            decode_wtf8(&[b'a', 0xed, 0xa0, 0x80, b'b', 0xed, 0xbf, 0xbf]),
            "a\u{fffd}b\u{fffd}"
        );
        // Other invalid bytes, including a truncated surrogate, are replaced
        // as `from_utf8_lossy` replaces them.
        assert_eq!(
            decode_wtf8(&[0xff, 0xe2, 0x82, 0xac, 0xed, 0xa0]),
            "\u{fffd}€\u{fffd}\u{fffd}"
        );
    }
}
