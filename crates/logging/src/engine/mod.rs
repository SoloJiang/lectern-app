//! The logger behind the `log` facade: encoding, free-text handling and the
//! third-party rate limits. Shared by the sink and the test capture.

pub(crate) mod encode;
pub(crate) mod limit;
pub(crate) mod logger;
pub(crate) mod sanitize;
pub(crate) mod time;
