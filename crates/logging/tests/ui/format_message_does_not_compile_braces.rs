// A message interpolates nothing: braces are rejected.
fn main() {
    let x = 1;
    let _ = x;
    lectern_logging::warn!("value {x}");
}
