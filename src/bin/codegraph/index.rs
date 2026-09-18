use super::{
    BufRead,
    CodeGraph,
    DEFAULT_SYNC_HOOKS,
    DIM,
    IndexOptions,
    IndexProgress,
    NodeKind,
    OpenOptions,
    RESET,
    RefCell,
    SearchOptions,
    UiIndexProgress,
    Write,
    bold,
    clack_intro,
    clack_log_error,
    clack_log_info,
    clack_log_success,
    clack_log_warn,
    clack_outro,
    create_shimmer_progress,
    cyan,
    detect_worktree_index_mismatch,
    dim,
    error_msg,
    format_duration,
    format_number,
    get_codegraph_dir,
    get_glyphs,
    green,
    info,
    io,
    is_initialized,
    iso_from_epoch_ms,
    js_to_fixed,
    offer_watch_fallback,
    parse_int_js,
    print_index_result,
    process,
    refuse_foreign_index,
    remove_git_sync_hook,
    resolve_absolute,
    resolve_index_path,
    resolve_project_path,
    resolve_project_path_quiet,
    resolve_writable_project_path,
    run_index_all,
    success,
    warn,
    white,
    worktree_mismatch_warning,
    yellow,
};

mod external;
mod lifecycle;
mod lock;
mod query;
mod status;

pub(crate) use lifecycle::{cmd_sync, cmd_uninit};
pub(crate) use lock::{cmd_resolve_bench, cmd_unlock};
pub(crate) use query::cmd_query;
pub(crate) use status::cmd_status;

pub(crate) async fn cmd_init(path_arg: Option<&str>, force: bool, verbose: bool) {
    #[cfg(unix)]
    let _supervision = codegraph::mcp::proxy::start_command_supervision("init");
    lifecycle::cmd_init(path_arg, force, verbose).await;
}

pub(crate) async fn cmd_index(path_arg: Option<&str>, force: bool, quiet: bool, verbose: bool) {
    #[cfg(unix)]
    let _supervision = codegraph::mcp::proxy::start_command_supervision("index");
    lifecycle::cmd_index(path_arg, force, quiet, verbose).await;
}
