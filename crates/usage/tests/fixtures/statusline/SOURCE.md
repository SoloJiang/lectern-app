# statusline fixtures

All files here are synthetic. They are written by hand from the public
documentation below; none is taken from a machine's Claude Code
configuration or session logs. Values (IDs, paths, the model entry) are
made up.

- Source: <https://code.claude.com/docs/en/statusline> (the source ADR 0014
  lists), section "Available data" with its JSON example, the notes below
  it and the "rate limit usage" example.
- Checked: 2026-11-01, by fetching the page (no browser, no local file).
  The page shows no publication or "last updated" date; the check date is
  the date of record. It mentions Claude Code versions only for
  `rate_limits.spend_limit` (v2.1.251 and later).

## What the page says about `rate_limits`

| Field | Type and meaning |
|---|---|
| `rate_limits.five_hour.used_percentage` | number, share of the 5-hour window used, from 0 to 100 (the example has a fraction, 23.5) |
| `rate_limits.five_hour.resets_at` | Unix epoch seconds when the window resets |
| `rate_limits.seven_day.used_percentage` | as above, for the 7-day window |
| `rate_limits.seven_day.resets_at` | as above |
| `rate_limits.spend_limit.*` | behind a Claude apps gateway with a spend limit; `used_percentage` may exceed 100; `used_usd`, `limit_usd`, `period` are optional. Not read by `usage` |

- `rate_limits` is there only for claude.ai Pro, Max and Ultra
  subscribers (or behind a gateway with a spend limit), and only after the
  first API response of the session.
- Each window (`five_hour`, `seven_day`, `spend_limit`) may be absent on
  its own, and Claude Code drops a window once its `resets_at` has passed.
- The script runs again when a window reaches its `resets_at`, when an
  assistant message arrives, and on a few other triggers; on an idle
  session it may not run for a long time.

`usage` reads `five_hour` and `seven_day` only. The window lengths (300 and
10080 minutes) come from the field names, not from the file. The file's
modification time is the sampling time.

## Files

| File | Case |
|---|---|
| `complete.json` | both windows, plus fields `usage` ignores (`spend_limit`, `model`, `workspace`, `context_window`, `vim`) |
| `five-hour-only.json`, `seven-day-only.json` | one window absent |
| `no-rate-limits.json` | no `rate_limits` (not a subscriber, or before the first response) |
| `out-of-range.json` | percentages below 0 and above 100 |
| `wrong-types.json` | a percentage that is a string, a reset time that is a string |
| `not-json.txt`, `array.json` | not an object |

A file over 1 MiB is made by the tests themselves.
