mod apex;
mod exact;
mod file;
mod fixture;
mod fuzzy;
mod qualified;
mod receiver;

use std::collections::HashMap;

use fixture::{Fixture, make_ref, node};

use super::receiver::normalize_cpp_type_name;
use super::support::split_camel_case;
use super::{match_by_exact_name, match_by_qualified_name, match_method_call, match_reference};
use crate::resolution::types::{ImportMapping, ResolutionContext, ResolvedBy, UnresolvedRef};
use crate::types::{EdgeKind, Language, Node, NodeKind};
