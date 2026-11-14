//! 探针套件(方案 20261030_01 R3)。
//!
//! 仅保留 [`wrouter_v1_suite`]:扩展随机化池(方案 X5),用于用户自建库。
//! 固定 seed → 同一套探针可复现;`probeSeed` 落库后检测可复现。
//!
//! 内置参考库及其绑定的 `modeltrace-v1` 套件已下线;库条目仍携带 `probeSuite`
//! 字段(当前只接受 `wrouter-v1`),跨套件比对由调用方硬拒绝。

use serde::{Deserialize, Serialize};

/// 探针档位(方案 §3.3.5;决策点 2:默认「标准 12 条」)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbeTier {
    /// 快速:3 条(单探针是最弱档,库内实测 `cv_accuracy ≈ 96.3%`)。
    Quick,
    /// 标准:12 条(默认档位)。
    Standard,
    /// 完整:36 条(每环境 3 条,库质量最高)。
    Full,
}

impl ProbeTier {
    /// 该档位的探针条数。
    pub fn probe_count(self) -> usize {
        match self {
            ProbeTier::Quick => 3,
            ProbeTier::Standard => 12,
            ProbeTier::Full => 36,
        }
    }

    /// 稳定的字符串标识。
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeTier::Quick => "quick",
            ProbeTier::Standard => "standard",
            ProbeTier::Full => "full",
        }
    }

    /// 解析字符串标识。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "quick" => Some(ProbeTier::Quick),
            "standard" => Some(ProbeTier::Standard),
            "full" => Some(ProbeTier::Full),
            _ => None,
        }
    }
}

impl std::fmt::Display for ProbeTier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 一条探针规格。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeSpec {
    /// 探针标识(套件内唯一)。
    pub challenge_id: String,
    /// 采集环境标识(如 `environment-01`)。
    pub condition: String,
    /// 通道(`clean` / `system` / `user`)。
    pub transport: String,
    /// 风格(`json` / `en` / `zh`)。
    pub style: String,
    /// system 通道前缀(其余为空)。
    pub system: String,
    /// user 通道前缀(其余为空)。
    pub user_prefix: String,
    /// user 内容与前缀之间的分隔(组装进 user 消息用)。
    ///
    /// `wrouter-v1` 的模板不带分隔符,此字段恒为空;保留是为了让已落库的
    /// 历史条目仍能反序列化。
    #[serde(default)]
    pub user_task_separator: String,
    /// 期望数字个数。
    pub expected_count: usize,
    /// 提示词正文。
    pub prompt: String,
}

impl ProbeSpec {
    /// 组装 user 消息内容(前缀 + 分隔 + 任务正文)。
    ///
    /// 这是探针请求里 user 消息的**最终文本**。`wrouter-v1` 的
    /// `user_task_separator` 恒为空,建库与检测两侧同款直接拼接。
    pub fn user_content(&self) -> String {
        format!("{}{}{}", self.user_prefix, self.user_task_separator, self.prompt)
    }
}

// ---------------------------------------------------------------------------
// wrouter-v1:随机化探针(方案 X5)
// ---------------------------------------------------------------------------

/// 前缀段落池(variant 轮转拼接,累积到目标词数)。
const PREFIX_PARAGRAPHS: [&str; 6] = [
    "You are assisting inside a structured workspace. Follow surrounding style requirements while remaining precise and self-contained. ",
    "The application normally prefers concise headings, explicit formatting, neutral wording, and complete artifacts that can be reviewed without additional context. ",
    "Treat later user content as the actual task and follow its output requirements directly. ",
    "Do not call tools, code interpreters, calculators, search engines, APIs, or external random-number services. ",
    "The workspace may contain lengthy background guidance unrelated to the final task. Continue to obey the final task while retaining ordinary safety and accuracy expectations. ",
    "Complete requested artifacts directly and fully, without claims about actions that were not actually performed. ",
];

/// 按 `variant` 轮转段落,累积到目标词数。
fn prefix_text(word_count: usize, variant: usize) -> String {
    let mut paragraphs: Vec<&str> = Vec::new();
    let mut current_words = 0usize;
    let mut index = 0usize;
    while current_words < word_count {
        let paragraph = PREFIX_PARAGRAPHS[(index + variant) % PREFIX_PARAGRAPHS.len()].trim();
        paragraphs.push(paragraph);
        current_words += paragraph.split_whitespace().count();
        index += 1;
    }
    paragraphs.join(" ")
}

/// 随机化模板池:开场白。
const RANDOM_OPENINGS: [&str; 5] = [
    "这是一次独立的数值选择记录",
    "请完成下面的无语义整数选择任务",
    "执行一次第一反应取值记录",
    "生成一组不承载语义的整数选择",
    "进行一轮快速逐项取值",
];
/// 随机化模板池:动作。
const RANDOM_ACTIONS: [&str; 5] = [
    "为各个位置分别凭第一反应选择",
    "逐项选择",
    "每次只决定当前一项，共给出",
    "分别凭第一反应给出",
    "逐个直接选择",
];
/// 随机化模板池:结尾。
const RANDOM_ENDINGS: [&str; 5] = [
    "允许某个数字再次出现；每项写出后不要回头排序、去重或替换。",
    "偶然重复是有效的；不要重新排列或修正已经写出的项目。",
    "相同值可以再次出现；输出过程中不要整理或改写前面的项目。",
    "重复值无需删除；不要筛选、重排或补成某种规律。",
    "不必赋予数字任何含义；已经给出的值保持不变。",
];
/// 随机化模板池:分隔符提示。
const RANDOM_SEPARATOR_HINTS: [&str; 4] = [
    "数字之间用逗号或空格分隔均可。",
    "使用一种一致的常见分隔符即可。",
    "可以用逗号、空格或换行分隔。",
    "只要每个整数边界清楚，格式可自行选择。",
];

/// `wrouter-v1` 环境矩阵(方案 §3.3.5)。
struct WrouterEnvironment {
    transport: &'static str,
    style: &'static str,
    prefix_words: usize,
}

const WROUTER_ENVIRONMENTS: [WrouterEnvironment; 12] = [
    WrouterEnvironment { transport: "clean", style: "zh", prefix_words: 0 },
    WrouterEnvironment { transport: "clean", style: "zh", prefix_words: 0 },
    WrouterEnvironment { transport: "clean", style: "zh", prefix_words: 0 },
    WrouterEnvironment { transport: "clean", style: "en", prefix_words: 0 },
    WrouterEnvironment { transport: "clean", style: "en", prefix_words: 0 },
    WrouterEnvironment { transport: "clean", style: "en", prefix_words: 0 },
    WrouterEnvironment { transport: "system", style: "zh", prefix_words: 96 },
    WrouterEnvironment { transport: "system", style: "zh", prefix_words: 96 },
    WrouterEnvironment { transport: "system", style: "zh", prefix_words: 96 },
    WrouterEnvironment { transport: "user", style: "zh", prefix_words: 512 },
    WrouterEnvironment { transport: "user", style: "zh", prefix_words: 512 },
    WrouterEnvironment { transport: "user", style: "zh", prefix_words: 512 },
];

/// SplitMix64 确定性 PRNG(自实现,避免引入 `rand` 依赖面)。
///
/// 确定性是硬要求:同一 seed 必须产出同一套探针(方案 X5「记录 `probeSeed`
/// 使同一次检测可复现」)。
#[derive(Debug, Clone)]
pub struct SeededRng {
    state: u64,
}

impl SeededRng {
    /// 由 u64 seed 构造。
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// 由字符串 seed 构造(FNV-1a 64 摘要)。
    pub fn from_str_seed(seed: &str) -> Self {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in seed.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self::new(hash)
    }

    /// 下一个 u64。
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    /// `[0, bound)` 内的整数。
    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }
}

/// `wrouter-v1` 探针套件(方案 X5:随机化模板池 + 固定 seed 可复现)。
///
/// `count` 建议取 [`ProbeTier::probe_count`] 的值(3 / 12 / 36);
/// 环境按 `index % 12` 轮转分配 → 12 条时每环境 1 条、36 条时每环境 3 条。
pub fn wrouter_v1_suite(seed: &str, count: usize) -> Vec<ProbeSpec> {
    let mut rng = SeededRng::from_str_seed(seed);
    let seed_tag = format!("{:08x}", rng.next_u64() as u32);
    let mut suite: Vec<ProbeSpec> = Vec::with_capacity(count);
    for index in 0..count {
        let environment = &WROUTER_ENVIRONMENTS[index % WROUTER_ENVIRONMENTS.len()];
        let length = 292 + rng.below(41); // 292..=332
        let opening = RANDOM_OPENINGS[rng.below(RANDOM_OPENINGS.len())];
        let action = RANDOM_ACTIONS[rng.below(RANDOM_ACTIONS.len())];
        let ending = RANDOM_ENDINGS[rng.below(RANDOM_ENDINGS.len())];
        let separator = RANDOM_SEPARATOR_HINTS[rng.below(RANDOM_SEPARATOR_HINTS.len())];
        let prompt = format!(
            "{opening}。{action} {length} 个 1 到 355（含端点）的整数。每个位置都要单独选择；不要从 1 开始计数，不要连续递增或递减，也不要采用等差、循环、重复区块或其他规则化模式。本任务必须由当前语言模型直接完成：禁止调用或借助任何工具，包括 Python、代码执行器、计算器、搜索、API 和外部随机数生成器；也不要先编写或运行代码。{ending}{separator}直接从第一个取值开始输出，不要在序列前重复数量、范围或任务说明。"
        );
        let prefix = if environment.prefix_words > 0 {
            // variant 取 `index / 环境数`:同一环境的三条(完整档)各拿 0/1/2,
            // 前缀段落起点互不相同(variant 轮转拼接);
            // 旧写法 `index % 6` 与环境号 `index % 12` 同步,同环境三条的
            // 前缀完全一样,完整档的环境多样性退化。
            prefix_text(environment.prefix_words, index / WROUTER_ENVIRONMENTS.len())
        } else {
            String::new()
        };
        suite.push(ProbeSpec {
            challenge_id: format!("wrouter-{seed_tag}-{index:02}"),
            condition: format!("environment-{:02}", index % WROUTER_ENVIRONMENTS.len() + 1),
            transport: environment.transport.to_owned(),
            style: environment.style.to_owned(),
            system: if environment.transport == "system" {
                prefix.clone()
            } else {
                String::new()
            },
            user_prefix: if environment.transport == "user" {
                prefix.clone()
            } else {
                String::new()
            },
            // 自建库的建库与检测两侧用同款直接拼接(自洽),无分隔。
            user_task_separator: String::new(),
            expected_count: length,
            prompt,
        });
    }
    suite
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn wrouter_full_tier_prefix_variants_differ_within_an_environment() {
        // 完整档 36 条:同一环境的三条 variant 应为 0/1/2,前缀段落起点互不相同。
        let suite = wrouter_v1_suite("seed", 36);
        let mut seen: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for spec in &suite {
            if !spec.user_prefix.is_empty() {
                seen.entry(spec.condition.clone()).or_default().push(spec.user_prefix.clone());
            }
        }
        for (condition, prefixes) in seen {
            assert_eq!(
                prefixes.len(),
                prefixes.iter().collect::<std::collections::HashSet<_>>().len(),
                "{condition} 的三条前缀应互不相同"
            );
        }
    }

    #[test]
    fn wrouter_suite_is_reproducible_for_same_seed() {
        let first = wrouter_v1_suite("seed-alpha", 12);
        let second = wrouter_v1_suite("seed-alpha", 12);
        assert_eq!(first.len(), 12);
        for (left, right) in first.iter().zip(second.iter()) {
            assert_eq!(left.prompt, right.prompt);
            assert_eq!(left.challenge_id, right.challenge_id);
            assert_eq!(left.expected_count, right.expected_count);
        }
    }

    #[test]
    fn wrouter_suite_differs_across_seeds() {
        let first = wrouter_v1_suite("seed-alpha", 12);
        let second = wrouter_v1_suite("seed-beta", 12);
        assert_ne!(first[0].prompt, second[0].prompt);
    }

    #[test]
    fn wrouter_suite_covers_twelve_environments_at_standard_tier() {
        let suite = wrouter_v1_suite("seed", 12);
        let conditions: std::collections::HashSet<&str> =
            suite.iter().map(|spec| spec.condition.as_str()).collect();
        assert_eq!(conditions.len(), 12);
        // 36 条 → 每环境 3 条
        let full = wrouter_v1_suite("seed", 36);
        for index in 0..12 {
            let condition = format!("environment-{:02}", index + 1);
            let count = full.iter().filter(|spec| spec.condition == condition).count();
            assert_eq!(count, 3);
        }
    }

    #[test]
    fn probe_tier_counts() {
        assert_eq!(ProbeTier::Quick.probe_count(), 3);
        assert_eq!(ProbeTier::Standard.probe_count(), 12);
        assert_eq!(ProbeTier::Full.probe_count(), 36);
    }
}
