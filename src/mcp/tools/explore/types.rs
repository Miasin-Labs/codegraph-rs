use std::collections::{HashMap, HashSet};

use crate::types::Node;

pub(in crate::mcp::tools::explore) struct ExploreSeeds {
    pub glue_node_ids: HashSet<String>,
    pub named_seed_ids: HashSet<String>,
}

pub(in crate::mcp::tools::explore) struct FileGroup {
    pub nodes: Vec<Node>,
    pub score: i64,
}

pub(in crate::mcp::tools::explore) struct RankedExploreFiles {
    pub file_order: Vec<String>,
    pub file_groups: HashMap<String, FileGroup>,
    pub entry_node_ids: HashSet<String>,
    pub connected_to_entry: HashSet<String>,
    pub central_files: HashSet<String>,
    pub sorted_files: Vec<String>,
}

pub(in crate::mcp::tools::explore) struct RenderedFile {
    pub header: String,
    pub language: String,
    pub body: String,
    pub cost: usize,
}
