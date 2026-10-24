//! Tests of the probe (docs/design/ui_probe.md「测试」). The recorder's armed
//! flag is process-wide, so the tests that record run one per process under
//! nextest (`just test`).

mod annotate;
mod canary;
mod client;
mod frames;
mod protocol;
mod server;
mod tree;
