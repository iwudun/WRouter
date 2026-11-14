//! 数字指纹特征提取(20261030_01 R2)。
//!
//! 逐函数镜像 ModelTrace `fingerprint.py` 的特征段(数值语义完全对齐,
//! 不是"近似实现"):
//! - [`parse_numbers`] —— 取**最长数字游程**;两个数字之间出现字母则断游;
//!   只保留 [1,355]。镜像 `re.finditer(r"\d+")` + `character.isalpha()` 语义
//!   —— Rust 侧用 `\p{L}`(Unicode Letter 类),与 Python `str.isalpha()`
//!   的判定集合一致(Lm/Lt/Lu/Ll/Lo),也与 ModelTrace JS 评分器的
//!   `/\p{L}/u` 一致。
//! - [`count_numbers`] —— 355 维计数向量。
//! - [`hellinger_feature`] —— `sqrt((counts + 0.5) / sum)`(镜像 numpy 一致
//!   的分母 `sum(counts) + 0.5 * 355`)。
//! - [`ordered_block_feature`] —— 序列切 4 段 × 16 桶直方图 + 末位数字分布,
//!   共 74 维。分桶规则镜像 `np.histogram(bins=16, range=(1, 356))`:
//!   桶宽 355/16,值 v 落在 `floor((v-1)/355*16)` 桶(与 ModelTrace JS
//!   评分器 `Math.floor(((value-1)/355)*16)` 同式)。
//! - [`standardize`]:**总体**标准差(除以 n,镜像 numpy `ddof=0`)。

use std::sync::OnceLock;

use regex::Regex;

/// 数字取值下界。
pub const VALUE_MIN: u32 = 1;
/// 数字取值上界。
pub const VALUE_MAX: u32 = 355;
/// 边缘分布特征维度(355 个取值 → 355 维)。
pub const DIMENSION: usize = (VALUE_MAX - VALUE_MIN + 1) as usize;
/// Hellinger 平滑常数(镜像 `fingerprint.py: ALPHA`)。
pub const ALPHA: f64 = 0.5;
/// 有序块特征在融合分中的权重(镜像 `ORDERED_BLOCK_WEIGHT`)。
pub const ORDERED_BLOCK_WEIGHT: f64 = 0.25;
/// 单条回答被接受所需的最少解析数字数(镜像 `minimum_valid_numbers`)。
pub const MINIMUM_VALID_NUMBERS: usize = 80;
/// 有序块特征维度(4 段 × 16 桶 + 10 个末位桶)。
pub const ORDERED_BLOCK_DIMENSION: usize = 4 * 16 + 10;

/// 字母判定(Unicode Letter 类)。
fn letter_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"\p{L}").expect("letter regex is valid"))
}

/// 取最长数字游程。
///
/// 镜像 `fingerprint.py:22-37`:按出现顺序扫描数字段,若当前游程非空且
/// **上一个数字到本数字之间出现字母**,则先结算当前游程;数字本身只在
/// [1,355] 区间内才入游程(区间外的数字被丢弃但**不会**断游 —— 与 Python
/// 一致:断游只看分隔符里的字母)。
pub fn parse_numbers(text: &str) -> Vec<u32> {
    let mut runs: Vec<Vec<u32>> = Vec::new();
    let mut current: Vec<u32> = Vec::new();
    let mut previous_end = 0usize;
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        let end = index;
        let separator = &text[previous_end..start];
        // Python:`if current and any(c.isalpha() for c in separator)`。
        if !current.is_empty() && letter_re().is_match(separator) {
            runs.push(std::mem::take(&mut current));
        }
        // 超长数字串(Python 大整数)一律落在区间外:用位数先判,避免溢出。
        let value: u64 = if end - start > 3 {
            u64::MAX
        } else {
            text[start..end].parse::<u64>().unwrap_or(u64::MAX)
        };
        if (VALUE_MIN as u64..=VALUE_MAX as u64).contains(&value) {
            current.push(value as u32);
        }
        previous_end = end;
    }
    if !current.is_empty() {
        runs.push(current);
    }
    // Python `max(runs, key=len)` 与 JS `reduce((best, run) => run.length > best.length ? ... )`
    // 在**等长并列**时都返回第一个;Rust `max_by_key` 并列时返回最后一个,
    // 会取错游程 —— 这里手写「严格大于才替换」以对齐取第一个的语义。
    let mut best: Vec<u32> = Vec::new();
    for run in runs {
        if run.len() > best.len() {
            best = run;
        }
    }
    best
}

/// 355 维计数向量。
pub fn count_numbers(numbers: &[u32]) -> Vec<u32> {
    let mut counts = vec![0u32; DIMENSION];
    for number in numbers {
        let slot = (*number - VALUE_MIN) as usize;
        if slot < DIMENSION {
            counts[slot] += 1;
        }
    }
    counts
}

/// Hellinger 边缘分布特征(355 维)。
pub fn hellinger_feature(counts: &[u32]) -> Vec<f64> {
    let total = counts.iter().map(|value| *value as f64).sum::<f64>() + ALPHA * DIMENSION as f64;
    counts
        .iter()
        .map(|value| ((*value as f64 + ALPHA) / total).sqrt())
        .collect()
}

/// 序列切 4 段(镜像 `np.array_split(values, 4)`:前 `n % 4` 段多一个元素)。
fn split_into_four(numbers: &[u32]) -> [Vec<u32>; 4] {
    let base = numbers.len() / 4;
    let remainder = numbers.len() % 4;
    let mut chunks: [Vec<u32>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    let mut start = 0usize;
    for (index, chunk) in chunks.iter_mut().enumerate() {
        let size = base + usize::from(index < remainder);
        chunk.extend_from_slice(&numbers[start..start + size]);
        start += size;
    }
    chunks
}

/// 有序块特征(4 段 × 16 桶 + 末位 10 桶 = 74 维)。
pub fn ordered_block_feature(numbers: &[u32]) -> Vec<f64> {
    let mut pieces: Vec<f64> = Vec::with_capacity(ORDERED_BLOCK_DIMENSION);
    for chunk in split_into_four(numbers) {
        let mut bins = [0.5f64; 16];
        for value in &chunk {
            let index = (((*value as f64 - 1.0) / 355.0) * 16.0).floor() as usize;
            bins[index.min(15)] += 1.0;
        }
        let total = bins.iter().sum::<f64>();
        pieces.extend(bins.iter().map(|value| (value / total).sqrt()));
    }
    let mut last_digits = [0.5f64; 10];
    for value in numbers {
        last_digits[(*value % 10) as usize] += 1.0;
    }
    let total = last_digits.iter().sum::<f64>();
    pieces.extend(last_digits.iter().map(|value| (value / total).sqrt()));
    pieces
}

/// 总体标准差标准化(镜像 `fingerprint.py:47-51`)。
pub fn standardize(values: &[f64]) -> Vec<f64> {
    if values.is_empty() {
        return Vec::new();
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values.iter().map(|value| (value - mean).powi(2)).sum::<f64>()
        / values.len() as f64;
    let scale = variance.sqrt().max(1e-12);
    values.iter().map(|value| (value - mean) / scale).collect()
}

/// 元素积。
pub fn dot(left: &[f64], right: &[f64]) -> f64 {
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

/// 欧氏范数。
pub fn norm(values: &[f64]) -> f64 {
    dot(values, values).sqrt()
}

/// 单位化(镜像 ModelTrace JS 的 `normalized`:范数下限 1e-12)。
pub fn normalize(values: &[f64]) -> Vec<f64> {
    let scale = norm(values).max(1e-12);
    values.iter().map(|value| value / scale).collect()
}

/// Jensen-Shannon 相似度(镜像 `fingerprint.py:141-152`)。
///
/// 返回 0..1;1 表示两侧分布一致。
pub fn js_similarity(left: &[u32], right: &[u32]) -> f64 {
    let left_total = left.iter().map(|value| *value as f64).sum::<f64>();
    let right_total = right.iter().map(|value| *value as f64).sum::<f64>() + ALPHA * DIMENSION as f64;
    let p: Vec<f64> = left.iter().map(|value| *value as f64 / left_total).collect();
    let q: Vec<f64> = right
        .iter()
        .map(|value| (*value as f64 + ALPHA) / right_total)
        .collect();
    let midpoint: Vec<f64> = p.iter().zip(&q).map(|(a, b)| (a + b) / 2.0).collect();
    let divergence = |values: &[f64], middle: &[f64]| -> f64 {
        values
            .iter()
            .zip(middle)
            .filter(|(value, _)| **value != 0.0)
            .map(|(value, target)| value * (value / target).ln())
            .sum()
    };
    let js = (divergence(&p, &midpoint) + divergence(&q, &midpoint)) / 2.0;
    1.0 - (js / std::f64::consts::LN_2).sqrt()
}

/// softmax(镜像 `fingerprint.py:134-138`)。
pub fn softmax(values: &[f64]) -> Vec<f64> {
    if values.is_empty() {
        return Vec::new();
    }
    let maximum = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let weights: Vec<f64> = values.iter().map(|value| (value - maximum).exp()).collect();
    let total = weights.iter().sum::<f64>();
    weights.iter().map(|weight| weight / total).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_longest_digit_run_and_breaks_on_letters() {
        // 无字母分隔 → 全部入同一游程。
        assert_eq!(parse_numbers("1, 2, 3"), vec![1, 2, 3]);
        // 字母断游:后半段成为独立游程,取更长者。
        let text = "12, 13, 14 and 7, 8";
        assert_eq!(parse_numbers(text), vec![12, 13, 14]);
        // 区间外数字被丢弃但不���游。
        assert_eq!(parse_numbers("1 2 999 3"), vec![1, 2, 3]);
        // 区间外数字独占长游程时,最长游程可能为空。
        assert!(parse_numbers("900 901").is_empty());
    }

    #[test]
    fn equal_length_runs_pick_the_first_like_python_and_js() {
        // 两个等长游程:Python `max(key=len)` 与 JS `reduce(>)` 都取**第一个**,
        // Rust `max_by_key` 会取最后一个 —— 必须对齐取第一个。
        assert_eq!(parse_numbers("1, 2, 3 and 7, 8, 9"), vec![1, 2, 3]);
        assert_eq!(parse_numbers("10 11 and 20 21"), vec![10, 11]);
        // 不等长时仍取最长。
        assert_eq!(parse_numbers("1 and 7, 8, 9"), vec![7, 8, 9]);
    }

    #[test]
    fn hellinger_feature_matches_python_shape() {
        let counts = count_numbers(&[1, 1, 2, 355]);
        let feature = hellinger_feature(&counts);
        assert_eq!(feature.len(), DIMENSION);
        let total: f64 = feature.iter().map(|value| value * value).sum();
        // 平方和 = sum(counts+alpha)/total = 1
        assert!((total - 1.0).abs() < 1e-12);
    }

    #[test]
    fn ordered_block_feature_is_74_dims_and_normalized() {
        let feature = ordered_block_feature(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(feature.len(), ORDERED_BLOCK_DIMENSION);
        // 前 4 段各自平方和为 1,末位段为 1
        for block in 0..4 {
            let sum: f64 = feature[block * 16..(block + 1) * 16]
                .iter()
                .map(|value| value * value)
                .sum();
            assert!((sum - 1.0).abs() < 1e-12);
        }
        let last: f64 = feature[64..74].iter().map(|value| value * value).sum();
        assert!((last - 1.0).abs() < 1e-12);
    }

    #[test]
    fn split_into_four_matches_numpy_array_split() {
        for len in [0usize, 1, 3, 4, 5, 7, 8, 9, 13, 100] {
            let numbers: Vec<u32> = (1..=len as u32).collect();
            let chunks = split_into_four(&numbers);
            assert_eq!(chunks.iter().map(Vec::len).sum::<usize>(), len);
            let sizes: Vec<usize> = chunks.iter().map(Vec::len).collect();
            let base = len / 4;
            let remainder = len % 4;
            for (index, size) in sizes.iter().enumerate() {
                assert_eq!(*size, base + usize::from(index < remainder));
            }
        }
    }

    #[test]
    fn standardize_uses_population_std() {
        let values = [1.0, 2.0, 3.0, 4.0];
        let standardized = standardize(&values);
        let mean = standardized.iter().sum::<f64>() / 4.0;
        assert!(mean.abs() < 1e-12);
        let variance = standardized.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / 4.0;
        assert!((variance - 1.0).abs() < 1e-12);
    }

    #[test]
    fn softmax_sums_to_one() {
        let probabilities = softmax(&[1.0, 2.0, 3.0]);
        assert!((probabilities.iter().sum::<f64>() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn js_similarity_is_high_for_matching_profiles() {
        // 用真实量级的汇总计数(1000 个数字铺满 355 桶)才有意义:
        // 少量数字时 p 极度集中而 q 被 ALPHA 平滑,相似度天然很低。
        let dense: Vec<u32> = (1..=355u32).cycle().take(1000).collect();
        let counts = count_numbers(&dense);
        let similarity = js_similarity(&counts, &counts);
        assert!(similarity > 0.99, "同分布相似度应接近 1,实际 {similarity}");
        // 明显不同的分布应更低。
        let skewed: Vec<u32> = (1..=10u32).cycle().take(1000).collect();
        let other = count_numbers(&skewed);
        assert!(js_similarity(&counts, &other) < similarity);
    }
}
