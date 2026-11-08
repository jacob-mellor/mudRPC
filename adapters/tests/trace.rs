//! Traceability (SPEC.md, the bar): every test bullet in the spec ends with the names of the tests that pin it, in
//! square brackets, and each name is a `#[test]` function in the tree; a bullet may instead say `[manual: ...]` or
//! `[ci: ...]` when the check is a human's or a CI job's, which is then stated, not hidden. Fails on a bullet with no
//! bracket, a name no test file defines, or a bracket with nothing in it.
#![warn(clippy::pedantic, clippy::nursery)]
use std::collections::HashSet;

fn test_names(dir: &std::path::Path, out: &mut HashSet<String>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() { if p.file_name().is_some_and(|n| n != "target" && n != "wycheproof" && !n.to_string_lossy().starts_with('.')) { test_names(&p, out); } continue; }
        if p.extension().is_some_and(|x| x == "rs") {
            let mut test = false; // the attributes above the next item include `#[test]`
            for l in std::fs::read_to_string(&p).unwrap().lines() {
                if l.starts_with("#[") { test |= l.trim_end() == "#[test]"; continue; }
                if let Some(rest) = l.strip_prefix("fn ").filter(|_| test) { if let Some(i) = rest.find('(') { out.insert(rest[..i].to_string()); } }
                if !l.trim().is_empty() { test = false; }
            }
        }
    }
}

#[test]
fn every_spec_test_bullet_names_a_test_that_exists() {
    let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."));
    let spec = std::fs::read_to_string(root.join("SPEC.md")).unwrap();
    let mut names = HashSet::new();
    test_names(root, &mut names);
    let (mut bullets, mut in_tests, mut unpinned) = (0, false, Vec::new());
    for (n, l) in spec.lines().enumerate() {
        if l.starts_with("## ") || l.starts_with("### ") { in_tests = false; }
        if l == "Tests that define this layer:" || l == "Tests:" { in_tests = true; continue; }
        if !in_tests { continue; }
        if !l.starts_with("- ") { if !l.trim().is_empty() { in_tests = false; } continue; }
        bullets += 1;
        let Some((_, tag)) = l.trim_end().strip_suffix(']').and_then(|l| l.rsplit_once('[')) else { unpinned.push(format!("SPEC.md:{}: no [tests]", n + 1)); continue };
        if tag.starts_with("manual:") || tag.starts_with("ci:") { continue; }
        let cited: Vec<&str> = tag.split('`').skip(1).step_by(2).collect();
        if cited.is_empty() { unpinned.push(format!("SPEC.md:{}: empty bracket", n + 1)); }
        for t in cited { if !names.contains(t) { unpinned.push(format!("SPEC.md:{}: no test named {t}", n + 1)); } }
    }
    assert!(unpinned.is_empty(), "{}", unpinned.join("\n"));
    assert!(bullets >= 50, "the spec's test lists were found: {bullets} bullets");
}
