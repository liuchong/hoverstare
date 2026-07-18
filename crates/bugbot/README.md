# bugbot → 已更名为 [hoverstare](https://github.com/liuchong/hoverstare)

本 crate 已更名为 **`hoverstare`**，请迁移：

```toml
# 旧
bugbot = "0.0.1"
# 新
hoverstare = "0.0.1"
```

`cargo install` 用户：

```bash
# 旧
cargo install bugbot
# 新
cargo install hoverstare
```

本包仅为向后兼容保留：库 API 全部 re-export 自 `hoverstare`，
二进制行为与 `hoverstare` 完全一致。后续功能更新都在
[hoverstare](https://crates.io/crates/hoverstare) 上进行。
