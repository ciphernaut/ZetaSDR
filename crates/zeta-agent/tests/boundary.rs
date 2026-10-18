//! Design 5.2: the runtime never calls a model from anywhere but zeta-agent.
use std::path::{Path, PathBuf};

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn only_zeta_agent_speaks_the_llm_wire() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    rs_files(&crates, &mut files);
    for f in files
        .iter()
        .filter(|f| f.components().any(|c| c.as_os_str() == "src"))
    {
        let text = std::fs::read_to_string(f).unwrap();
        if text.contains("chat/completions") || text.contains("chat_template_kwargs") {
            assert!(
                f.starts_with(crates.join("zeta-agent")),
                "{} speaks the LLM wire",
                f.display()
            );
        }
    }
}

#[test]
fn core_has_no_transport_deps() {
    let toml = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../zeta-core/Cargo.toml"
    ))
    .unwrap();
    for banned in ["axum", "reqwest", "rmcp", "hyper", "tungstenite"] {
        assert!(
            !toml.contains(banned),
            "zeta-core must not depend on {banned}"
        );
    }
}
