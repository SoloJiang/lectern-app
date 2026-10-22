//! Shares kept as millionths of a token, and the largest remainder method
//! that turns them back into whole tokens (docs/design/usage.md「取整与时间窗口」,
//! ADR 0020「分摊按分数保存、精确相加」).

/// Millionths of a token per token.
pub const MICRO: u64 = 1_000_000;

/// Most tokens [`split`] takes: beyond this the millionths do not fit in a
/// `u64`. A single record is many orders of magnitude below it.
pub const MAX_SPLIT_TOKENS: u64 = u64::MAX / MICRO;

/// The largest remainder method: `parts` are rows in millionths of a token;
/// returns each row in whole tokens, adding up to `parts`' sum divided by
/// [`MICRO`]. Rows with equal remainders are rounded up in index order.
///
/// The sum should be a whole number of tokens (every record's facts add up
/// to whole tokens). When it is not, the target is the nearest whole number,
/// halves rounded up.
pub fn largest_remainder(parts: &[u64]) -> Vec<u64> {
    let sum: u128 = parts.iter().map(|&p| u128::from(p)).sum();
    let micro = u128::from(MICRO);
    let target = (sum + micro / 2) / micro;
    apportion(parts, target)
}

/// `parts` rounded to whole tokens adding up to `target`, which must lie
/// between the sum of the floors and the sum of the ceilings.
fn apportion(parts: &[u64], target: u128) -> Vec<u64> {
    let mut out: Vec<u64> = parts.iter().map(|&p| p / MICRO).collect();
    let floors: u128 = out.iter().map(|&p| u128::from(p)).sum();
    let mut left = target.saturating_sub(floors);
    if left == 0 {
        return out;
    }
    let mut order: Vec<usize> = (0..parts.len())
        .filter(|&i| !parts[i].is_multiple_of(MICRO))
        .collect();
    // Larger remainder first; equal remainders in index order (a stable sort).
    order.sort_by_key(|&i| std::cmp::Reverse(parts[i] % MICRO));
    for i in order {
        if left == 0 {
            break;
        }
        out[i] += 1;
        left -= 1;
    }
    out
}

/// `tokens` (whole) split by `weights` into millionths of a token, adding up
/// to exactly `tokens * MICRO`: each part is rounded down, the remaining
/// millionths go to the largest remainders, equal ones in index order.
///
/// All-zero weights split equally; no weights give no parts. `tokens` above
/// [`MAX_SPLIT_TOKENS`] is taken as that.
pub fn split(tokens: u64, weights: &[u64]) -> Vec<u64> {
    if weights.is_empty() {
        return Vec::new();
    }
    let total = u128::from(tokens.min(MAX_SPLIT_TOKENS)) * u128::from(MICRO);
    let equal = weights.iter().all(|&w| w == 0);
    let weight = |i: usize| -> u128 { if equal { 1 } else { u128::from(weights[i]) } };
    let sum: u128 = (0..weights.len()).map(weight).sum();
    let mut out = Vec::with_capacity(weights.len());
    let mut rems = Vec::with_capacity(weights.len());
    let mut given: u128 = 0;
    for i in 0..weights.len() {
        let exact = total * weight(i);
        let part = exact / sum;
        rems.push(exact % sum);
        given += part;
        // `part <= total <= u64::MAX`.
        out.push(part as u64);
    }
    let mut left = total - given;
    let mut order: Vec<usize> = (0..weights.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(rems[i]));
    for i in order {
        if left == 0 {
            break;
        }
        if rems[i] == 0 {
            break;
        }
        out[i] += 1;
        left -= 1;
    }
    out
}
