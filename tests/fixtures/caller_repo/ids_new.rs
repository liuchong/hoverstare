use std::time::{SystemTime, UNIX_EPOCH};

/// 生成字符串 ID。
pub fn make_id(name: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("{}-{}", name.trim().to_lowercase().replace(' ', "-"), millis)
}
