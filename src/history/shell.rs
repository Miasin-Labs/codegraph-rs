//! Just enough shell parsing to profile a command line: the command word of
//! each pipeline/list segment, and the `cd` targets that lead it.
//!
//! Not a shell grammar. Quote-aware splitting on `;`, newlines, `|`, `||`,
//! `&&` and a backgrounding `&` (never the `&` of `2>&1`/`&>`), with
//! `$(…)`/backtick bodies kept inside their segment and heredoc bodies
//! dropped. Callers pass *redacted* text: the words come out of it verbatim.

/// Longest chain kept (segments beyond it are dropped).
const MAX_CHAIN: usize = 16;

/// Set-up commands that precede the command a line is for.
const PREAMBLE: &[&str] = &[
    "cd", "pushd", "popd", "echo", "printf", "sleep", "export", "unset", "source", ".", "set",
    "true", ":", "clear",
];

/// Tokens scanned per segment when looking for its command word.
const MAX_TOKENS: usize = 48;

/// A command line reduced to its command words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CommandProfile {
    /// The command word of each segment, in order.
    words: Vec<String>,
    /// Targets of the `cd`/`pushd` segments that precede the first real
    /// command (`cd /repo && cargo test` → `["/repo"]`).
    leading_cds: Vec<String>,
}

impl CommandProfile {
    pub(crate) fn parse(cmd: &str) -> Self {
        let body = strip_heredocs(cmd);
        let mut profile = Self::default();
        let mut leading = true;
        for seg in split_segments(&body) {
            let tokens = tokenize(seg);
            let Some((word, args)) = command_word(&tokens) else {
                continue;
            };
            let is_cd = matches!(word.as_str(), "cd" | "pushd");
            if leading && is_cd {
                if let Some(target) = args.first() {
                    profile.leading_cds.push(target.clone());
                }
            } else if !is_cd {
                leading = false;
            }
            profile.words.push(word);
        }
        profile
    }

    /// The command the line is *for*: the first word that isn't set-up
    /// (`cd /repo && cargo test` → `cargo`, `echo pw | sudo -S apt …` →
    /// `apt`); the set-up word only when that's all the line does.
    pub(crate) fn primary(&self) -> Option<&str> {
        self.words
            .iter()
            .find(|w| !PREAMBLE.contains(&w.as_str()))
            .or_else(|| self.words.first())
            .map(String::as_str)
    }

    /// Normalized `word | word` chain, consecutive repeats collapsed.
    pub(crate) fn chain(&self) -> Option<String> {
        let mut chain: Vec<&str> = Vec::new();
        for w in &self.words {
            if chain.last() != Some(&w.as_str()) {
                chain.push(w);
            }
            if chain.len() == MAX_CHAIN {
                break;
            }
        }
        (!chain.is_empty()).then(|| chain.join(" | "))
    }

    pub(crate) fn leading_cds(&self) -> &[String] {
        &self.leading_cds
    }
}

/// The command word and argument tokens of each segment of `cmd`, heredoc
/// bodies dropped (`cd /r && cargo test -p x` → `[("cd", ["/r"]), ("cargo",
/// ["test", "-p", "x"])]`). Redirection tokens stay in the arguments.
pub(crate) fn command_segments(cmd: &str) -> Vec<(String, Vec<String>)> {
    let body = strip_heredocs(cmd);
    split_segments(&body)
        .into_iter()
        .filter_map(|seg| {
            let tokens = tokenize(seg);
            command_word(&tokens).map(|(word, args)| (word, args.to_vec()))
        })
        .collect()
}

/// Drop heredoc bodies (`<<EOF … EOF`, `<<-'X' … X`): they're data, not commands.
fn strip_heredocs(cmd: &str) -> String {
    let mut out = String::with_capacity(cmd.len());
    let mut lines = cmd.lines();
    while let Some(line) = lines.next() {
        out.push_str(line);
        out.push('\n');
        if let Some(delim) = heredoc_delimiter(line) {
            for body in lines.by_ref() {
                if body.trim() == delim {
                    break;
                }
            }
        }
    }
    out
}

/// The delimiter of a heredoc opened on `line`, if any (`<<<` is a here-string).
fn heredoc_delimiter(line: &str) -> Option<&str> {
    let mut rest = line;
    while let Some(pos) = rest.find("<<") {
        let after = &rest[pos + 2..];
        if after.starts_with('<') {
            rest = after.trim_start_matches('<');
            continue;
        }
        let after = after.strip_prefix('-').unwrap_or(after).trim_start();
        let after = after.trim_start_matches(['\'', '"']);
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        if end > 0 {
            return Some(&after[..end]);
        }
        rest = after;
    }
    None
}

/// Split into list/pipeline segments, honouring quotes, escapes, `$(…)` and backticks.
fn split_segments(cmd: &str) -> Vec<&str> {
    let bytes = cmd.as_bytes();
    let mut segs = Vec::new();
    let mut start = 0;
    let mut quote: Option<u8> = None;
    let mut subshell_depth = 0usize;
    let mut in_backtick = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if b == b'\\' && q == b'"' {
                i += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\\' => {
                i += 2;
                continue;
            }
            b'\'' | b'"' => quote = Some(b),
            b'`' => in_backtick = !in_backtick,
            b'$' if bytes.get(i + 1) == Some(&b'(') => {
                subshell_depth += 1;
                i += 2;
                continue;
            }
            b')' if subshell_depth > 0 => subshell_depth -= 1,
            _ if subshell_depth > 0 || in_backtick => {}
            b';' | b'\n' => {
                segs.push(&cmd[start..i]);
                start = i + 1;
            }
            b'|' => {
                segs.push(&cmd[start..i]);
                // `||` and `|&` are single operators.
                let width = if matches!(bytes.get(i + 1), Some(b'|' | b'&')) {
                    2
                } else {
                    1
                };
                i += width;
                start = i;
                continue;
            }
            b'&' => {
                if bytes.get(i + 1) == Some(&b'&') {
                    segs.push(&cmd[start..i]);
                    i += 2;
                    start = i;
                    continue;
                }
                // A backgrounding `&`, not the `&` of `2>&1` or `&>file`.
                let redirect =
                    i > 0 && matches!(bytes[i - 1], b'>' | b'<') || bytes.get(i + 1) == Some(&b'>');
                if !redirect {
                    segs.push(&cmd[start..i]);
                    start = i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if start < cmd.len() {
        segs.push(&cmd[start..]);
    }
    segs.into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whitespace-split with quotes removed and backslash escapes resolved.
fn tokenize(seg: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    let mut chars = seg.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_token = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
                in_token = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_token {
                    tokens.push(std::mem::take(&mut cur));
                    in_token = false;
                    if tokens.len() == MAX_TOKENS {
                        return tokens;
                    }
                }
            }
            (None, c) => {
                cur.push(c);
                in_token = true;
            }
        }
    }
    if in_token {
        tokens.push(cur);
    }
    tokens
}

/// Keywords that introduce the command after them.
const PREFIX_KEYWORDS: &[&str] = &[
    "if", "elif", "then", "else", "do", "while", "until", "!", "{", "(", "time",
];

/// Tokens that end a construct or head a non-command clause: no command word.
const NON_COMMANDS: &[&str] = &[
    "done", "fi", "esac", "}", "for", "case", "select", "function", "in",
];

/// Wrappers that run the command after them, with the options that take a value.
const WRAPPERS: &[(&str, &[&str])] = &[
    (
        "sudo",
        &["-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U"],
    ),
    ("doas", &["-u", "-C"]),
    ("env", &["-u", "-C", "-S"]),
    ("nohup", &[]),
    ("timeout", &["-s", "-k", "--signal", "--kill-after"]),
    ("nice", &["-n"]),
    ("ionice", &["-c", "-n"]),
    ("stdbuf", &["-i", "-o", "-e"]),
    ("exec", &["-a"]),
    ("command", &[]),
    ("builtin", &[]),
    ("xargs", &["-I", "-n", "-P", "-d", "-L", "-a", "-E", "-s"]),
    ("rtk", &[]),
];

/// The command word of a segment and the tokens after it.
fn command_word(tokens: &[String]) -> Option<(String, &[String])> {
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i].trim_start_matches('(').trim_end_matches(')');
        if tok.is_empty() || is_env_assignment(tok) || PREFIX_KEYWORDS.contains(&tok) {
            i += 1;
            continue;
        }
        if tok.starts_with('#') || NON_COMMANDS.contains(&tok) {
            return None;
        }
        if let Some((wrapper, value_opts)) = WRAPPERS.iter().find(|(w, _)| *w == tok) {
            i += 1;
            while let Some(opt) = tokens.get(i) {
                if value_opts.contains(&opt.as_str()) {
                    i += 2;
                } else if opt.starts_with('-') || is_env_assignment(opt) {
                    i += 1;
                } else {
                    break;
                }
            }
            // `timeout 30 cmd`, `rtk proxy cmd`: skip the positional operand.
            let operand = tokens.get(i).map(String::as_str);
            let skip = match *wrapper {
                "timeout" => operand.is_some_and(|t| t.starts_with(|c: char| c.is_ascii_digit())),
                "rtk" => operand == Some("proxy"),
                _ => false,
            };
            if skip {
                i += 1;
            }
            continue;
        }
        let word = tok
            .rsplit('/')
            .next()
            .filter(|w| !w.is_empty())
            .unwrap_or(tok);
        return Some((word.to_owned(), &tokens[i + 1..]));
    }
    None
}

/// `NAME=value` (a shell variable assignment prefix).
fn is_env_assignment(tok: &str) -> bool {
    let Some((name, _)) = tok.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(cmd: &str) -> Vec<String> {
        CommandProfile::parse(cmd).words
    }

    #[test]
    fn primary_skips_leading_cd() {
        let p = CommandProfile::parse("cd /repo && cargo test -p codegraph-rs 2>&1 | tail -5");
        assert_eq!(p.primary(), Some("cargo"));
        assert_eq!(p.chain().as_deref(), Some("cd | cargo | tail"));
        assert_eq!(p.leading_cds(), ["/repo"]);
        // A bare `cd` is still a `cd`.
        assert_eq!(CommandProfile::parse("cd /tmp").primary(), Some("cd"));
        // Other set-up is skipped the same way.
        assert_eq!(
            CommandProfile::parse("echo '== build ==' && export X=1 && make").primary(),
            Some("make")
        );
        assert_eq!(
            CommandProfile::parse("echo <REDACTED> | sudo -S apt install jq").primary(),
            Some("apt")
        );
        assert_eq!(
            CommandProfile::parse("echo hi > f.txt").primary(),
            Some("echo")
        );
    }

    #[test]
    fn quotes_escapes_and_subshells_do_not_split() {
        assert_eq!(
            words(r#"grep -E "a|b;c" src && echo 'x && y'"#),
            ["grep", "echo"]
        );
        assert_eq!(words(r"echo a\|b | wc -l"), ["echo", "wc"]);
        assert_eq!(words("echo $(date | cut -c1) done"), ["echo"]);
        assert_eq!(words("echo `ls | head`"), ["echo"]);
    }

    #[test]
    fn redirect_ampersands_are_not_separators() {
        assert_eq!(words("cargo build 2>&1 | tee log"), ["cargo", "tee"]);
        assert_eq!(words("make &> out.txt"), ["make"]);
        assert_eq!(words("sleep 5 & wait"), ["sleep", "wait"]);
        assert_eq!(
            words("make || echo failed |& tee x"),
            ["make", "echo", "tee"]
        );
    }

    #[test]
    fn env_assignments_wrappers_and_paths_are_skipped() {
        assert_eq!(
            words("RUST_LOG=debug ./target/debug/codegraph status"),
            ["codegraph"]
        );
        assert_eq!(words("sudo -u postgres psql -c 'select 1'"), ["psql"]);
        assert_eq!(words("timeout 30 cargo test"), ["cargo"]);
        assert_eq!(words("env -u FOO BAR=1 python3 x.py"), ["python3"]);
        assert_eq!(words("rtk proxy git status"), ["git"]);
        assert_eq!(
            words("find . -name '*.rs' | xargs -I{} grep -l foo {}"),
            ["find", "grep"]
        );
        assert_eq!(words("/usr/bin/git log"), ["git"]);
    }

    #[test]
    fn control_flow_and_comments() {
        assert_eq!(
            words("for f in a b; do cat $f; done; if [ -f x ]; then rm x; fi"),
            ["cat", "[", "rm"]
        );
        assert_eq!(words("# just a note\ngrep -n foo src"), ["grep"]);
        assert_eq!(words("(cd sub && make)"), ["cd", "make"]);
    }

    #[test]
    fn heredoc_bodies_are_dropped() {
        let cmd = "cat > x.py <<'EOF'\nimport os; os.system('rm -rf /')\nprint(1) | foo\nEOF\npython3 x.py";
        assert_eq!(words(cmd), ["cat", "python3"]);
        // A here-string is not a heredoc.
        assert_eq!(words("sudo -S true <<< pw\nls"), ["true", "ls"]);
    }

    #[test]
    fn leading_cds_stop_at_the_first_command() {
        let p = CommandProfile::parse("cd /a && cd b; cargo test && cd /elsewhere");
        assert_eq!(p.leading_cds(), ["/a", "b"]);
        assert_eq!(p.primary(), Some("cargo"));
    }

    #[test]
    fn chain_collapses_repeats_and_caps_length() {
        assert_eq!(
            CommandProfile::parse("git add . && git commit -m x && git push")
                .chain()
                .as_deref(),
            Some("git")
        );
        let long = vec!["echo a"; 40].join(" | ls | ");
        let chain = CommandProfile::parse(&long).chain().unwrap();
        assert_eq!(chain.split(" | ").count(), MAX_CHAIN);
        assert_eq!(CommandProfile::parse("   ").chain(), None);
    }
}
