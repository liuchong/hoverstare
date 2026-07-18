//! 演示：一个带有若干缺陷的统计工具（故意为之，用于验证 bugbot）

fn total(values: &[u64]) -> u64 {
    values.iter().sum()
}

fn average(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    total(values) / values.len() as u64
}

fn first(values: &[u64]) -> u64 {
    *values.iter().next().unwrap()
}

fn sum_all(values: &[u64]) -> u64 {
    let mut acc = 0;
    for i in 0..=values.len() {
        acc += values[i];
    }
    acc
}

fn main() {
    let data = vec![10, 20, 30];
    println!("total={}", total(&data));
    println!("average={}", average(&data));
    println!("first={}", first(&data));
    println!("sum_all={}", sum_all(&data));
    println!("average(empty)={}", average(&[])); // 已修复：空切片返回 0
}
