use std::sync::LazyLock;

use regex::Regex;

use super::super::common::{balanced_paren_end, blank_range, finish};

const C_CPP_GUARD_MAX_BODY_LINES: usize = 40;

fn is_cpp_guard_open(line: &str) -> bool {
    static OPEN_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^[ \t]*#[ \t]*(?:ifdef[ \t]+__cplusplus\b|if[ \t]+defined[ \t]*\(?[ \t]*__cplusplus[ \t]*\)?)",
        )
        .expect("valid C++ guard regex")
    });
    OPEN_RE.is_match(line)
}

pub(in super::super) fn blank_c_cpp_guard_bodies(source: &str) -> String {
    if !source.contains("__cplusplus") {
        return source.to_string();
    }
    let mut lines: Vec<String> = source.split('\n').map(str::to_string).collect();
    let mut index = 0usize;
    while index < lines.len() {
        if !is_cpp_guard_open(lines[index].trim_end_matches('\r')) {
            index += 1;
            continue;
        }
        let mut end = None;
        for offset in 1..=C_CPP_GUARD_MAX_BODY_LINES + 1 {
            let candidate = index + offset;
            if candidate >= lines.len() {
                break;
            }
            let line = lines[candidate]
                .trim_end_matches('\r')
                .trim_start_matches([' ', '\t']);
            if let Some(directive) = line.strip_prefix('#') {
                if directive
                    .trim_start_matches([' ', '\t'])
                    .starts_with("endif")
                {
                    end = Some(candidate);
                }
                break;
            }
        }
        let Some(end) = end else {
            index += 1;
            continue;
        };
        for line in &mut lines[index + 1..end] {
            let mut bytes = line.as_bytes().to_vec();
            blank_range(&mut bytes, 0, line.len());
            *line = finish(bytes);
        }
        index = end + 1;
    }
    lines.join("\n")
}

static C_SANDWICH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:static|extern|inline)([ \t]+)(noinline_for_stack|nokprobe_inline|noinline|notrace|noinstr)\b([ \t]+[A-Za-z_])")
        .expect("valid C annotation sandwich regex")
});

pub(in super::super) fn blank_c_sandwiched_annotations(source: &str) -> String {
    blank_capture(source, &C_SANDWICH_RE, 2)
}

static C_AUTO_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(auto)([ \t]+[A-Za-z_]\w*[ \t]*=)").expect("valid C auto regex")
});

pub(in super::super) fn blank_c_auto_inference(source: &str) -> String {
    blank_capture(source, &C_AUTO_RE, 1)
}

static C_TRAILING_PARAM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[A-Za-z_]\w*[ \t]+([A-Z][A-Z0-9_]{2,})[ \t]*([,)])")
        .expect("valid C trailing parameter annotation regex")
});

pub(in super::super) fn blank_c_trailing_param_attr_macros(source: &str) -> String {
    blank_capture(source, &C_TRAILING_PARAM_RE, 1)
}

fn blank_capture(source: &str, regex: &Regex, group: usize) -> String {
    let mut bytes = source.as_bytes().to_vec();
    for captures in regex.captures_iter(source) {
        if let Some(value) = captures.get(group) {
            blank_range(&mut bytes, value.start(), value.end());
        }
    }
    finish(bytes)
}

const C_KERNEL_ANNOTATIONS: &[&str] = &[
    "__init",
    "__exit",
    "__initdata",
    "__initconst",
    "__exitdata",
    "__devinit",
    "__devexit",
    "__cpuinit",
    "__meminit",
    "__meminitdata",
    "__net_init",
    "__net_exit",
    "__net_initdata",
    "__init_or_module",
    "__user",
    "__kernel",
    "__iomem",
    "__percpu",
    "__rcu",
    "__force",
    "__nocast",
    "__must_check",
    "__maybe_unused",
    "__always_unused",
    "__used",
    "__cold",
    "__hot",
    "__weak",
    "__pure",
    "__sched",
    "__malloc",
    "__visible",
    "__deprecated",
    "__ro_after_init",
    "__read_mostly",
    "__refdata",
    "__latent_entropy",
    "__randomize_layout",
    "__no_randomize_layout",
    "__bpf_kfunc",
    "__function_aligned",
    "__always_inline",
    "__noreturn",
    "__cacheline_aligned_in_smp",
    "__cacheline_aligned",
    "__cacheline_internodealigned_in_smp",
    "____cacheline_aligned_in_smp",
    "____cacheline_aligned",
    "____cacheline_internodealigned_in_smp",
    "__noclone",
    "__lockfunc",
    "__ref",
    "__private",
    "__bitwise",
    "__nosavedata",
    "__no_kcsan",
    "__cpuidle",
    "__ksym",
    "__initdata_memblock",
    "__initdata_or_meminfo",
];

static C_KERNEL_RE: LazyLock<Regex> = LazyLock::new(|| {
    let mut names = C_KERNEL_ANNOTATIONS.to_vec();
    names.sort_unstable_by_key(|name| std::cmp::Reverse(name.len()));
    Regex::new(&format!(r"\b({})\b", names.join("|"))).expect("valid C kernel annotation regex")
});
static CONTAINER_OF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bcontainer_of\s*\([^;()]*?,\s*(struct|union)(\s+)")
        .expect("valid container_of regex")
});

pub(in super::super) fn blank_c_kernel_annotations(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    for captures in C_KERNEL_RE.captures_iter(source) {
        let Some(name) = captures.get(1) else {
            continue;
        };
        let after = &source[name.end()..];
        if after
            .trim_start_matches(char::is_whitespace)
            .starts_with('(')
        {
            continue;
        }
        blank_range(&mut bytes, name.start(), name.end());
    }
    let intermediate = finish(bytes);
    blank_capture(&intermediate, &CONTAINER_OF_RE, 1)
}

const C_PARAMETERIZED_ANNOTATIONS: &[&str] = &[
    "__free",
    "__printf",
    "__scanf",
    "__counted_by",
    "__counted_by_le",
    "__counted_by_be",
    "__guarded_by",
    "__pt_guarded_by",
    "__acquires",
    "__releases",
    "__must_hold",
    "__cleanup",
    "__aligned",
    "__section",
    "__bpf_md_ptr",
    "__assume_aligned",
];

static C_PARAMETERIZED_HEAD_RE: LazyLock<Regex> = LazyLock::new(|| {
    let mut names = C_PARAMETERIZED_ANNOTATIONS.to_vec();
    names.sort_unstable_by_key(|name| std::cmp::Reverse(name.len()));
    Regex::new(&format!(r"\b(?:{})[ \t]*\(", names.join("|")))
        .expect("valid parameterized C annotation regex")
});

pub(in super::super) fn blank_c_parameterized_annotation_macros(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    for matched in C_PARAMETERIZED_HEAD_RE.find_iter(source) {
        let open = matched.end() - 1;
        let Some(mut end) = balanced_paren_end(source, open) else {
            continue;
        };
        let line_start = source[..matched.start()]
            .rfind('\n')
            .map_or(0, |pos| pos + 1);
        if source[line_start..matched.start()]
            .bytes()
            .all(|byte| matches!(byte, b' ' | b'\t'))
        {
            let remaining = &source[end..];
            let whitespace = remaining
                .bytes()
                .take_while(|byte| matches!(byte, b' ' | b'\t'))
                .count();
            if remaining.as_bytes().get(whitespace) == Some(&b';') {
                end += whitespace + 1;
            }
        }
        blank_range(&mut bytes, matched.start(), end);
    }
    finish(bytes)
}
