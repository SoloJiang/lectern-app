//! What the runtime log keeps of this crate's values (docs/design/project_home.md
//! 「日志与诊断」): counts, generations and a session's tag. Never a title, a
//! project or package name, a branch or a path.

use lectern_logging::Value;
use lectern_services::session::SessionKey;

/// A length as a log field.
pub(crate) fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The 128-bit tag of a session ID for log fields: the UUID itself when the
/// ID is one (Claude Code and Codex IDs are), a hash otherwise (a Claude Code
/// subagent's `<parent>/agent-<id>`). The same derivation as `review`'s and
/// `usage`'s, so that the records of one session carry the same value in all
/// three crates.
pub(crate) fn session_tag(id: &str) -> [u8; 16] {
    let hex: Vec<u8> = id.bytes().filter(|b| *b != b'-').collect();
    if hex.len() == 32 && hex.iter().all(u8::is_ascii_hexdigit) {
        let mut out = [0u8; 16];
        for (i, pair) in hex.chunks(2).enumerate() {
            let digit = |b: u8| (b as char).to_digit(16).unwrap_or(0) as u8;
            out[i] = digit(pair[0]) << 4 | digit(pair[1]);
        }
        return out;
    }
    use std::hash::{Hash, Hasher};
    let mut out = [0u8; 16];
    for (i, half) in out.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (i, id).hash(&mut h);
        half.copy_from_slice(&h.finish().to_le_bytes());
    }
    out
}

/// A session key as a field value: its tag (the source ID is a runtime
/// string and is not written).
pub(crate) fn session(key: &SessionKey) -> Value<'static> {
    Value::uuid(session_tag(&key.id))
}
