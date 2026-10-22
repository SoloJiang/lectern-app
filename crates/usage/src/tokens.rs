//! ADR 0014 / ADR 0022 normalization: four kinds of tokens that do not
//! overlap (docs/design/usage.md「规范化与去重」).

use lectern_services::session::RawTokens;
use lectern_services::usage::Tokens;

/// The four kinds of `raw`, done once per record when it is stored; the raw
/// numbers are not kept.
///
/// - Anthropic: `input` excludes the cache already. `cache_write` falls back
///   to `cache_write_5m + cache_write_1h` when it is 0 and they are not.
/// - OpenAI / Codex: `input` includes `cached_input`, which becomes
///   `cache_read` (at most `input`); `output` already includes
///   `reasoning_output`.
pub fn normalize(raw: &RawTokens) -> Tokens {
    match *raw {
        RawTokens::Anthropic {
            input,
            output,
            cache_read,
            cache_write,
            cache_write_5m,
            cache_write_1h,
        } => {
            let split = cache_write_5m.saturating_add(cache_write_1h);
            Tokens {
                input,
                output,
                cache_read,
                cache_write: if cache_write == 0 { split } else { cache_write },
            }
        }
        RawTokens::OpenAi {
            input,
            cached_input,
            cache_write,
            output,
            reasoning_output: _,
        } => {
            let cache_read = cached_input.min(input);
            Tokens {
                input: input - cache_read,
                output,
                cache_read,
                cache_write,
            }
        }
    }
}
