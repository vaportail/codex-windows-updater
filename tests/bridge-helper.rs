// Standalone mock launcher for the Node-API contract test. Never updates or kills.
fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--bridge-check") => print!("{}", std::env::var("BRIDGE_TEST_REPLY").unwrap()),
        Some("--bridge-install") => std::fs::write(
            std::env::var_os("BRIDGE_TEST_INSTALL_LOG").unwrap(),
            "--bridge-install",
        )
        .unwrap(),
        _ => std::process::exit(2),
    }
}
