//! Types outside the project, as the graphs the project can reach declare
//! them — the seam receiver inference types chains through.
//!
//! In-project resolution never sees these: its contexts return `None` from
//! [`crate::resolution::ResolutionContext::foreign_types`], so a chain ends
//! at the first type the project does not define, exactly as before. The
//! external resolution pass ([`crate::resolution::external`]) wraps the
//! project's context with one that answers from the dependency shards and
//! linked projects' indexes: `conn.prepare(sql)?.query_map(..)` then reads
//! `Connection::prepare`'s declared return type from rusqlite's graph,
//! unwraps it to `Statement`, and resolves `query_map` there.
//!
//! Every answer must be unique in its graph: a method or field several
//! same-named owners declare (or none) is `None`, which ends the chain.

/// A type as a crate outside the project writes it: a method's declared
/// return type or a field's declared type, and where it is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignText {
    /// The type as written (`Result<Statement<'_>>`).
    pub text: String,
    /// The crate it is written in, by the name the project's code uses for
    /// it (`rusqlite`, `tree_sitter`).
    pub krate: String,
    /// The file it is written in, relative to that graph's root.
    pub file: String,
}

/// Declarations of crates outside the project, by the name code uses for
/// each crate.
pub trait ForeignTypes {
    /// The declared return type of the method `method` of the type at
    /// `owner` (its path inside `krate`, the type's name last).
    fn method_return(&self, krate: &str, owner: &[String], method: &str) -> Option<ForeignText>;
    /// The declared return type of the free fn at `path` (the segments
    /// after the crate: `["de", "from_str"]`) of `krate`.
    fn fn_return(&self, krate: &str, path: &[String]) -> Option<ForeignText>;
    /// The declared type of the public field `field` of the type at `owner`
    /// of `krate`.
    fn field_type(&self, krate: &str, owner: &[String], field: &str) -> Option<ForeignText>;
    /// What the type path `path`, written in `file` of `krate`, names:
    /// the crate that defines it and the type's path inside that crate —
    /// `None` when no reachable graph defines such a type (a std type, a
    /// generic).
    fn resolve_type(&self, krate: &str, file: &str, path: &str) -> Option<(String, Vec<String>)>;
}
