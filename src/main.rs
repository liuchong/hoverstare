//! HoverStare CLI entry point (logic lives in lib, see cli::run)

#[tokio::main]
async fn main() {
    hoverstare::cli::run().await;
}
