//! Session and entry ids.
//!
//! Ported from Pi `packages/coding-agent/src/core/session-manager.ts`
//! (`createSessionId`, `generateId`, `assertValidSessionId`) and
//! `packages/ai/src/utils/uuid.ts` (`uuidv7`), v1.1.0.
//!
//! Session ids are UUIDv7 strings with Pi's monotonic 41-bit sequence; entry
//! ids are the first eight hex digits of a random UUID, checked against the
//! ids already in use. Two deviations, neither reachable in practice: when
//! the operating system's random source fails, bytes are drawn from the
//! standard library's per-process hash seed instead of throwing, and an
//! exhausted UUIDv7 sequence (2^40 ids in one process) is reseeded instead of
//! throwing.

use std::sync::Mutex;

use crate::session::SessionError;

const MAX_UUID_V7_TIMESTAMP: i64 = 0xffff_ffff_ffff;
const MAX_SEQUENCE: u64 = (1 << 41) - 1;

struct V7State {
    last_timestamp: i64,
    sequence: Option<u64>,
}

static V7: Mutex<V7State> = Mutex::new(V7State {
    last_timestamp: -1,
    sequence: None,
});

/// Fill `bytes` from the operating system's random source.
pub(crate) fn random_bytes(bytes: &mut [u8]) {
    if getrandom::getrandom(bytes).is_ok() {
        return;
    }
    use std::collections::hash_map::RandomState;
    use std::hash::BuildHasher;
    for (index, chunk) in bytes.chunks_mut(8).enumerate() {
        let word = RandomState::new().hash_one((bake_ai::now_ms(), index));
        for (byte, source) in chunk.iter_mut().zip(word.to_le_bytes()) {
            *byte = source;
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `crypto.randomUUID()`: a version 4 UUID.
pub fn uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    random_bytes(&mut bytes);
    bytes[6] = 0x40 | (bytes[6] & 0x0f);
    bytes[8] = 0x80 | (bytes[8] & 0x3f);
    format_uuid(&bytes)
}

fn format_uuid(bytes: &[u8; 16]) -> String {
    format!(
        "{}-{}-{}-{}-{}",
        hex(&bytes[0..4]),
        hex(&bytes[4..6]),
        hex(&bytes[6..8]),
        hex(&bytes[8..10]),
        hex(&bytes[10..16])
    )
}

/// Pi's `uuidv7()`: a time-ordered UUID whose 41-bit sequence starts at a
/// random 40-bit value and increments for each id, with the timestamp held
/// at the latest one seen so ids stay ordered when the clock steps back.
pub fn uuid_v7() -> String {
    let requested = bake_ai::now_ms().clamp(0, MAX_UUID_V7_TIMESTAMP);
    let mut bytes = [0u8; 16];
    random_bytes(&mut bytes);
    let (timestamp, sequence) = {
        let mut state = match V7.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let timestamp = requested.max(state.last_timestamp);
        state.last_timestamp = timestamp;
        let seed = || {
            (u64::from(bytes[1]) << 32)
                | (u64::from(bytes[2]) << 24)
                | (u64::from(bytes[3]) << 16)
                | (u64::from(bytes[4]) << 8)
                | u64::from(bytes[5])
        };
        let sequence = match state.sequence {
            Some(sequence) if sequence < MAX_SEQUENCE => sequence + 1,
            _ => seed(),
        };
        state.sequence = Some(sequence);
        (timestamp, sequence)
    };
    let timestamp = u64::try_from(timestamp).unwrap_or(0);
    for (index, byte) in bytes.iter_mut().take(6).enumerate() {
        *byte = (timestamp >> ((5 - index) * 8)) as u8;
    }
    bytes[6] = 0x70 | ((sequence >> 37) & 0x0f) as u8;
    bytes[7] = ((sequence >> 29) & 0xff) as u8;
    bytes[8] = 0x80 | ((sequence >> 23) & 0x3f) as u8;
    bytes[9] = ((sequence >> 15) & 0xff) as u8;
    bytes[10] = ((sequence >> 7) & 0xff) as u8;
    bytes[11] = (((sequence & 0x7f) << 1) as u8) | (bytes[11] & 0x01);
    format_uuid(&bytes)
}

/// Pi's `generateId`: eight hex digits not yet in use, or a whole UUID after
/// 100 collisions.
pub fn generate_entry_id(in_use: impl Fn(&str) -> bool) -> String {
    for _ in 0..100 {
        let mut bytes = [0u8; 4];
        random_bytes(&mut bytes);
        let id = hex(&bytes);
        if !in_use(&id) {
            return id;
        }
    }
    uuid_v4()
}

/// Pi's `assertValidSessionId`: non-empty, ASCII letters, digits, `-`, `_`,
/// and `.`, starting and ending with a letter or digit.
pub fn assert_valid_session_id(id: &str) -> Result<(), SessionError> {
    let bytes = id.as_bytes();
    let edge_ok = |byte: Option<&u8>| byte.is_some_and(u8::is_ascii_alphanumeric);
    let valid = edge_ok(bytes.first())
        && edge_ok(bytes.last())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(SessionError::InvalidSessionId)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_uuid_v7(id: &str) -> bool {
        let parts: Vec<&str> = id.split('-').collect();
        let lengths: Vec<usize> = parts.iter().map(|part| part.len()).collect();
        lengths == [8, 4, 4, 4, 12]
            && id
                .chars()
                .all(|ch| ch == '-' || ch.is_ascii_digit() || ('a'..='f').contains(&ch))
            && parts.get(2).is_some_and(|part| part.starts_with('7'))
            && parts
                .get(3)
                .is_some_and(|part| part.starts_with(['8', '9', 'a', 'b']))
    }

    #[test]
    fn uuid_v7_matches_pi_shape_and_orders() {
        let ids: Vec<String> = (0..50).map(|_| uuid_v7()).collect();
        for id in &ids {
            assert!(is_uuid_v7(id), "{id}");
        }
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn entry_ids_avoid_collisions() {
        let id = generate_entry_id(|_| false);
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_eq!(generate_entry_id(|_| true).len(), 36);
    }

    #[test]
    fn session_id_validation() {
        for id in ["a", "abc-123_def.456", "A9"] {
            assert!(assert_valid_session_id(id).is_ok(), "{id}");
        }
        for id in [
            "", "-abc", "abc-", "_abc", "abc_", ".abc", "abc.", "abc/def", "abc\\def", "abc def",
            "é",
        ] {
            assert!(assert_valid_session_id(id).is_err(), "{id}");
        }
    }
}
