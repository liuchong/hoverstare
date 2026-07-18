//! HoverStare CLI 入口（逻辑在 lib，见 cli::run）

#[tokio::main]
async fn main() {
    hoverstare::cli::run().await;
}
