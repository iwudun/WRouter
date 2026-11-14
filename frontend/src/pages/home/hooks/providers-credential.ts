// 供应商凭据池的纯配置变更(自 useProvidersState.ts 搬移)。
//
// 这些函数只接收并返回 AppConfig;持久化/乐观更新由 hook 的
// buildConfigUpdate + persistConfig 负责。

import type { AppConfig } from "@/contracts/app";

import {
  applyEnabledIds,
  createProviderAccount,
  enabledAccountIds,
  enforceSingleEnabledKey,
  type ProviderCredentialConfig,
  type ProviderKeyMode,
} from "../shared/index";

/** 固定密钥 / 负载均衡两模式切换,各自记忆启用集合。 */
export function applyProviderCredentialMode(config: AppConfig, providerIndex: number, mode: ProviderKeyMode): AppConfig {
  const provider = config.Providers[providerIndex];
  if (!provider) {
    return config;
  }
  const accounts = provider.accounts ?? [];
  if (mode === "fixed") {
    // 离开负载均衡前快照启用集合;恢复固定侧记忆(记忆指向的密钥仍在
    // 就用它,否则落第一把启用/第一把密钥)。
    provider.balancedEnabledAccountIds = enabledAccountIds(accounts);
    const target = provider.pinnedAccountId
      && accounts.some((account) => account.id === provider.pinnedAccountId)
      ? provider.pinnedAccountId
      : enabledAccountIds(accounts)[0] ?? accounts[0]?.id;
    if (target) {
      provider.accounts = enforceSingleEnabledKey(accounts, target);
      provider.pinnedAccountId = target;
    }
    provider.credentialStrategy = "first";
  } else {
    // 固定侧记忆 = 当前启用的那把(全部停用则保留原记忆,切回时可恢复);
    // 负载均衡侧有记忆才恢复,首次切入保持现状开关不动。
    provider.pinnedAccountId = enabledAccountIds(accounts)[0] ?? provider.pinnedAccountId;
    const memory = provider.balancedEnabledAccountIds;
    if (memory && memory.length > 0) {
      provider.accounts = applyEnabledIds(accounts, memory);
    }
    provider.credentialStrategy = "balanced";
  }
  return config;
}

/** 固定密钥模式:单启用 + pinned 同步,单次配置写入。 */
export function applyProviderFixedKey(config: AppConfig, providerIndex: number, accountId: string): AppConfig {
  const provider = config.Providers[providerIndex];
  if (!provider) {
    return config;
  }
  const accounts = provider.accounts ?? [];
  if (!accounts.some((account) => account.id === accountId)) {
    return config;
  }
  provider.accounts = enforceSingleEnabledKey(accounts, accountId);
  provider.pinnedAccountId = accountId;
  return config;
}

/**
 * 把存量 legacy 单密钥迁移为列表第一行,并显式清空 `api_key`
 * (空串在三态里是「不动 secrets」,必须写 null 才能清掉影子键)。
 */
export function applyProviderLegacyKeyMigration(config: AppConfig, providerIndex: number): AppConfig {
  const provider = config.Providers[providerIndex];
  if (!provider) {
    return config;
  }
  const legacyKey = provider.api_key?.trim() ?? "";
  if (!legacyKey) {
    return config;
  }
  if (provider.accounts && provider.accounts.length > 0) {
    // 池已在但供应商级残留密钥:只清影子键,不动池。
    provider.api_key = null;
    return config;
  }
  provider.accounts = [
    {
      ...createProviderAccount([]),
      api_key: legacyKey,
      enabled: true
    }
  ];
  provider.api_key = null;
  return config;
}

/** 账号数组整体替换;空数组摘键并清理悬空 pinned 引用。 */
export function applyProviderAccounts(
  config: AppConfig,
  providerIndex: number,
  accounts: ProviderCredentialConfig[]
): AppConfig {
  const provider = config.Providers[providerIndex];
  if (!provider) {
    return config;
  }
  // 空数组摘键,回到 legacy api_key 路径(方案 §6:无 accounts 零变化)。
  provider.accounts = accounts.length > 0 ? accounts : undefined;
  // 清理指向已删除账号的 pinned 引用,避免 pinned 策略命中空选号。
  if (provider.pinnedAccountId && !accounts.some((account) => account.id === provider.pinnedAccountId)) {
    provider.pinnedAccountId = undefined;
  }
  return config;
}
