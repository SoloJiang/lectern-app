// A field value is never a runtime string: `String` has no `Field`.
fn main() {
    let owned = String::from("file contents");
    lectern_logging::info!("owned string"; text = owned);
}
