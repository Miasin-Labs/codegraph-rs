//! Domain-specific language for graph queries.
//!
//! Grammar (legacy pipe-chain — preserved for back-compat):
//! ```text
//! query       := op ( '|' op )*
//! op          := fn_select | type_select | callers | callees | depth | filter
//!              | show | taint | preconditions | since | hot | scc
//!              | dispatch | cluster_by_type | affected | co_changes
//!              | reachable
//! fn_select   := 'fn' '(' STRING ')'
//! type_select := 'type' '(' STRING ')'
//! callers     := 'callers'
//! callees     := 'callees'
//! depth       := 'depth' NUMBER
//! filter      := 'filter' 'kind' '=' IDENT
//! show        := 'show' PROJECTION
//! taint       := 'taint' STRING
//! since       := 'since' NUMBER
//! hot         := 'hot' NUMBER
//! scc         := 'scc'
//! dispatch    := 'dispatch'
//! cluster_by_type := 'cluster' 'by' 'type'
//! affected    := 'affected' NUMBER 'since' NUMBER
//! reachable   := 'reachable' 'via' STRING ( 'incoming' | 'outgoing' )?
//! ```
//!
//! The `reachable` STRING is a label-constraint pattern — whitespace-
//! separated edge labels, each with an optional `*` / `+` repetition
//! (`"Contains Calls+"`, `"any*"`); see
//! [`crate::label_reachability::parse_pattern`]. The op expands the working
//! set to every node reachable via a path whose edge-label *sequence*
//! matches the pattern (label-constrained reachability), in the given
//! direction (default `outgoing`; `incoming` answers "who reaches me").
//!
//! Extended grammar (set algebra, path patterns, entrypoint selector,
//! dominator queries, trait/cluster selectors, multi-source path):
//! ```text
//! expr        := setop_expr
//! setop_expr  := atom ( ( 'union' | 'intersect' | 'diff' | '\' ) atom )*
//! atom        := pipe_chain | path_query | entrypoint_query
//!              | dominators_query | dominates_query | trait_impls_query
//!              | multi_path_query | '(' expr ')'
//! pipe_chain  := op ( '|' op )*
//! path_query  := ( 'path' | 'paths' ) atom '->' atom
//!                ( 'where' 'intermediate' 'kind' '=' IDENT )?
//!                ( 'via' EDGE_KIND )?
//!                ( 'depth' NUMBER )?
//! entrypoint_query := 'entrypoints' ( 'kind' '=' IDENT )?
//! dominators_query := 'dominators' 'of' atom
//! dominates_query  := 'dominates' atom
//! trait_impls_query := 'trait_impls' 'of' atom
//! multi_path_query := 'multi_path' '{' atom ( ',' atom )* '}' '->' atom
//!                     ( 'depth' NUMBER )?
//! ```
//!
//! Set-op precedence: all three (`union`, `intersect`, `diff`/`\`) share
//! the same precedence level and are left-associative. Use parentheses
//! to disambiguate. Set ops only preserve the `nodes` field of each
//! operand's [`QueryResult`]; edges, cycle records, and other metadata
//! are intentionally dropped because there is no meaningful merge
//! semantics across heterogenous traversals.

mod ast;
mod error;
mod executor;
mod lexer;
mod parser;

pub use ast::*;
pub use error::ParseError;
pub use executor::*;
pub use lexer::lex;
pub use parser::{parse, parse_expr, parse_query};

#[cfg(test)]
mod tests;
