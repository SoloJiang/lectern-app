// A `&str` field must be `'static`: a borrowed runtime string does not
// compile.
fn main() {
    let owned = String::from("file contents");
    let borrowed: &str = &owned;
    lectern_logging::info!("borrowed string"; text = borrowed);
}
