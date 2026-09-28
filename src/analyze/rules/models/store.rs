//! The models' on-disk form: one tab-separated line per model, `#` lines
//! are comments.
//!
//! ```text
//! role  namespace  type  subtypes  name  arity  input  output  kind  flags
//! sink  java.sql   Statement  1  executeQuery  -  arg0  -  sql-injection  -
//! ```
//!
//! `subtypes` is `0`/`1`, `arity`, `input` and `output` are `-` when
//! absent, positions are [`Pos::render`]'s, and `flags` holds `g`
//! (generated), `a` (approximate), `T`/`F` (a guard's accepting value), or
//! `-`.

use super::{Model, Pos, Role};

/// The column names, as the header line writes them.
const HEADER: &str = "# role\tnamespace\ttype\tsubtypes\tname\tarity\tinput\toutput\tkind\tflags";

/// Models read from `text`; malformed lines are skipped.
pub fn parse_models(text: &str) -> Vec<Model> {
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(parse_line)
        .collect()
}

fn parse_line(line: &str) -> Option<Model> {
    let fields: Vec<&str> = line.split('\t').collect();
    if fields.len() != 10 {
        return None;
    }
    let position = |text: &str| -> Option<Option<Pos>> {
        if text == "-" {
            Some(None)
        } else {
            Pos::parse(text).map(Some)
        }
    };
    let flags = fields[9];
    Some(Model {
        role: Role::parse(fields[0])?,
        namespace: fields[1].to_string(),
        type_name: fields[2].to_string(),
        subtypes: fields[3] == "1",
        name: fields[4].to_string(),
        arity: match fields[5] {
            "-" => None,
            n => Some(n.parse().ok()?),
        },
        input: position(fields[6])?,
        output: position(fields[7])?,
        kind: fields[8].to_string(),
        accepting: if flags.contains('T') {
            Some(true)
        } else if flags.contains('F') {
            Some(false)
        } else {
            None
        },
        generated: flags.contains('g'),
        approximate: flags.contains('a'),
    })
}

/// `models` as lines, after `preamble` (each line becomes a `#` comment).
pub fn write_models(preamble: &str, models: &[Model]) -> String {
    let mut out = String::new();
    for line in preamble.lines() {
        out.push_str("# ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(HEADER);
    out.push('\n');
    for model in models {
        let position = |pos: &Option<Pos>| pos.as_ref().map_or("-".to_string(), Pos::render);
        let mut flags = String::new();
        if model.generated {
            flags.push('g');
        }
        if model.approximate {
            flags.push('a');
        }
        match model.accepting {
            Some(true) => flags.push('T'),
            Some(false) => flags.push('F'),
            None => {}
        }
        if flags.is_empty() {
            flags.push('-');
        }
        let fields = [
            model.role.as_str().to_string(),
            model.namespace.clone(),
            model.type_name.clone(),
            if model.subtypes { "1" } else { "0" }.to_string(),
            model.name.clone(),
            model.arity.map_or("-".to_string(), |n| n.to_string()),
            position(&model.input),
            position(&model.output),
            model.kind.clone(),
            flags,
        ];
        out.push_str(&fields.join("\t"));
        out.push('\n');
    }
    out
}
