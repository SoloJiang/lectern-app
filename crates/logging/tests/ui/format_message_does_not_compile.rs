// A message is a string literal, not `format!`.
fn main() {
    let x = 1;
    lectern_logging::warn!(format!("value {}", x));
}
