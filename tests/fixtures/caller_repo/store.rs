use crate::ids::make_id;

/// 数据库主键生成。
///
/// 重要：同一实体名必须永远映射到同一个 key。
/// dedup / upsert 逻辑依赖这个确定性，历史数据也是按旧 key 存储的。
pub fn key_for(entity_name: &str) -> String {
    make_id(entity_name)
}

/// upsert：重复调用必须覆盖同一行。
pub fn upsert(entity_name: &str, value: u64) {
    let key = key_for(entity_name);
    // db[key] = value
    let _ = (key, value);
}
