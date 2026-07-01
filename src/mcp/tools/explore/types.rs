use std::collections::{HashMap, HashSet};

use crate::types::Node;

/// Seed sets selected directly by the query and by nearby glue symbols.
pub(in crate::mcp::tools::explore) struct ExploreSeeds {
    pub glue_node_ids: HashSet<String>,
    pub named_seed_ids: HashSet<String>,
}

/// Nodes from one file plus the file-level relevance score used for ranking.
pub(in crate::mcp::tools::explore) struct FileGroup {
    pub nodes: Vec<Node>,
    pub score: i64,
}

/// Ranked file set and the intermediate sets that explain why files were kept.
pub(in crate::mcp::tools::explore) struct RankedExploreFiles {
    pub file_order: Vec<String>,
    pub file_groups: HashMap<String, FileGroup>,
    pub entry_node_ids: HashSet<String>,
    pub connected_to_entry: HashSet<String>,
    pub central_files: HashSet<String>,
    pub sorted_files: Vec<String>,
}

/// Rendered source section ready to insert into a codegraph_explore response.
pub(in crate::mcp::tools::explore) struct RenderedFile {
    pub header: String,
    pub language: String,
    pub body: String,
    pub cost: usize,
}
