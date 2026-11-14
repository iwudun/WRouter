import type { AppConfig } from "./config";
import type { GatewayProviderConfig } from "./providers";

export const CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY_ENV = "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY";
export const CLAUDE_CODE_DEFAULT_ENV: Record<string, string> = {
  [CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY_ENV]: "1"
};
export const NO_AVAILABLE_GATEWAY_MODELS_MESSAGE =
  "No available models. Configure at least one provider with a model before starting the WRouter gateway or opening an agent through WRouter.";

export function assertAvailableGatewayModels(config: Pick<AppConfig, "Providers">): void {
  if (!hasAvailableGatewayModels(config)) {
    throw new Error(NO_AVAILABLE_GATEWAY_MODELS_MESSAGE);
  }
}

export function hasAvailableGatewayModels(config: Pick<AppConfig, "Providers">): boolean {
  return availableGatewayModelIds(config).length > 0;
}

export function availableGatewayModelIds(
  config: Pick<AppConfig, "Providers"> & Partial<Pick<AppConfig, "modelGroups">>
): string[] {
  const baseEntries = availableGatewayBaseModelEntries(config.Providers);
  const baseIds = baseEntries.map((entry) => `${entry.providerName}/${entry.modelName}`);
  // 20260905_04 §3.6:聚合名视为一个模型名进白名单选项(命中聚合即放行,
  // 目标级不再二次鉴权)。
  // 2026-09-26 调用契约:聚合与供应商模型一样以「命名空间/名称」进白名单
  // 选项(`wrouter/<聚合名>`,与后端 /v1/models 及请求选择器同口径)。
  const groupIds = (config.modelGroups ?? [])
    .filter((group) => group.enabled !== false && group.name.trim() && group.targets.length > 0)
    .map((group) => `wrouter/${group.name.trim()}`);
  return uniqueGatewayModelIds([...baseIds, ...groupIds]);
}

function availableGatewayBaseModelEntries(providers: GatewayProviderConfig[]): Array<{ modelName: string; providerName: string }> {
  return providers.flatMap((provider) => {
    const providerName = provider.name?.trim();
    if (!providerName || !Array.isArray(provider.models)) {
      return [];
    }
    return provider.models.flatMap((rawModel) => {
      const modelName = rawModel.trim();
      return modelName ? [{ modelName, providerName }] : [];
    });
  });
}

function uniqueGatewayModelIds(values: string[]): string[] {
  const seen = new Set<string>();
  const result: string[] = [];
  for (const value of values) {
    const normalized = value.trim();
    const key = normalized.toLowerCase();
    if (!normalized || seen.has(key)) {
      continue;
    }
    seen.add(key);
    result.push(normalized);
  }
  return result;
}
