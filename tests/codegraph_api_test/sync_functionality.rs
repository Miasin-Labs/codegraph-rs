mod sync_functionality {
    use super::*;

    include!("sync_functionality/changes.rs");
    include!("sync_functionality/indexing.rs");
    include!("sync_functionality/repair.rs");
    include!("sync_functionality/search.rs");
}
