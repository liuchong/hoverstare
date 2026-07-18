//! bugbot 二进制（向后兼容别名）：行为与 hoverstare 完全一致。
//! 请迁移到 hoverstare。

#[tokio::main]
async fn main() {
    hoverstare::cli::run().await;
}
