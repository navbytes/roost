//! Openers: config.json's `"open"` rules, the path-under-the-cursor parser and
//! argv templating. Pure — the stat and the spawn live in `infra::open`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Url,
    File,
    Dir,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OpenRule {
    pub kind: Kind,
    #[serde(default)]
    pub ext: Vec<String>,
    pub run: Vec<String>,
}

/// What a click landed on. `is_dir` is only meaningful after `infra::open`'s stat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Url(String),
    Path { path: PathBuf, line: Option<u32>, col: Option<u32>, is_dir: bool },
}

const PLACEHOLDERS: [&str; 5] = ["{path}", "{line}", "{col}", "{loc}", "{url}"];
/// Launchers (and wrappers around them) that open anything, `.app` bundles included.
const OPENERS: [&str; 21] = [
    "open",
    "xdg-open",
    "env",
    "gio",
    "kde-open",
    "gnome-open",
    "handlr",
    "nohup",
    "nice",
    "timeout",
    "arch",
    "exo-open",
    "mimeopen",
    "wslview",
    "rifle",
    "cygstart",
    "gvfs-open",
    "kde-open5",
    "kioclient",
    "kioclient5",
    "kioclient6",
];
/// Shell metacharacters; also what splits an argument into words for the launcher check.
const META: &str = "'\"`;$|&()<>\\";
/// Macros or bundles that run code when "opened".
const LAUNCHABLE: [&str; 9] =
    ["app", "command", "workflow", "terminal", "tool", "jar", "fileloc", "inetloc", "desktop"];

fn validate(r: &OpenRule) -> Result<(), String> {
    if r.run.is_empty() {
        return Err("\"run\" is empty".into());
    }
    if r.kind != Kind::File && !r.ext.is_empty() {
        return Err("\"ext\" only applies to kind \"file\"".into());
    }
    if let Some(e) = r.ext.iter().find(|e| e.is_empty() || e.contains('.')) {
        return Err(format!("ext {e:?} can never match: use one bare extension like \"gz\""));
    }
    let own: &[&str] = if r.kind == Kind::Url { &["{url}"] } else { &PLACEHOLDERS[..4] };
    for (i, arg) in r.run.iter().enumerate() {
        for p in PLACEHOLDERS.iter().filter(|p| arg.contains(**p)) {
            if i == 0 {
                return Err(format!("{p} in run[0] — the program must be literal"));
            }
            if !own.contains(p) {
                return Err(format!("{p} is not valid for kind {:?}", r.kind));
            }
            // Anything that re-parses its argument (sh -c, python -c, tmux, ...)
            // turns a path embedded in a string into code.
            if arg.chars().any(|c| c.is_whitespace() || META.contains(c)) {
                return Err(format!(
                    "{p} shares an argument with spaces or shell characters: split it into \
                     separate argv entries; never embed a placeholder in a command string"
                ));
            }
        }
    }
    let broad = r.kind == Kind::Dir || (r.kind == Kind::File && r.ext.is_empty());
    // Words, not whole arguments: `sh -c 'open "$1"'` runs `open` too.
    let launcher = r
        .run
        .iter()
        .filter(|a| !PLACEHOLDERS.iter().any(|p| a.contains(p)))
        .flat_map(|a| a.split(|c: char| c.is_whitespace() || META.contains(c)))
        .find(|w| {
            let base = Path::new(w).file_name().and_then(|b| b.to_str()).map(str::to_lowercase);
            base.is_some_and(|b| OPENERS.contains(&b.as_str()))
        });
    if let (true, Some(l)) = (broad, launcher) {
        return Err(format!(
            "{l} on a dir or a file rule without \"ext\" would launch .app bundles and \
             executables (a .app is a dir); give a file rule an \"ext\" list to use it"
        ));
    }
    Ok(())
}

/// The `"open"` array of a parsed config.json; a bad rule is skipped and
/// named in `problems`, never fatal.
pub fn parse_rules(
    value: &serde_json::Value,
    source: &str,
    problems: &mut Vec<String>,
) -> Vec<OpenRule> {
    let Some(v) = value.get("open") else { return Vec::new() };
    let Some(entries) = v.as_array() else {
        problems.push(format!("{source}: \"open\" must be an array — ignored"));
        return Vec::new();
    };
    let mut rules = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let parsed = serde_json::from_value::<OpenRule>(entry.clone()).map_err(|e| e.to_string());
        match parsed.and_then(|mut r| {
            for e in &mut r.ext {
                *e = e.strip_prefix('.').unwrap_or(e).to_lowercase();
            }
            validate(&r).map(|()| r)
        }) {
            Ok(r) => rules.push(r),
            Err(why) => problems.push(format!("{source}: open[{i}]: {why} — skipped")),
        }
    }
    rules
}

impl OpenRule {
    pub fn matches(&self, t: &Target) -> bool {
        match (self.kind, t) {
            (Kind::Url, Target::Url(_)) => true,
            (Kind::Dir, Target::Path { is_dir: true, .. }) => true,
            (Kind::File, Target::Path { path, is_dir: false, .. }) => {
                self.ext.is_empty()
                    || path
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| self.ext.contains(&e.to_lowercase()))
            }
            _ => false,
        }
    }

    /// Placeholders are substituted per argument and the result is an argv,
    /// never a shell line — a path with `;` or `$()` in it stays one argument.
    pub fn argv(&self, t: &Target) -> Vec<OsString> {
        let num = |n: &Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
        let vals: Vec<(&str, OsString)> = match t {
            Target::Url(u) => vec![("{url}", u.into())],
            Target::Path { path, line, col, .. } => vec![
                ("{path}", path.clone().into_os_string()),
                ("{line}", num(line).into()),
                ("{col}", num(col).into()),
                (
                    "{loc}",
                    match (line, col) {
                        (Some(l), Some(c)) => format!(":{l}:{c}"),
                        (Some(l), None) => format!(":{l}"),
                        _ => String::new(),
                    }
                    .into(),
                ),
            ],
        };
        self.run.iter().map(|a| expand(a, &vals)).collect()
    }
}

/// The argv to spawn for `t`: first matching rule, else the platform opener
/// for URLs, else `None` (the click falls through). `Err` when a rule would
/// open a bundle that runs code, whatever the handler is.
pub fn select(rules: &[OpenRule], t: &Target) -> Result<Option<Vec<OsString>>, String> {
    let Some(rule) = rules.iter().find(|r| r.matches(t)) else {
        return Ok(match t {
            Target::Url(u) => {
                let prog = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
                Some(vec![prog.into(), u.into()])
            }
            Target::Path { .. } => None,
        });
    };
    if let Target::Path { path, .. } = t {
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_lowercase);
        if ext.is_some_and(|e| LAUNCHABLE.contains(&e.as_str())) {
            return Err("open: refusing to open a launchable bundle".into());
        }
    }
    Ok(Some(rule.argv(t)))
}

/// Single pass, so a substituted value that itself contains `{line}` is not re-expanded.
fn expand(arg: &str, vals: &[(&str, OsString)]) -> OsString {
    let mut out = OsString::new();
    let mut rest = arg;
    while let Some(i) = rest.find('{') {
        out.push(&rest[..i]);
        rest = &rest[i..];
        match vals.iter().find(|(k, _)| rest.starts_with(k)) {
            Some((k, v)) => {
                out.push(v);
                rest = &rest[k.len()..];
            }
            None => {
                out.push("{");
                rest = &rest[1..];
            }
        }
    }
    out.push(rest);
    out
}

fn split_num(s: &str) -> Option<(&str, u32)> {
    let (head, n) = s.rsplit_once(':')?;
    // `parse` alone would take "+3".
    if !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((head, n.parse().ok()?))
}

/// A path-looking token (`./a.rs:12:3`, `~/x`, `src/lib`) resolved against
/// `cwd`; existence is not checked here. Leading `.` is never trimmed — `./a`
/// must not become the absolute `/a`.
pub fn path_in_token(token: &str, cwd: &Path, home: Option<&Path>) -> Option<Target> {
    let wrap = |c: char| "()[]{}<>\"'`".contains(c);
    let mut t = token.trim_start_matches(wrap);
    loop {
        let n = t.trim_end_matches(|c: char| wrap(c) || ",;:!?".contains(c));
        // A trailing dot is sentence punctuation unless it is `.`/`..` itself.
        let dots = matches!(n, "." | "..") || n.ends_with("/.") || n.ends_with("/..");
        let n = if n.ends_with('.') && !dots { &n[..n.len() - 1] } else { n };
        if n.len() == t.len() {
            break;
        }
        t = n;
    }
    let (mut rest, mut line, mut col) = (t, None, None);
    if let Some((head, n)) = split_num(rest) {
        (rest, line) = (head, Some(n));
        if let Some((head, l)) = split_num(rest) {
            (rest, line, col) = (head, Some(l), Some(n));
        }
    }
    // A stray `/` or `//` exists and is a dir, so it would hint on every slash.
    if rest.chars().all(|c| c == '/') {
        return None;
    }
    let path = if rest.starts_with('/') {
        PathBuf::from(rest)
    } else if let Some(sub) = rest.strip_prefix("~/") {
        home?.join(sub)
    } else if rest.starts_with("./")
        || rest.starts_with("../")
        || rest.contains('/')
        || Path::new(rest).extension().is_some()
        || (rest.starts_with('.') && rest.len() > 1 && rest != "..")
    {
        cwd.join(rest)
    } else {
        return None;
    };
    let nonzero = |n: Option<u32>| n.filter(|&n| n > 0);
    Some(Target::Path { path, line: nonzero(line), col: nonzero(col), is_dir: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(json: &str) -> (Vec<OpenRule>, Vec<String>) {
        let mut problems = Vec::new();
        let rules = parse_rules(&serde_json::from_str(json).unwrap(), "c.json", &mut problems);
        (rules, problems)
    }

    fn path(token: &str) -> Option<(PathBuf, Option<u32>, Option<u32>)> {
        match path_in_token(token, Path::new("/proj"), Some(Path::new("/home/u")))? {
            Target::Path { path, line, col, .. } => Some((path, line, col)),
            Target::Url(_) => unreachable!(),
        }
    }

    #[test]
    fn tokens_become_paths() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(path("src/a.rs:12:3,"), Some((p("/proj/src/a.rs"), Some(12), Some(3))));
        assert_eq!(path("(src/a.rs:12)."), Some((p("/proj/src/a.rs"), Some(12), None)));
        assert_eq!(path("\"/etc/hosts\""), Some((p("/etc/hosts"), None, None)));
        assert_eq!(path("~/x/y.md"), Some((p("/home/u/x/y.md"), None, None)));
        assert_eq!(path("a.rs"), Some((p("/proj/a.rs"), None, None)));
        assert_eq!(path("src/dir"), Some((p("/proj/src/dir"), None, None)));
        // a `+` is not a digit, and `~user` is not expanded
        assert_eq!(path("a.rs:+3").map(|t| t.1), Some(None));
        assert_eq!(path("~root/x").map(|t| t.0), Some(p("/proj/~root/x")));
        // prose is not a path
        assert_eq!(path("hello"), None);
        assert_eq!(path("done!"), None);
        // `:0` is no line; dotfiles are candidates
        assert_eq!(path("a.rs:0:0").map(|t| (t.1, t.2)), Some((None, None)));
        assert_eq!(path(".gitignore").map(|t| t.0), Some(p("/proj/.gitignore")));
        assert_eq!(path(".."), None);
        assert_eq!(path("."), None);
        // separators alone are not a path
        assert_eq!(path("/"), None);
        assert_eq!(path("//"), None);
        assert_eq!(path("/:3"), None);
        assert!(path("/tmp").is_some() && path("/tmp/x").is_some());
    }

    /// Sentence-final `.` is trimmed, but never from `.`/`..` path components.
    #[test]
    fn trailing_dot_trim_spares_dot_components() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(path("../..").unwrap().0, p("/proj/../.."));
        assert_eq!(path("src/..").unwrap().0, p("/proj/src/.."));
        assert_eq!(path("src/..,").unwrap().0, p("/proj/src/.."));
        assert_eq!(path("see/a.rs.").unwrap().0, p("/proj/see/a.rs"));
        assert_eq!(path("a.rs:12.").map(|t| (t.0, t.1)), Some((p("/proj/a.rs"), Some(12))));
    }

    /// The URL trimmer strips `.` from both ends, which would turn `./a` into `/a`.
    #[test]
    fn leading_dots_survive() {
        assert_eq!(path("./src/a.rs").unwrap().0, PathBuf::from("/proj/./src/a.rs"));
        assert_eq!(path("../up/b.rs").unwrap().0, PathBuf::from("/proj/../up/b.rs"));
    }

    #[test]
    fn argv_substitutes_per_argument_without_a_shell() {
        let (rules, problems) = cfg(
            r#"{"open":[{"kind":"file","run":["code","--goto","{path}{loc}","{line}/{col}"]}]}"#,
        );
        assert!(problems.is_empty());
        let t = |line, col| Target::Path {
            path: PathBuf::from("/a b/$(x);y {line}"),
            line,
            col,
            is_dir: false,
        };
        assert_eq!(
            rules[0].argv(&t(Some(4), Some(2))),
            ["code", "--goto", "/a b/$(x);y {line}:4:2", "4/2"]
        );
        assert_eq!(rules[0].argv(&t(Some(4), None))[2], "/a b/$(x);y {line}:4");
        assert_eq!(rules[0].argv(&t(None, None)), ["code", "--goto", "/a b/$(x);y {line}", "/"]);
        // a path that is not UTF-8 reaches the argv byte for byte
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::OsStr::from_bytes(b"/a/\xff.rs");
        let argv =
            rules[0].argv(&Target::Path { path: raw.into(), line: None, col: None, is_dir: false });
        assert_eq!(argv[2].as_bytes(), b"/a/\xff.rs");
        let (rules, _) = cfg(r#"{"open":[{"kind":"url","run":["br","--new","{url}"]}]}"#);
        assert_eq!(
            rules[0].argv(&Target::Url("https://a.co".into())),
            ["br", "--new", "https://a.co"]
        );
    }

    #[test]
    fn rules_match_by_kind_and_ext() {
        let (rules, _) =
            cfg(r#"{"open":[{"kind":"file","ext":[".PNG","pdf"],"run":["open","{path}"]}]}"#);
        let f = |p: &str, is_dir| Target::Path { path: p.into(), line: None, col: None, is_dir };
        assert!(rules[0].matches(&f("/a/b.png", false)));
        assert!(rules[0].matches(&f("/a/B.Pdf", false)));
        assert!(!rules[0].matches(&f("/a/b.rs", false)));
        assert!(!rules[0].matches(&f("/a/noext", false)));
        assert!(!rules[0].matches(&f("/a/b.png", true)));
        assert!(!rules[0].matches(&Target::Url("https://a.co".into())));
    }

    #[test]
    fn invalid_rules_are_skipped_one_problem_each() {
        let bad = [
            r#"{"kind":"link","run":["x"]}"#,
            r#"{"kind":"file","run":[]}"#,
            r#"{"kind":"file"}"#,
            r#"{"kind":"file","run":["{path}"]}"#,
            r#"{"kind":"url","run":["x","{path}"]}"#,
            r#"{"kind":"file","run":["x","{url}"]}"#,
            r#"{"kind":"dir","ext":["a"],"run":["x"]}"#,
            r#"{"kind":"dir","run":["open","{path}"]}"#,
            r#"{"kind":"file","run":["/usr/bin/open","{path}"]}"#,
            r#"{"kind":"file","ext":[],"run":["xdg-open","{path}"]}"#,
        ];
        let ok = r#"{"kind":"url","run":["open","{url}"]}"#;
        let json = format!(r#"{{"open":[{},{ok}]}}"#, bad.join(","));
        let (rules, problems) = cfg(&json);
        assert_eq!(rules.len(), 1, "{problems:?}");
        assert_eq!(problems.len(), bad.len());
        assert!(problems[7].contains(".app"), "{}", problems[7]);
        // `open` is fine once a file rule names its extensions
        let (rules, problems) =
            cfg(r#"{"open":[{"kind":"file","ext":["pdf"],"run":["open","{path}"]}]}"#);
        assert_eq!((rules.len(), problems.len()), (1, 0));
        let (rules, problems) = cfg(r#"{"open":{}}"#);
        assert_eq!((rules.len(), problems.len()), (0, 1));
    }

    #[test]
    fn launchers_wrappers_and_odd_ext_are_rejected() {
        let bad = [
            r#"{"kind":"dir","run":["OPEN","{path}"]}"#,
            r#"{"kind":"dir","run":["env","{path}"]}"#,
            r#"{"kind":"file","run":["/usr/bin/gio","open","{path}"]}"#,
            r#"{"kind":"file","run":["handlr","open","{path}"]}"#,
            r#"{"kind":"dir","run":["nohup","open","{path}"]}"#,
            r#"{"kind":"file","run":["timeout","5","open","{path}"]}"#,
            r#"{"kind":"dir","run":["exo-open","{path}"]}"#,
            r#"{"kind":"file","run":["arch","-arm64","open","{path}"]}"#,
            r#"{"kind":"file","run":["sh","-c","open \"$1\"","_","{path}"]}"#,
            r#"{"kind":"dir","run":["sh","-c","/usr/bin/OPEN \"$1\"","_","{path}"]}"#,
            r#"{"kind":"dir","run":["kioclient6","exec","{path}"]}"#,
            r#"{"kind":"file","run":["kioclient","exec","{path}"]}"#,
            r#"{"kind":"file","ext":["tar.gz"],"run":["x"]}"#,
            r#"{"kind":"file","ext":["."],"run":["x"]}"#,
            r#"{"kind":"url","run":["x","{loc}"]}"#,
        ];
        let json = format!(r#"{{"open":[{}]}}"#, bad.join(","));
        let (rules, problems) = cfg(&json);
        assert_eq!((rules.len(), problems.len()), (0, bad.len()), "{problems:?}");
        // an ext-filtered launcher passes, wrappers included
        let (rules, problems) =
            cfg(r#"{"open":[{"kind":"file","ext":["PNG"],"run":["OPEN","{path}"]},
                {"kind":"file","ext":["pdf"],"run":["nohup","open","{path}"]},
                {"kind":"file","ext":["pdf"],"run":["sh","-c","open \"$1\"","_","{path}"]},
                {"kind":"file","run":["sh","-c","gui-editor \"$1\"","_","{path}"]}]}"#);
        assert_eq!((rules.len(), problems.len()), (4, 0), "{problems:?}");
    }

    /// A placeholder may only sit in an argument that is one plain token.
    #[test]
    fn placeholders_in_command_strings_are_rejected() {
        let bad = [
            r#"["sh","-c","vi {path}"]"#,
            r#"["bash","-ce","vi {path}"]"#,
            r#"["bash","-c","-e","vi {path}"]"#,
            r#"["/usr/bin/env","sh","-c","vi {path}"]"#,
            r#"["python3","-c","open('{path}')"]"#,
            r#"["osascript","-e","tell app \"X\" to open \"{path}\""]"#,
            r#"["tmux","new-window","vi {path}"]"#,
            r#"["x","$(touch {path})"]"#,
            r#"["x","a;{path}"]"#,
            r#"["x","{path}|y"]"#,
        ];
        let json: Vec<String> =
            bad.iter().map(|run| format!(r#"{{"kind":"file","run":{run}}}"#)).collect();
        let (rules, problems) = cfg(&format!(r#"{{"open":[{}]}}"#, json.join(",")));
        assert_eq!((rules.len(), problems.len()), (0, bad.len()), "{problems:?}");
        assert!(problems[0].contains("separate argv"), "{}", problems[0]);
        // plain tokens, and the safe wrapper (the path is its own argument)
        let (rules, problems) =
            cfg(r#"{"open":[{"kind":"file","run":["code","--goto={path}{loc}"]},
                {"kind":"file","run":["sh","-c","gui-editor \"$1\"","_","{path}"]},
                {"kind":"url","run":["br","{url}"]}]}"#);
        assert_eq!((rules.len(), problems.len()), (3, 0), "{problems:?}");
    }

    #[test]
    fn select_picks_the_first_match_and_refuses_bundles() {
        let (rules, problems) = cfg(r#"{"open":[
            {"kind":"file","ext":["png"],"run":["viewer","{path}"]},
            {"kind":"file","run":["code","{path}{loc}"]},
            {"kind":"dir","run":["code","{path}"]}]}"#);
        assert!(problems.is_empty(), "{problems:?}");
        let f = |p: &str, is_dir| Target::Path { path: p.into(), line: Some(9), col: None, is_dir };
        let sel = |t: &Target| select(&rules, t).unwrap().unwrap();
        assert_eq!(sel(&f("/a/x.PNG", false)), ["viewer", "/a/x.PNG"]);
        assert_eq!(sel(&f("/a/x.rs", false)), ["code", "/a/x.rs:9"]);
        assert_eq!(sel(&f("/a/d", true)), ["code", "/a/d"]);
        // a Tool.app is a dir: the dir rule matches but the bundle is refused
        assert!(select(&rules, &f("/Apps/Tool.app", true)).is_err());
        assert!(select(&rules, &f("/a/run.COMMAND", false)).is_err());
        for bundle in ["/a/x.desktop", "/a/x.jar", "/a/x.tool"] {
            assert!(select(&rules, &f(bundle, false)).is_err(), "{bundle}");
        }
        // no rule: paths fall through, URLs get the platform opener, a URL rule wins
        assert_eq!(select(&[], &f("/Apps/Tool.app", true)), Ok(None));
        let url = Target::Url("https://a.co".into());
        let default = select(&[], &url).unwrap().unwrap();
        assert_eq!(default[1], "https://a.co");
        assert!(default[0] == "open" || default[0] == "xdg-open");
        let (rules, _) = cfg(r#"{"open":[{"kind":"url","run":["br","{url}"]}]}"#);
        assert_eq!(select(&rules, &url), Ok(Some(vec!["br".into(), "https://a.co".into()])));
    }
}
