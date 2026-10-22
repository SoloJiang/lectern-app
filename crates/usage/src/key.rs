//! The canonical bytes of a `UsageKey` (docs/design/usage.md「规范化与去重」):
//! a variant tag, then each field length-prefixed, so two keys are equal
//! exactly when their bytes are.

use lectern_services::session::{RawTokens, UsageKey};

const CLAUDE: u8 = 1;
const CODEX_TOKEN_COUNT: u8 = 2;
const CODEX_RESPONSE: u8 = 3;

/// Appended to a record's key for the advisor's share of the same record.
/// The encoding is self-delimiting, so a trailing byte cannot collide with
/// another key.
const ADVISOR: u8 = 0xAD;

pub(crate) fn encode(key: &UsageKey) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    match key {
        UsageKey::Claude {
            message_id,
            request_id,
        } => {
            out.push(CLAUDE);
            text(&mut out, message_id);
            match request_id {
                Some(r) => {
                    out.push(1);
                    text(&mut out, r);
                }
                None => out.push(0),
            }
        }
        UsageKey::CodexTokenCount { thread_id, total } => {
            out.push(CODEX_TOKEN_COUNT);
            text(&mut out, thread_id);
            raw(&mut out, total);
        }
        UsageKey::CodexResponse {
            thread_id,
            response_id,
        } => {
            out.push(CODEX_RESPONSE);
            text(&mut out, thread_id);
            text(&mut out, response_id);
        }
    }
    out
}

/// The key of a record's advisor share.
pub(crate) fn advisor(mut key: Vec<u8>) -> Vec<u8> {
    key.push(ADVISOR);
    key
}

fn text(out: &mut Vec<u8>, s: &str) {
    let len = u32::try_from(s.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&s.as_bytes()[..len as usize]);
}

fn raw(out: &mut Vec<u8>, tokens: &RawTokens) {
    match *tokens {
        RawTokens::Anthropic {
            input,
            output,
            cache_read,
            cache_write,
            cache_write_5m,
            cache_write_1h,
        } => {
            out.push(1);
            for n in [
                input,
                output,
                cache_read,
                cache_write,
                cache_write_5m,
                cache_write_1h,
            ] {
                out.extend_from_slice(&n.to_le_bytes());
            }
        }
        RawTokens::OpenAi {
            input,
            cached_input,
            cache_write,
            output,
            reasoning_output,
        } => {
            out.push(2);
            for n in [input, cached_input, cache_write, output, reasoning_output] {
                out.extend_from_slice(&n.to_le_bytes());
            }
        }
    }
}
