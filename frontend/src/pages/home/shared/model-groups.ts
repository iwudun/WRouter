// 聚合模型(方案 20260905_04)共享工具:归一化、命名冲突检查、目标快捷
// 构造。契约类型在 @/contracts/config(ModelGroupConfig),此处只做
// 前端草稿/校验层,保存走现有 saveConfig 配置链路(无新增 RPC)。
import type { GatewayProviderConfig } from "@/contracts/app";
import type { ModelGroupConfig, ModelGroupStrategy, ModelGroupTarget } from "@/contracts/config";

export type { ModelGroupConfig, ModelGroupStrategy, ModelGroupTarget };

/** 聚合模型命名空间保留名(2026-09-15):客户端可用 `wrouter/<聚合名>`
 * 显式调用聚合(后端 resolve 前缀分支),`/v1/models` 聚合条目的 owned_by
 * 也是它 —— 供应商名称/slug 不得占用(见 providers.isReservedProviderName)。 */
export const RESERVED_PROVIDER_SLUG = "wrouter";

export const MODEL_GROUP_STRATEGIES: ModelGroupStrategy[] = ["first", "weighted", "sticky"];

export function isModelGroupStrategy(value: unknown): value is ModelGroupStrategy {
  return value === "first" || value === "weighted" || value === "sticky";
}

function normalizeTarget(raw: unknown): ModelGroupTarget | undefined {
  if (!raw || typeof raw !== "object") {
    return undefined;
  }
  const value = raw as Record<string, unknown>;
  const providerId = typeof value.providerId === "string" ? value.providerId.trim() : "";
  const model = typeof value.model === "string" ? value.model.trim() : "";
  if (!providerId || !model) {
    return undefined;
  }
  const target: ModelGroupTarget = { model, providerId };
  const weight = Number(value.weight);
  if (value.weight !== undefined && Number.isFinite(weight) && weight >= 0) {
    target.weight = weight;
  }
  const priority = Number(value.priority);
  if (value.priority !== undefined && Number.isFinite(priority)) {
    target.priority = Math.trunc(priority);
  }
  if (value.enabled !== undefined) {
    target.enabled = Boolean(value.enabled);
  }
  return target;
}

/** 与后端 parse_model_groups 同口径:非法项跳过、名称小写去重首见胜出。 */
export function normalizeModelGroups(raw: unknown): ModelGroupConfig[] {
  if (!Array.isArray(raw)) {
    return [];
  }
  const seen = new Set<string>();
  const groups: ModelGroupConfig[] = [];
  for (const item of raw) {
    if (!item || typeof item !== "object") {
      continue;
    }
    const value = item as Record<string, unknown>;
    const name = typeof value.name === "string" ? value.name.trim() : "";
    if (!name) {
      continue;
    }
    const targets = Array.isArray(value.targets)
      ? value.targets.map(normalizeTarget).filter((target): target is ModelGroupTarget => Boolean(target))
      : [];
    if (targets.length === 0) {
      continue;
    }
    const key = name.toLowerCase();
    if (seen.has(key)) {
      continue;
    }
    seen.add(key);
    const id = typeof value.id === "string" && value.id.trim() ? value.id.trim() : createModelGroupId();
    groups.push({
      enabled: value.enabled === undefined ? undefined : Boolean(value.enabled),
      id,
      name,
      strategy: isModelGroupStrategy(value.strategy) ? value.strategy : undefined,
      targets
    });
  }
  return groups;
}

/** 稳定 id(与凭据账号 id 同一形态:时间戳 36 进制 + 随机段)。 */
export function createModelGroupId(): string {
  return `mg-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

export function createModelGroupDraft(): ModelGroupConfig {
  return {
    enabled: true,
    id: createModelGroupId(),
    name: "",
    strategy: "first",
    targets: []
  };
}

export function isModelGroupEnabled(group: ModelGroupConfig): boolean {
  return group.enabled !== false;
}

export function modelGroupTargetWeight(target: ModelGroupTarget): number {
  return Number.isFinite(target.weight) && (target.weight ?? 0) >= 0 ? (target.weight ?? 1) : 1;
}

/** 顺序即优先级(2026-09-26 目标行重设计):列表排列序就是尝试序,排在前
 *  先尝试 —— first/sticky 按序给 0..n-1(后端按 priority ASC 稳定分层,
 *  数值唯一 ⇒ 严格按列表序);weighted 全体同层(priority 0,后端层内加权
 *  随机才作用于全部目标)。启用开关已从 UI 移除,统一 enabled: true。
 *  未变化的 target 保留原对象引用,避免拖拽/编辑行无谓重挂载。 */
export function serializeTargetsByOrder(
  targets: ModelGroupTarget[],
  strategy: ModelGroupStrategy | undefined
): ModelGroupTarget[] {
  return targets.map((target, index) => {
    const priority = strategy === "weighted" ? 0 : index;
    if (target.priority === priority && target.enabled === true) {
      return target;
    }
    return { ...target, enabled: true, priority };
  });
}

/** 旧配置打开即迁移:按旧 priority(升序稳定,同值保持配置序)排出新展示
 *  序,再按新语义序列化 —— 迁移后「配置顺序 = 实际优先序」,面板展示不撒谎。 */
export function migrateTargetsByOrder(group: ModelGroupConfig): ModelGroupTarget[] {
  const sorted = [...group.targets].sort((a, b) => (a.priority ?? 0) - (b.priority ?? 0));
  return serializeTargetsByOrder(sorted, group.strategy);
}

/** 聚合名冲突检查:与其他聚合重名 → 保存被后端去重规则拒绝。2026-09-26
 * 调用契约下聚合只经 `WRouter/<聚合名>` 调用,与供应商模型名互不干扰,
 * 原「遮蔽裸模型名」检查随裸名通道一并移除。返回重名的聚合名。 */
export function modelGroupNameConflict(
  name: string,
  groups: ModelGroupConfig[],
  selfId?: string
): string | undefined {
  const trimmed = name.trim();
  if (!trimmed) {
    return undefined;
  }
  const key = trimmed.toLowerCase();
  const group = groups.find(
    (item) => item.id !== selfId && item.name.trim().toLowerCase() === key
  );
  return group?.name;
}

/** 「按模型名跨供应商」快捷视图:同名模型的全部提供方可一次勾选带入
 * (同名聚合是最常见场景但不做限定,方案决策 #10)。 */
export function providersServingModel(providers: GatewayProviderConfig[], model: string): GatewayProviderConfig[] {
  const key = model.trim().toLowerCase();
  if (!key) {
    return [];
  }
  return providers.filter((provider) =>
    (provider.models ?? []).some((candidate) => candidate.trim().toLowerCase() === key)
  );
}

/** 已配置供应商的全部模型名(小写去重,保留首见大小写)。 */
export function distinctProviderModelNames(providers: GatewayProviderConfig[]): string[] {
  const seen = new Set<string>();
  const names: string[] = [];
  for (const provider of providers) {
    for (const model of provider.models ?? []) {
      const trimmed = model.trim();
      const key = trimmed.toLowerCase();
      if (!trimmed || seen.has(key)) {
        continue;
      }
      seen.add(key);
      names.push(trimmed);
    }
  }
  return names.sort((a, b) => a.localeCompare(b));
}

/** 目标行重复检查(同 providerId + 同模型名的目标只允许一条)。 */
export function isDuplicateModelGroupTarget(group: ModelGroupConfig, target: ModelGroupTarget): boolean {
  return group.targets.some(
    (item) =>
      item.providerId === target.providerId &&
      item.model.trim().toLowerCase() === target.model.trim().toLowerCase()
  );
}
