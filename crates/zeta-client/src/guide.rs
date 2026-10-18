//! Line-oriented extraction of the committed AGENTS.md into tool descriptions and a system prompt.

use std::collections::BTreeMap;

/// Per-operation sections under `## Operations`, keyed by `### <name>`, with
/// `CLI: ` lines removed and blank runs collapsed.
pub fn op_sections(md: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut in_ops = false;
    let mut cur: Option<(String, Vec<&str>)> = None;
    let mut flush = |cur: &mut Option<(String, Vec<&str>)>| {
        if let Some((name, lines)) = cur.take() {
            let mut kept: Vec<&str> = Vec::new();
            for l in lines.into_iter().filter(|l| !l.starts_with("CLI: ")) {
                if l.trim().is_empty() && kept.last().is_none_or(|p| p.trim().is_empty()) {
                    continue;
                }
                kept.push(l);
            }
            out.insert(name, kept.join("\n").trim().to_string());
        }
    };
    for line in md.lines() {
        if line.starts_with("## ") {
            flush(&mut cur);
            in_ops = line.trim_end() == "## Operations";
        } else if in_ops && line.starts_with("### ") {
            flush(&mut cur);
            cur = Some((line[4..].trim().to_string(), Vec::new()));
        } else if let Some((_, lines)) = cur.as_mut() {
            lines.push(line);
        }
    }
    flush(&mut cur);
    out
}

/// The title and intro, then the `## Rules` and `## Field craft` sections.
pub fn system_prompt(md: &str) -> String {
    let mut intro = Vec::new();
    let mut rules = Vec::new();
    let mut craft = Vec::new();
    let mut target = 0; // 0 intro, 1 rules, 2 craft, 3 dropped
    for line in md.lines() {
        if line.starts_with("## ") {
            target = match line.trim_end() {
                "## Rules" => 1,
                "## Field craft" => 2,
                _ => 3,
            };
        }
        match target {
            0 => intro.push(line),
            1 => rules.push(line),
            2 => craft.push(line),
            _ => {}
        }
    }
    let parts: Vec<String> = [intro, rules, craft]
        .iter()
        .map(|p| p.join("\n").trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    format!("{}\n", parts.join("\n\n").trim())
}
