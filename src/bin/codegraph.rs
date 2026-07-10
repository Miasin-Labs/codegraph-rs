#[path = "codegraph/main.rs"]
mod codegraph_cli;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    codegraph_cli::main().await;
}
