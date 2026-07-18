/// 生成稳定的字符串 ID（调用方依赖其确定性）。
pub fn make_id(name: &str) -> String {
    name.trim().to_lowercase().replace(' ', "-")
}
