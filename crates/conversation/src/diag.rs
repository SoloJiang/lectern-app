//! What the runtime log keeps of this crate's values (docs/design/conversation.md
//! 「日志与诊断」): counts, generations, sizes and discriminant names. Never a
//! prompt, a draft, the agent's text, an injected text, a tool summary, a
//! permission title, a command or working directory, a preview path or line, a
//! plan step, the text of a failed prompt, an option ID, a review note's
//! location, a session title, a terminal's output or an entry key.

use lectern_logging::Ctx;
use lectern_services::conversation::{CallKind, Link, LinkError, OptionKind, PermissionDetail};

/// A length as a log field.
pub(crate) fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The discriminant of a link, for `conversation shown` and the status line's
/// `link` attribute.
pub(crate) fn link_name(link: &Link) -> &'static str {
    match link {
        Link::None => "none",
        Link::Connecting => "connecting",
        Link::Idle => "idle",
        Link::Working => "working",
        Link::Stopping => "stopping",
        Link::Failed(_) => "failed",
    }
}

/// The discriminant of a link error, for `conversation failed shown`.
pub(crate) fn link_error_name(error: &LinkError) -> &'static str {
    match error {
        LinkError::Spawn => "spawn",
        LinkError::Unsupported => "unsupported",
        LinkError::LoadFailed => "load_failed",
        LinkError::Protocol => "protocol",
        LinkError::Exited { .. } => "exited",
        LinkError::AuthRequired => "auth_required",
        LinkError::PromptFailed { .. } => "prompt_failed",
    }
}

/// The discriminant of a tool call's kind: the `kind` field of `permission
/// answered` and the `kind` attribute of a request's node.
pub(crate) fn call_kind_name(kind: CallKind) -> &'static str {
    match kind {
        CallKind::Read => "read",
        CallKind::Edit => "edit",
        CallKind::Delete => "delete",
        CallKind::Move => "move",
        CallKind::Search => "search",
        CallKind::Execute => "execute",
        CallKind::Fetch => "fetch",
        CallKind::Think => "think",
        CallKind::SpawnAgent => "spawn_agent",
        CallKind::Other => "other",
    }
}

/// The kind of option that answered a request, for `permission answered`.
pub(crate) fn option_name(kind: OptionKind) -> &'static str {
    match kind {
        OptionKind::AllowOnce => "allow_once",
        OptionKind::AllowAlways => "allow_always",
        OptionKind::RejectOnce => "reject_once",
        OptionKind::RejectAlways => "reject_always",
    }
}

/// The `detail` attribute of a request's node.
pub(crate) fn detail_name(detail: &PermissionDetail) -> &'static str {
    match detail {
        PermissionDetail::Command { .. } => "command",
        PermissionDetail::Edits(_) => "edits",
        PermissionDetail::None => "none",
    }
}

/// The 128-bit tag of a session ID for the `session` scope: the UUID itself
/// when the ID is one (Claude Code and Codex IDs are), a hash otherwise. The
/// same algorithm as `agents`, `usage` and `review`, so that the records of the
/// crates for one session can be matched. Does not allocate.
fn session_tag(id: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let mut digits = 0usize;
    let mut uuid = true;
    for b in id.bytes() {
        if b == b'-' {
            continue;
        }
        let Some(d) = (b as char).to_digit(16) else {
            uuid = false;
            break;
        };
        if digits == 32 {
            uuid = false;
            break;
        }
        // `to_digit(16)` is below 16.
        let d = d as u8;
        out[digits / 2] |= if digits.is_multiple_of(2) { d << 4 } else { d };
        digits += 1;
    }
    if uuid && digits == 32 {
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

/// A session's scope: only `session` is set, the rest (`plugin`, `act`) stays
/// the outer scope's.
pub(crate) fn session_ctx(id: &str) -> Ctx {
    Ctx {
        session: Some(session_tag(id)),
        ..Ctx::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uuid_is_its_own_tag() {
        let tag = session_tag("11111111-2222-4333-8444-555555555555");
        assert_eq!(
            tag,
            [
                0x11, 0x11, 0x11, 0x11, 0x22, 0x22, 0x43, 0x33, 0x84, 0x44, 0x55, 0x55, 0x55, 0x55,
                0x55, 0x55
            ]
        );
    }

    #[test]
    fn another_id_is_hashed_and_stable() {
        let a = session_tag("parent/agent-1");
        assert_eq!(a, session_tag("parent/agent-1"));
        assert_ne!(a, session_tag("parent/agent-2"));
    }
}
