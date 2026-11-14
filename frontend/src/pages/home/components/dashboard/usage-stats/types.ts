export type TrendChartKind = "bar" | "area";

export type TrendMetric = "requests" | "tokens";
export type TrendModel = { color: string; key: string; name: string };
export type TrendRow = { bucket: string; label: string; [key: string]: string | number };
export type TrendData = { models: TrendModel[]; rows: TrendRow[] };

/**
 * 20261106:排行维度(模型 / 客户端)。
 *
 * 原先是「用量趋势」卡右侧只挂模型排行,客户端用量另开一张卡 → 现把
 * 两套排行合并进同一列,用卡头的 tab 切维度。
 */
export type RankingDimension = "models" | "clients";

/**
 * 20261104_01 阶段三 · B-1a:按调用方聚合的用量行。
 *
 * 后端 `clientModels` 是 **(client, model, provider) 三元组**维度行
 * (`usage_store.rs` 的 client_models 分组),label 形如
 * `"{client} / {供应商/模型名}"` → 直接拿 label 分组会把同一客户端按模型
 * 拆成多行。此处按 `row.client` 归并成「谁在用我的网关」。
 *
 * 20261106(B-1b):`row.client` 已是**归一 id**(产品名,如 `ZCode`)——
 * 后端把同一客户端的多个版本号合并成一条 → 前端拿到的就是一条。
 */
export type ClientUsageRow = {
  cacheRatio: number;
  cacheTokens: number;
  client: string;
  costUsd: number;
  errorCount: number;
  inputTokens: number;
  models: string[];
  requestCount: number;
  totalTokens: number;
};

/**
 * 把后端 `clientModels` 行按纯 client 归并(缓存命中率口径与后端
 * `Accumulator::cache_ratio` 一致:cache_tokens / input_tokens)。
 */
