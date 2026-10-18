#[test]
fn core_has_no_driver_or_vendor_deps() {
    let toml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    for banned in [
        "soapysdr",
        "gnuradio",
        "futuresdr",
        "openai",
        "anthropic",
        "egui",
        "tauri",
        "wgpu",
    ] {
        assert!(
            !toml.contains(banned),
            "zeta-core must not depend on {banned}"
        );
    }
}
