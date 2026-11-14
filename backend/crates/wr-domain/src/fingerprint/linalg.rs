//! 线性代数内核(20261030_01 R1):对称矩阵 Jacobi 特征分解 + 正交投影。
//!
//! **为什么必须自实现**:`backend/Cargo.toml` 的 workspace 依赖里没有任何
//! 数值/矩阵 crate(`Cargo.lock` 只有传递依赖 `rand`),本机 cargo registry
//! 缓存里也没有 nalgebra/ndarray;叠加环境无 crates.io 可达性,引入新依赖
//! 会直接构建失败。故按方案 §3.3.3 自实现纯 Rust 版。
//!
//! **为什么选 Jacobi**:循环 Jacobi 约 120 行,一次 n×n 分解 O(n³) 次旋转,
//! 且**完全确定性**(无随机初值、无迭代次数抖动)——「同一输入必须复现同一库」
//! 是硬要求(用户自建库每次重建都要给出同一结果)。
//!
//! **与 ModelTrace 的 SVD 不逐位相同**:ModelTrace 用 `numpy.linalg.svd`
//! 取右奇异向量,本模块用 Jacobi 特征分解。两者数学目标一致(取偏移子空间的
//! 主方向),但旋转顺序不同 → 特征向量符号/简并空间内的基不唯一。
//! 打分只依赖**投影后的子空间**,不依赖基的具体方向,故结论一致。

use crate::fingerprint::features::{dot, normalize};

/// 对称矩阵的特征分解结果。
#[derive(Debug, Clone)]
pub struct Eigen {
    /// 特征值(降序)。
    pub values: Vec<f64>,
    /// 特征向量;`vectors[i]` 是第 i 个特征向量(与 `values[i]` 对应)。
    pub vectors: Vec<Vec<f64>>,
}

/// 对称矩阵的 Jacobi 特征分解。
///
/// 输入按行给出(仅使用上三角;下三角视为镜像,调用方需保证对称)。
/// 经典循环 Jacobi:反复扫描所有非对角元并做旋转,直至非对角元范数收敛。
///
/// 确定性:旋转顺序固定为 `p < q` 的行序,收敛阈值固定 → 同一输入恒得同一
/// 结果(含符号)。
///
/// `needless_range_loop` 豁免:Jacobi 旋转要**就地**改写矩阵的 `a[p][q]` /
/// `a[k][p]` 等元素,必须按索引访问(迭代器借用会与就地写入冲突)。旋转顺序
/// 本身就是算法的一部分,不能换成别的遍历序。
#[allow(clippy::needless_range_loop)]
pub fn symmetric_eigen(matrix: &[Vec<f64>]) -> Eigen {
    let n = matrix.len();
    if n == 0 {
        return Eigen {
            values: Vec::new(),
            vectors: Vec::new(),
        };
    }
    let mut a: Vec<Vec<f64>> = matrix.to_vec();
    // 特征向量矩阵 V = I
    let mut v: Vec<Vec<f64>> = (0..n)
        .map(|row| (0..n).map(|col| if row == col { 1.0 } else { 0.0 }).collect())
        .collect();

    // Frobenius 范数作为收敛尺度的基准。
    let frobenius = a
        .iter()
        .enumerate()
        .flat_map(|(row, values)| {
            values
                .iter()
                .enumerate()
                .map(move |(col, value)| if row == col { value * value } else { 2.0 * value * value })
        })
        .sum::<f64>()
        .sqrt()
        .max(1e-300);

    for _sweep in 0..100 {
        let mut off_diagonal = 0.0f64;
        for (p, row) in a.iter().enumerate() {
            for value in &row[p + 1..] {
                off_diagonal += value * value;
            }
        }
        if off_diagonal.sqrt() <= 1e-13 * frobenius {
            break;
        }
        for p in 0..n {
            for q in (p + 1)..n {
                let apq = a[p][q];
                if apq.abs() <= 1e-300 {
                    continue;
                }
                // Numerical Recipes 的 Jacobi 旋转参数。
                let theta = (a[q][q] - a[p][p]) / (2.0 * apq);
                let t = if theta >= 0.0 {
                    1.0 / (theta + (1.0 + theta * theta).sqrt())
                } else {
                    -1.0 / (-theta + (1.0 + theta * theta).sqrt())
                };
                let c = 1.0 / (1.0 + t * t).sqrt();
                let s = t * c;
                let tau = s / (1.0 + c);
                let h = t * apq;
                a[p][p] -= h;
                a[q][q] += h;
                a[p][q] = 0.0;
                a[q][p] = 0.0;
                for k in 0..n {
                    if k == p || k == q {
                        continue;
                    }
                    let g = a[k][p];
                    let h = a[k][q];
                    a[k][p] = g - s * (h + g * tau);
                    a[p][k] = a[k][p];
                    a[k][q] = h + s * (g - h * tau);
                    a[q][k] = a[k][q];
                }
                for k in 0..n {
                    let g = v[k][p];
                    let h = v[k][q];
                    v[k][p] = c * g - s * h;
                    v[k][q] = s * g + c * h;
                }
            }
        }
    }

    // 特征值降序;特征向量随其重排。
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|left, right| {
        a[*right][*right]
            .partial_cmp(&a[*left][*left])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let values: Vec<f64> = order.iter().map(|index| a[*index][*index]).collect();
    let vectors: Vec<Vec<f64>> = order
        .iter()
        .map(|index| (0..n).map(|row| v[row][*index]).collect())
        .collect();
    Eigen { values, vectors }
}

/// 行向量的 Gram 矩阵 `rowsᵀ · rows`(D×D)。
pub fn gram_matrix(rows: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let dimension = rows.first().map(Vec::len).unwrap_or(0);
    let mut gram = vec![vec![0.0f64; dimension]; dimension];
    for row in rows {
        for i in 0..dimension {
            let left = row[i];
            if left == 0.0 {
                continue;
            }
            for j in 0..dimension {
                gram[i][j] += left * row[j];
            }
        }
    }
    gram
}

/// 行向量的 Gram 矩阵 `rows · rowsᵀ`(E×E,远小于 D×D)。
fn small_gram(rows: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let count = rows.len();
    let mut gram = vec![vec![0.0f64; count]; count];
    for i in 0..count {
        for j in 0..count {
            gram[i][j] = dot(&rows[i], &rows[j]);
        }
    }
    gram
}

/// 右奇异向量主方向(偏移/干扰基)及对应奇异值。
#[derive(Debug, Clone)]
pub struct SingularBasis {
    /// 奇异值(降序)。
    pub singular_values: Vec<f64>,
    /// 单位正交右奇异向量(与 `singular_values` 一一对应)。
    pub basis: Vec<Vec<f64>>,
}

/// 取行向量矩阵的全部**右奇异方向**(偏移/干扰基候选)与对应奇异值。
///
/// 输入 `rows` 是 E×D 的行向量(每行一个环境的偏移向量)。
///
/// **实现说明**:ModelTrace 用 `numpy.linalg.svd(offsets)` 取右奇异向量。
/// 本实现走等价路径 —— 先对 E×E 的 `offsets·offsetsᵀ` 做 Jacobi 分解拿到
/// 左奇异向量 `u` 与 σ²,再由 `v = offsetsᵀ·u / σ` 还原右奇异向量。
/// 两者给出的**子空间相同**(右奇异向量张成的空间一致),但基的具体方向
/// 可能不同(分解算法差异)——打分只依赖投影后的子空间,结论一致。
/// E×E 分解比 D×D 分解快两个数量级(12³ vs 355³),而建库的分组交叉验证
/// 要重复拟合几十次,这个差距是决定性的。
///
/// 退化判定沿用 ModelTrace 的相对阈值:`σ ≤ σ₀ · 1e-8` 视为数值零
/// (`bank_builder.py:87` 的 `singular > singular[0] * 1e-8`)。
/// 秩的**选取**在 `bank.rs`(方案 X1 自适应秩),本函数只给候选。
pub fn right_singular_directions(rows: &[Vec<f64>]) -> SingularBasis {
    if rows.len() < 2 {
        return SingularBasis {
            singular_values: Vec::new(),
            basis: Vec::new(),
        };
    }
    let gram = small_gram(rows);
    let eigen = symmetric_eigen(&gram);
    let total: f64 = eigen.values.iter().map(|value| value.max(0.0)).sum();
    if total <= 0.0 {
        return SingularBasis {
            singular_values: Vec::new(),
            basis: Vec::new(),
        };
    }
    let mut basis: Vec<Vec<f64>> = Vec::new();
    let mut singular_values: Vec<f64> = Vec::new();
    let first_sigma = eigen.values.first().copied().unwrap_or(0.0).max(0.0).sqrt();
    let degeneracy_threshold = first_sigma * 1e-8;
    for (index, value) in eigen.values.iter().enumerate() {
        let energy = value.max(0.0);
        let sigma = energy.sqrt();
        if sigma <= degeneracy_threshold {
            break;
        }
        // 左奇异向量 u(归一),右奇异向量 = offsetsᵀ·u / σ
        let u: Vec<f64> = (0..rows.len()).map(|row| eigen.vectors[row][index]).collect();
        let mut right = vec![0.0f64; rows.first().map(Vec::len).unwrap_or(0)];
        for (row_index, row) in rows.iter().enumerate() {
            let coefficient = u[row_index];
            for (slot, value) in right.iter_mut().zip(row) {
                *slot += coefficient * value;
            }
        }
        if sigma > 0.0 {
            for value in &mut right {
                *value /= sigma;
            }
        }
        let normalized = normalize(&right);
        if normalized.iter().all(|value| value.is_finite()) {
            basis.push(normalized);
            singular_values.push(sigma);
        }
    }
    SingularBasis {
        singular_values,
        basis,
    }
}

/// 按方案 X1 选秩:累计能量占比 ≥ `energy_threshold` 即停,上限 `max_rank`。
///
/// ModelTrace 硬编码 `rank ≤ 2`(`bank_builder.py:87`),环境数变化时会欠拟合
/// 或过拟合;本实现改为**能量自适应**(默认 95%),并保留硬上限。
pub fn adaptive_rank(singular_values: &[f64], max_rank: usize, energy_threshold: f64) -> usize {
    if singular_values.is_empty() || max_rank == 0 {
        return 0;
    }
    let total: f64 = singular_values.iter().map(|value| value * value).sum();
    if total <= 0.0 {
        return 0;
    }
    let mut accumulated = 0.0f64;
    for (index, sigma) in singular_values.iter().enumerate() {
        if index >= max_rank {
            break;
        }
        accumulated += sigma * sigma;
        if accumulated / total >= energy_threshold {
            return index + 1;
        }
    }
    singular_values.len().min(max_rank)
}

/// 取行向量矩阵的**右奇异向量**主方向(偏移/干扰基),按方案 X1 选秩。
pub fn right_singular_basis(
    rows: &[Vec<f64>],
    max_rank: usize,
    energy_threshold: f64,
) -> Vec<Vec<f64>> {
    let directions = right_singular_directions(rows);
    let rank = adaptive_rank(&directions.singular_values, max_rank, energy_threshold);
    directions.basis.into_iter().take(rank).collect()
}

/// 从向量中投影掉基张成的子空间(镜像 `projected -= (projected @ Bᵀ) @ B`)。
///
/// 基向量必须是单位正交的(本模块产出的基满足);顺序正交投影与一次性
/// 正交投影在基正交时等价。
pub fn project_out(vector: &[f64], basis: &[Vec<f64>]) -> Vec<f64> {
    let mut output = vector.to_vec();
    for direction in basis {
        let projection = dot(&output, direction);
        for (slot, component) in output.iter_mut().zip(direction) {
            *slot -= projection * component;
        }
    }
    output
}

/// 两组向量是否逐点相等(带容差)。
pub fn approx_equal(left: &[f64], right: &[f64], tolerance: f64) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| (a - b).abs() <= tolerance)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 对称 3×3 手算对拍:
    /// [[2,1,0],[1,2,1],[0,1,2]] 的特征值为 2+√2, 2, 2-√2。
    #[test]
    fn decomposes_symmetric_three_by_three_against_hand_computation() {
        let matrix = vec![
            vec![2.0, 1.0, 0.0],
            vec![1.0, 2.0, 1.0],
            vec![0.0, 1.0, 2.0],
        ];
        let eigen = symmetric_eigen(&matrix);
        let expected = [2.0 + 2f64.sqrt(), 2.0, 2.0 - 2f64.sqrt()];
        for (index, want) in expected.iter().enumerate() {
            assert!(
                (eigen.values[index] - want).abs() < 1e-10,
                "eigenvalue[{index}] = {} expected {want}",
                eigen.values[index]
            );
        }
    }

    #[test]
    fn eigenvectors_are_orthonormal() {
        let matrix = vec![
            vec![4.0, 1.0, 0.5],
            vec![1.0, 3.0, 0.25],
            vec![0.5, 0.25, 2.0],
        ];
        let eigen = symmetric_eigen(&matrix);
        for i in 0..eigen.vectors.len() {
            for j in 0..eigen.vectors.len() {
                let inner = dot(&eigen.vectors[i], &eigen.vectors[j]);
                let want = if i == j { 1.0 } else { 0.0 };
                assert!(
                    (inner - want).abs() < 1e-10,
                    "QᵀQ[{i},{j}] = {inner} expected {want}"
                );
            }
        }
    }

    #[test]
    fn projection_is_idempotent() {
        let matrix = vec![
            vec![3.0, 1.0, 0.0],
            vec![1.0, 3.0, 1.0],
            vec![0.0, 1.0, 3.0],
        ];
        let eigen = symmetric_eigen(&matrix);
        let basis: Vec<Vec<f64>> = eigen.vectors.iter().take(1).cloned().collect();
        let vector = vec![1.0, 2.0, 3.0];
        let once = project_out(&vector, &basis);
        let twice = project_out(&once, &basis);
        assert!(approx_equal(&once, &twice, 1e-12), "P(Px) != Px");
    }

    #[test]
    fn right_singular_basis_captures_known_subspace() {
        // 行向量张成的空间是 span{[1,0,0],[0,1,0]};主方向应还原该子空间。
        let rows = vec![
            vec![1.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0],
            vec![1.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0],
        ];
        let basis = right_singular_basis(&rows, 4, 0.95);
        assert_eq!(basis.len(), 2);
        for direction in &basis {
            assert!((direction[2]).abs() < 1e-10, "第三分量应接近 0");
            assert!((dot(direction, direction) - 1.0).abs() < 1e-10);
        }
        // 两个基方向应互相正交
        assert!((dot(&basis[0], &basis[1])).abs() < 1e-10);
    }

    #[test]
    fn right_singular_basis_respects_rank_cap() {
        let rows: Vec<Vec<f64>> = (0..6)
            .map(|index| vec![index as f64, (index * index) as f64, 1.0])
            .collect();
        let basis = right_singular_basis(&rows, 1, 0.95);
        assert_eq!(basis.len(), 1);
    }

    #[test]
    fn decomposition_is_deterministic() {
        let matrix = vec![
            vec![5.0, 2.0, 1.0, 0.5],
            vec![2.0, 4.0, 0.75, 0.25],
            vec![1.0, 0.75, 3.0, 0.1],
            vec![0.5, 0.25, 0.1, 2.0],
        ];
        let first = symmetric_eigen(&matrix);
        let second = symmetric_eigen(&matrix);
        assert!(approx_equal(&first.values, &second.values, 0.0));
        assert!(approx_equal(&first.vectors[0], &second.vectors[0], 0.0));
    }
}
