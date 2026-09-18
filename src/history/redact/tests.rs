use super::*;

/// `(input, secret that must disappear, context that must survive)`.
/// Token-shaped literals are split with `concat!` so the repo's pre-commit
/// secret scan doesn't mistake this table for leaked credentials.
const SECRETS: &[(&str, &str, &str)] = &[
    // Case-insensitive keys, every occurrence.
    ("export Password=hunter2x", "hunter2x", "export Password="),
    (
        "PASSWORD=aa1x password=bb2y PassWord=cc3z",
        "bb2y",
        "PassWord=",
    ),
    (
        "PASSWORD=aa1x password=bb2y PassWord=cc3z",
        "cc3z",
        "PASSWORD=",
    ),
    (
        "GITHUB_TOKEN='quoted value' gh pr list",
        "quoted value",
        "gh pr list",
    ),
    (
        "PGPASSWORD=pg-pw-9 psql -h db -U app",
        "pg-pw-9",
        "psql -h db -U app",
    ),
    (
        "AWS_SECRET_ACCESS_KEY=abc/def+ghi aws s3 ls",
        "abc/def+ghi",
        "aws s3 ls",
    ),
    (
        "spring.datasource.password = s3cr3t",
        "s3cr3t",
        "spring.datasource.password",
    ),
    // sudo -S fed on stdin.
    (
        "echo 'S3cretPw!' | sudo -S apt install foo",
        "S3cretPw!",
        "| sudo -S apt install foo",
    ),
    (
        "cd /x && echo hunter2 | sudo -kS systemctl restart x",
        "hunter2",
        "sudo -kS systemctl",
    ),
    (
        "ssh box \"echo pw123abc | sudo -S reboot\"",
        "pw123abc",
        "sudo -S reboot",
    ),
    (
        "printf '%s\\n' 'Pw0rd99' | sudo --stdin true",
        "Pw0rd99",
        "sudo --stdin true",
    ),
    (
        "sudo -S apt update <<< 'herestring-pw'",
        "herestring-pw",
        "sudo -S apt update",
    ),
    // Tools that take a password on the command line.
    ("sshpass -p 'topsecret' ssh user@host", "topsecret", "ssh"),
    ("sshpass -ptopsecret2 scp a b:", "topsecret2", "scp a b:"),
    ("mysql -u root -pS3cr3tDB appdb", "S3cr3tDB", "-u root"),
    ("mysqldump --user=x -p'q uoted' db", "q uoted", "mysqldump"),
    ("tool --password hunter3 --verbose", "hunter3", "--verbose"),
    ("tool --TOKEN=tok-value-1", "tok-value-1", "--TOKEN="),
    ("tool --api-key=\"k e y\" run", "k e y", "run"),
    // A truncated command with an unterminated quote fails closed.
    (
        "login --password 'unterminated secret",
        "unterminated secret",
        "login --password",
    ),
    ("TOKEN=\"cut off here", "cut off here", "TOKEN="),
    // Headers and URLs.
    (
        "curl -H 'Authorization: Bearer abc.def.ghi123' https://x",
        "abc.def.ghi123",
        "Authorization: Bearer",
    ),
    (
        "curl -H \"authorization: token tokval42\" x",
        "tokval42",
        "authorization: token",
    ),
    (
        "curl -H 'Authorization: Basic dXNlcjpwYXNz' x",
        "dXNlcjpwYXNz",
        "Authorization: Basic",
    ),
    (
        "curl -H 'X-Api-Key: k3y-v4lue' https://api",
        "k3y-v4lue",
        "X-Api-Key:",
    ),
    (
        "http GET api bearer   opaque_token_77",
        "opaque_token_77",
        "bearer",
    ),
    (
        "git clone https://user:passw0rd@github.com/o/r",
        "passw0rd",
        "https://user:",
    ),
    (
        "psql postgres://admin:pa55word@db:5432/app",
        "pa55word",
        "@db:5432/app",
    ),
    // Vendor tokens.
    (
        "export K=sk-ant-api03-AbCdEfGhIjKlMnOpQrStUv",
        "AbCdEfGhIjKlMnOpQrStUv",
        "export K=",
    ),
    (
        "key sk-proj-abcdefghijklmnop1234 used",
        "abcdefghijklmnop1234",
        "used",
    ),
    (
        concat!("sk_", "live_4eC39HqLyjWDarjtT1zdp7dc charge"),
        "4eC39HqLyjWDarjtT1zdp7dc",
        "charge",
    ),
    (
        concat!("gh auth gh", "p_0123456789abcdefghijABCDEFGHIJ"),
        "ghp_0123456789abcdefghij",
        "gh auth",
    ),
    (
        concat!("gh", "o_0123456789abcdefghijABCDEFGHIJ"),
        "o_0123456789abcdefghij",
        "",
    ),
    (
        "github_pat_11ABCDEFG0123456789_abcdefghijKLMNOP",
        "github_pat_11ABCDEFG",
        "",
    ),
    (
        "slack xoxb-1234567890-abcdefghij",
        "xoxb-1234567890",
        "slack",
    ),
    (
        "slack xoxp-1234567890-abcdefghij",
        "xoxp-1234567890",
        "slack",
    ),
    (
        concat!("aws AK", "IAIOSFODNN7EXAMPLE s3"),
        "IAIOSFODNN7EXAMPLE",
        "aws",
    ),
    (
        "maps AIzaSyA-1234567890abcdefghijklmnopqrstu x",
        "AIzaSyA-1234567890",
        "maps",
    ),
    (
        "glpat-abcdefghij0123456789 in ci",
        "glpat-abcdefghij0123456789",
        "in ci",
    ),
    // Structured blobs.
    (
        concat!(
            "key: -----BEGIN RSA ",
            "PRIVATE KEY-----\\nMIIEpAIBAAKCAQEA\\n-----END RSA PRIVATE KEY----- done"
        ),
        "MIIEpAIBAAKCAQEA",
        "done",
    ),
    (
        concat!(
            "-----BEGIN OPENSSH ",
            "PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjE\n(truncated"
        ),
        "b3BlbnNzaC1rZXktdjE",
        "",
    ),
    (
        "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpM tail",
        "eyJzdWIiOiIxMjM0NTY3ODkwIn0",
        "tail",
    ),
    (
        "{\"password\": \"json-pw\", \"user\": \"u\"}",
        "json-pw",
        "\"user\": \"u\"",
    ),
    (
        "{\\\"client_secret\\\": \\\"esc-secret\\\"}",
        "esc-secret",
        "client_secret",
    ),
    ("db_password: yaml-pw-1", "yaml-pw-1", "db_password:"),
    // Emails keep their domain.
    (
        "mail john.doe@example.com now",
        "john.doe",
        "@example.com now",
    ),
    // High-entropy runs no rule names.
    (
        "blob Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5MEFCQ0RFRg== end",
        "Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5MEFCQ0RFRg",
        "end",
    ),
    (
        "sha 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        "9f86d081884c7d659a2feaa0c55ad015",
        "sha",
    ),
    (
        "k=oHBvRPOIvGrv5iFlbCBFNOgmBjMtpsiaOclRz3Aw next",
        "oHBvRPOIvGrv5iFlbCBF",
        "k=",
    ),
];

/// Commands and paths that must pass through untouched.
const BENIGN: &[&str] = &[
    "cargo test -p codegraph-rs --lib",
    "git log --oneline -20 && git status",
    "grep -rn 'fn parse' src/history/",
    "/home/cole/RustProjects/active/codegraph-rs/src/history/redact.rs",
    "cd /repo && cargo build --release 2>&1 | tail -5",
    "max_tokens=4096 temperature=0.2",
    "550e8400-e29b-41d4-a716-446655440000",
    "target/x86_64-unknown-linux-gnu-release-build-output-directory/deps",
    "mysql -u root -p mydb",
    "sudo apt install ripgrep && sudo -s",
    "echo hello | tee out.txt",
    "ls ~/.cache/huggingface/hub/models--bert-base-uncased/snapshots",
    "python3 -m pytest tests/test_redaction_of_long_identifier_names.py -k token",
    "--password-file /run/secrets/db --token-file x",
    "kubectl get secrets -n kube-system",
    "rg 'Authorization' src && rg bearer",
    "echo $PASSWORD_FILE",
    "ses_20260810_141304",
];

#[test]
fn every_secret_shape_is_masked_and_context_kept() {
    for (input, secret, context) in SECRETS {
        let (out, hit) = redact(input);
        assert!(hit, "not flagged: {input:?} -> {out:?}");
        assert!(
            !out.contains(secret),
            "secret survived: {input:?} -> {out:?}"
        );
        assert!(out.contains(context), "context lost: {input:?} -> {out:?}");
        assert!(out.contains("<REDACTED"), "no mask: {out:?}");
    }
}

#[test]
fn benign_text_passes_through() {
    for input in BENIGN {
        let (out, hit) = redact(input);
        assert!(!hit, "false positive: {input:?} -> {out:?}");
        assert_eq!(&out, input);
    }
}

#[test]
fn redaction_is_idempotent() {
    for input in SECRETS.iter().map(|s| s.0).chain(BENIGN.iter().copied()) {
        let (once, _) = redact(input);
        let (twice, hit) = redact(&once);
        assert_eq!(once, twice, "not idempotent for {input:?}");
        assert!(!hit, "second pass re-flagged {once:?}");
    }
}

// ─── generated shapes ────────────────────────────────────────────────────────

/// Deterministic xorshift64* — property-style coverage without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }

    fn string(&mut self, alphabet: &[u8], len: usize) -> String {
        (0..len)
            .map(|_| alphabet[self.below(alphabet.len())] as char)
            .collect()
    }

    /// Flip the case of each ASCII letter at random.
    fn jumble_case(&mut self, s: &str) -> String {
        s.chars()
            .map(|c| {
                if self.below(2) == 0 {
                    c.to_ascii_uppercase()
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect()
    }
}

const ALNUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const HEX: &[u8] = b"0123456789abcdef";
/// base64url plus `+` (a `/` would split the token: the sweep judges path segments).
const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-_";
/// Password characters: printable, no whitespace/quotes/shell separators.
const PW: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!#%*+.^_~-";

/// A random password that starts alphanumeric (so it never reads as a flag).
fn password(rng: &mut Rng) -> String {
    let len = 8 + rng.below(16);
    let mut pw = rng.string(ALNUM, 1);
    pw.push_str(&rng.string(PW, len));
    pw
}

/// Contexts a *password* shows up in; `{}` is the password.
const PASSWORD_CONTEXTS: &[&str] = &[
    "echo {} | sudo -S apt-get install -y jq",
    "cd /srv && echo '{}' | sudo -S systemctl restart app",
    "printf '%s\\n' {} | sudo -S -k true",
    "sudo -S reboot <<< {}",
    "sshpass -p {} ssh admin@10.0.0.2",
    "sshpass -p'{}' rsync -a x y:",
    "mysql -h db -u root -p{} shop",
    "PGPASSWORD={} psql -h db",
    "export {key}={}",
    "{key}=\"{}\" ./run.sh",
    "tool --{flag} {} --dry-run",
    "tool --{flag}={}",
    "curl -H 'Authorization: Bearer {}' https://api.example.com/v1",
    "curl -H 'X-Api-Key: {}' https://api.example.com/v1",
    "git push https://deploy:{}@git.example.com/o/r.git",
    "{\"{key}\": \"{}\"}",
];

const SECRET_KEYS: &[&str] = &[
    "password",
    "passwd",
    "db_password",
    "secret",
    "client_secret",
    "api_key",
    "apikey",
    "access_token",
    "auth_token",
    "github_token",
    "aws_secret_access_key",
    "private_key",
    "pgpassword",
    "sshpass",
    "credentials",
];

const SECRET_FLAGS: &[&str] = &[
    "password",
    "passwd",
    "token",
    "secret",
    "api-key",
    "apikey",
    "access-token",
    "client-secret",
];

#[test]
fn generated_passwords_never_survive_their_context() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..2_000 {
        let pw = password(&mut rng);
        let key = rng.pick(SECRET_KEYS);
        let key = rng.jumble_case(key);
        let flag = rng.pick(SECRET_FLAGS);
        let flag = rng.jumble_case(flag);
        let template = rng.pick(PASSWORD_CONTEXTS);
        let input = template
            .replace("{key}", &key)
            .replace("{flag}", &flag)
            .replace("{}", &pw);
        let (out, hit) = redact(&input);
        assert!(
            hit && !out.contains(&pw),
            "leaked {pw:?}: {input:?} -> {out:?}"
        );
        assert_eq!(
            redact(&out),
            (out.clone(), false),
            "not idempotent: {out:?}"
        );
    }
}

/// Generators for self-identifying secrets that may appear anywhere.
fn token(rng: &mut Rng) -> String {
    match rng.below(9) {
        0 => format!("sk-ant-api03-{}", rng.string(ALNUM, 40)),
        1 => format!("sk-proj-{}", rng.string(ALNUM, 32)),
        2 => format!(
            "gh{}_{}",
            rng.pick(&["p", "o", "u", "s", "r"]),
            rng.string(ALNUM, 36)
        ),
        3 => format!("github_pat_{}", rng.string(ALNUM, 60)),
        4 => format!(
            "xox{}-{}-{}",
            rng.pick(&["b", "p"]),
            rng.string(HEX, 12),
            rng.string(ALNUM, 24)
        ),
        5 => format!(
            "AKIA{}",
            rng.string(b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567", 16)
        ),
        6 => format!("AIza{}", rng.string(ALNUM, 35)),
        7 => format!(
            "eyJ{}.{}.{}",
            rng.string(ALNUM, 30),
            rng.string(ALNUM, 40),
            rng.string(ALNUM, 43)
        ),
        _ => {
            // A bare random run no prefix rule knows: the entropy sweep's job.
            let len = 32 + rng.below(48);
            if rng.below(2) == 0 {
                let mut s = rng.string(HEX, len);
                s.replace_range(0..1, "7");
                s.replace_range(1..2, "c");
                s
            } else {
                let mut s = rng.string(B64, len);
                s.replace_range(0..2, "Q9");
                s
            }
        }
    }
}

const TOKEN_CONTEXTS: &[&str] = &[
    "{}",
    "run {} now",
    "x={}",
    "'{}'",
    "curl -d token={} https://x",
    "echo \"{}\" > /tmp/k",
    "config set key {} --global",
];

#[test]
fn generated_tokens_are_masked_anywhere() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for _ in 0..2_000 {
        let tok = token(&mut rng);
        let input = rng.pick(TOKEN_CONTEXTS).replace("{}", &tok);
        let (out, hit) = redact(&input);
        assert!(hit, "not flagged: {input:?}");
        // No 12-char window of the token may survive.
        let window = &tok[tok.len() / 2..(tok.len() / 2 + 12).min(tok.len())];
        assert!(
            !out.contains(window),
            "leaked {tok:?}: {input:?} -> {out:?}"
        );
        assert_eq!(
            redact(&out),
            (out.clone(), false),
            "not idempotent: {out:?}"
        );
    }
}

const WORDS: &[&str] = &[
    "src",
    "lib",
    "main",
    "history",
    "redact",
    "graph",
    "index",
    "target",
    "debug",
    "release",
    "tests",
    "fixtures",
    "config",
    "home",
    "cole",
    "projects",
    "active",
    "codegraph",
    "worktrees",
    "agent",
    "analysis",
    "cache",
    "build",
    "server",
    "client",
    "handlers",
    "registry",
    "schema",
];

#[test]
fn generated_paths_and_commands_are_left_alone() {
    let mut rng = Rng(0x0123_4567_89AB_CDEF);
    for _ in 0..2_000 {
        let depth = 2 + rng.below(8);
        let mut path = String::new();
        for _ in 0..depth {
            path.push('/');
            path.push_str(rng.pick(WORDS));
            if rng.below(3) == 0 {
                path.push(rng.pick(&["_", "-"]).chars().next().unwrap_or('_'));
                path.push_str(rng.pick(WORDS));
            }
        }
        path.push_str(rng.pick(&[".rs", ".py", ".toml", ".md", ""]));
        let cmd = match rng.below(4) {
            0 => format!(
                "cargo test -p {}-{} --lib",
                rng.pick(WORDS),
                rng.pick(WORDS)
            ),
            1 => format!("grep -rn {} {path}", rng.pick(WORDS)),
            2 => format!("cd {path} && git status && git diff HEAD~{}", rng.below(9)),
            _ => path.clone(),
        };
        let (out, hit) = redact(&cmd);
        assert!(!hit, "false positive: {cmd:?} -> {out:?}");
    }
}
