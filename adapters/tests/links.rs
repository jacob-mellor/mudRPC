//! Every link in every `.md` file in the tree resolves inside it: a relative path to a file or directory, an `#anchor` to
//! a heading of its target as GitHub names it, and nothing outside the repository, a bare URL included.
#![warn(clippy::pedantic, clippy::nursery)]
use std::path::{Path, PathBuf};

fn markdown(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let (p, name) = (e.path(), e.file_name().to_string_lossy().into_owned());
        let foreign = ["target", "vendor", "node_modules", "gen", "pb", "out"].contains(&name.as_str()) || name.starts_with('.');   // built or installed, not ours
        if p.is_dir() { if !foreign { markdown(&p, out); } } else if p.extension().is_some_and(|x| x == "md") { out.push(p); }
    }
}

/// The numbered lines outside fenced code blocks.
fn prose(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut fenced = false;
    text.lines().enumerate().filter(move |(_, l)| { if l.trim_start().starts_with("```") { fenced = !fenced; return false; } !fenced })
}

/// GitHub's anchor for each heading: lowercase, spaces to hyphens, punctuation dropped; a repeat gets `-1`, `-2`.
fn anchors(text: &str) -> Vec<String> {
    let mut bases: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for (_, l) in prose(text) {
        let Some(h) = l.strip_prefix('#').map(|h| h.trim_start_matches('#')).filter(|h| h.starts_with(' ')) else { continue };
        let slug: String = h.trim().chars().filter_map(|c| match c { ' ' => Some('-'), c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c), _ => None }).flat_map(char::to_lowercase).collect();
        let n = bases.iter().filter(|b| **b == slug).count();
        out.push(if n == 0 { slug.clone() } else { format!("{slug}-{n}") });
        bases.push(slug);
    }
    out
}

#[test]
fn every_internal_link_resolves_and_none_leaves_the_repository() {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/..")).canonicalize().unwrap();
    let mut files = Vec::new();
    markdown(&root, &mut files);
    let (mut links, mut broken) = (0, Vec::new());
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        let at = |n: usize| format!("{}:{}", f.strip_prefix(&root).unwrap().display(), n + 1);
        for (n, l) in prose(&text) {
            let l: String = l.split('`').step_by(2).collect(); // code spans are not links
            if l.contains("http://") || l.contains("https://") { broken.push(format!("{}: a URL", at(n))); }
            for (i, _) in l.match_indices("](") {
                let Some(target) = l[i + 2..].split(')').next().and_then(|t| t.split(' ').next()) else { continue };
                links += 1;
                if target.starts_with("//") || target.contains(':') { broken.push(format!("{}: {target} leaves the repository", at(n))); continue; }
                let (path, anchor) = target.split_once('#').unwrap_or((target, ""));
                let Ok(file) = (if path.is_empty() { f.clone() } else { f.parent().unwrap().join(path) }).canonicalize() else { broken.push(format!("{}: {target} does not exist", at(n))); continue };
                if !file.starts_with(&root) { broken.push(format!("{}: {target} is outside the tree", at(n))); continue; }
                if !anchor.is_empty() && !std::fs::read_to_string(&file).is_ok_and(|t| anchors(&t).iter().any(|a| a == anchor)) {
                    broken.push(format!("{}: {target} names no heading there", at(n)));
                }
            }
        }
    }
    assert!(broken.is_empty(), "{}", broken.join("\n"));
    assert!(files.len() >= 11 && links >= 30, "the tree's documents were read: {} files, {links} links", files.len());
}

#[test]
fn anchors_follow_githubs_rule() {
    let text = "# mudRPC\n## Try it in five minutes\n### 1 · The wire\n```\n# not a heading\n```\n## Verify\n## Verify\n## `mud gen`, the stubs\n#nope";
    assert_eq!(anchors(text), ["mudrpc", "try-it-in-five-minutes", "1--the-wire", "verify", "verify-1", "mud-gen-the-stubs"]);
}
