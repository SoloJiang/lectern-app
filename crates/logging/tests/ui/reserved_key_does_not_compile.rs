// The record's own keys and the scope's keys cannot be field names.
fn main() {
    lectern_logging::info!("reserved"; msg = 1);
    lectern_logging::info!("scope key"; plugin = 2);
}
