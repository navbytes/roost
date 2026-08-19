//! Test-only source scanning, shared by the mechanical gates that pin a
//! rule about *what may be written in `src/`* rather than about what a
//! function returns.
//!
//! Two such gates exist. §2's theme stance (`theme.rs`) bans constructing a
//! fixed colour anywhere in the tree; C34's (`input.rs`) bans spelling a
//! chord as a string literal outside the places licensed to author one.
//! Both are the same shape and were the same fifteen lines of directory
//! walk, so the walk lives here once.
//!
//! Source-scanning is the honest way to pin either rule: they are about
//! what the code *says*, and no runtime assertion can see a literal that a
//! future surface has not written yet.

/// Every `.rs` file under `src/`, as `(path, contents)`.
pub fn src_files() -> Vec<(std::path::PathBuf, String)> {
    let mut stack = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    let mut out = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read a src/ directory") {
            let path = entry.expect("a src/ dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).expect("read a src/ file");
                out.push((path, text));
            }
        }
    }
    assert!(out.len() > 5, "the source scan found almost nothing — check the walk");
    out
}

/// A file's **production** half: everything above its trailing test module.
///
/// Gates about what ships must not read test code, which legitimately writes
/// the very literals they ban (a test asserting the overlay draws `Alt+v`
/// has to say `Alt+v`). Requiring an exemption marker on every such line
/// instead would put dozens of markers in the test modules and train the
/// reader to ignore them, which is how a marker stops meaning anything.
///
/// The cut is the first test-gated `#[cfg(...)]` that introduces a
/// **module**. Both halves of that are load-bearing, and both were found by
/// this module's own guard tests rather than reasoned out:
/// - **a module, not any item.** `#[cfg(test)]` also attaches to
///   test-support items scattered through production code
///   (`render.rs`'s `status_legend_text`, `app.rs`'s `Search::over`,
///   `input.rs`'s `action_name`); cutting at the first of *those* hid most
///   of two files from the gates — a source scan that silently stops
///   reading is worse than none.
/// - **any test-gated cfg, not the bare literal.** `infra/qos.rs` gates its
///   module on `all(test, target_os = "macos")`, so matching `#[cfg(test)]`
///   exactly let a whole test module through into the production half.
pub fn production(text: &str) -> &str {
    let mut from = 0;
    while let Some(rel) = text[from..].find("#[cfg(") {
        let at = from + rel;
        from = at + 1;
        // The attribute's own text, up to the closing `)]`.
        let Some(end) = text[at..].find(")]") else { continue };
        let attr = &text[at..at + end];
        // Any cfg that *gates on test* and introduces a module — not just
        // the bare `#[cfg(test)]` form. `infra/qos.rs` gates its module on
        // `all(test, target_os = "macos")`, which the literal match missed;
        // that file's whole test module was surviving into the production
        // half until this module's own guard test said so.
        if !attr.contains("test") {
            continue;
        }
        let rest = text[at + end + ")]".len()..].trim_start();
        if rest.starts_with("mod ") || rest.starts_with("pub mod ") {
            return &text[..at];
        }
    }
    text
}

/// The contents of every double-quoted string literal on `line`, for a gate
/// that cares about text roost can *print* rather than text it discusses.
///
/// Deliberately simple: comment lines are dropped by the caller, and the
/// split treats every `"` as a delimiter, so a line containing an escaped
/// quote splits oddly. That costs nothing here — the gates ask only whether
/// a needle *appears* in some literal on the line, and a mis-split can move
/// a needle between pieces but never out of all of them.
pub fn string_literals(line: &str) -> impl Iterator<Item = &str> {
    line.split('"').skip(1).step_by(2)
}

/// Is this line prose rather than code? Doc comments and comments discuss
/// chords and colours constantly and must not trip a gate about what ships.
pub fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with("/*") || t.starts_with('*')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cut must leave no test code in the production half, or every
    /// gate built on it quietly reads assertions as if they were shipped
    /// strings. This is the check that caught the first implementation,
    /// which cut at the first `#[cfg(test)]` of any kind.
    #[test]
    fn the_production_cut_leaves_no_test_module_behind() {
        for (path, text) in src_files() {
            let prod = production(&text);
            assert!(
                !prod.contains("\nmod tests {"),
                "{}: a test module survived the production cut",
                path.display(),
            );
        }
    }

    /// And the opposite failure, which is the quieter one: a cut so early
    /// that real code stops being scanned. Every file that has a test
    /// module must keep the bulk of itself on the production side.
    #[test]
    fn the_production_cut_never_hides_real_code() {
        for (path, text) in src_files() {
            if !text.contains("#[cfg(test)]") {
                continue;
            }
            let prod = production(&text);
            // A test-support item near the top of a file used to truncate
            // it here; `app.rs` lost 6,000 lines and `render.rs` 1,300.
            assert!(
                prod.contains("#[cfg(test)]") || !text.contains("\n#[cfg(test)]\nfn "),
                "{}: the cut landed before a test-support item, so the code after it \
                 is no longer scanned",
                path.display(),
            );
        }
    }

    #[test]
    fn string_literals_reads_only_quoted_text() {
        let line = r#"    foo("Alt+w", "close"); // mentions Alt+q in prose"#;
        let lits: Vec<&str> = string_literals(line).collect();
        assert_eq!(lits, vec!["Alt+w", "close"]);
    }

    #[test]
    fn comments_are_recognized_in_every_form_the_tree_uses() {
        assert!(is_comment("// plain"));
        assert!(is_comment("    /// doc"));
        assert!(is_comment("//! module doc"));
        assert!(!is_comment(r#"    let x = "Alt+w";"#));
    }
}
