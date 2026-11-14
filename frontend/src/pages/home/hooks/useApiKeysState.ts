// R7 split: API keys dialog domain — drafts, edit targets, validation + persistence.
import { useMemo, useState } from "react";
import {
  AddApiKeyDraft, ApiKeyConfig, AppConfig, createApiKeyDraft, createApiKeyEditDraft, createApiKeyList,
  createGeneratedApiKey, normalizeApiKeys, positiveInteger, updateApiKeyEditableConfig
} from "../shared/index";

export type ApiKeysStateOptions = {
  draftConfig: AppConfig;
  buildConfigUpdate: (mutator: (config: AppConfig) => AppConfig) => AppConfig;
  setConfigDraft: (config: AppConfig) => AppConfig;
  persistApiKeys: (apiKeys: ApiKeyConfig[], setError: (message: string) => void) => Promise<boolean>;
  t: (value: string) => string;
};

export function useApiKeysState({ draftConfig, buildConfigUpdate, setConfigDraft, persistApiKeys, t }: ApiKeysStateOptions) {
  const [apiKeyAddOpen, setApiKeyAddOpen] = useState(false);
  const [apiKeyDraft, setApiKeyDraft] = useState<AddApiKeyDraft>(() => createApiKeyDraft());
  const [apiKeyEditDraft, setApiKeyEditDraft] = useState<AddApiKeyDraft>(() => createApiKeyDraft());
  const [apiKeyEditIndex, setApiKeyEditIndex] = useState<number>();
  const [apiKeyError, setApiKeyError] = useState("");
  const [createdApiKey, setCreatedApiKey] = useState<ApiKeyConfig>();

  const apiKeys = useMemo(() => createApiKeyList(draftConfig), [draftConfig.APIKEY, draftConfig.APIKEYS]);
  const apiKeyEditItem = apiKeyEditIndex === undefined ? undefined : apiKeys.find((apiKey) => apiKey.index === apiKeyEditIndex);
  const quotaTokensValid = (draft: AddApiKeyDraft) => draft.quotaUnlimited || Boolean(positiveInteger(draft.quotaTokens));
  const canSubmitApiKey = Boolean(apiKeyDraft.name.trim()) && quotaTokensValid(apiKeyDraft) && (apiKeyDraft.expirationPreset !== "custom" || Boolean(apiKeyDraft.expiresAt.trim()));
  const canSubmitApiKeyEdit = quotaTokensValid(apiKeyEditDraft) && (apiKeyEditDraft.expirationPreset !== "custom" || Boolean(apiKeyEditDraft.expiresAt.trim()));

  function openAddApiKeyDialog() {
    setCreatedApiKey(undefined);
    setApiKeyDraft(createApiKeyDraft());
    setApiKeyError("");
    setApiKeyAddOpen(true);
  }

  function updateApiKeyDraft(patch: Partial<AddApiKeyDraft>) {
    setApiKeyDraft((current) => ({ ...current, ...patch }));
    setApiKeyError("");
  }

  function openEditApiKeyDialog(index: number) {
    const apiKey = apiKeys.find((item) => item.index === index);
    if (!apiKey) {
      return;
    }
    setApiKeyEditIndex(index);
    setApiKeyEditDraft(createApiKeyEditDraft(apiKey.key));
    setApiKeyError("");
  }

  function updateApiKeyEditDraft(patch: Partial<AddApiKeyDraft>) {
    setApiKeyEditDraft((current) => ({ ...current, ...patch }));
    setApiKeyError("");
  }

  async function submitApiKeyDraft() {
    if (!apiKeyDraft.name.trim()) {
      setApiKeyError(t("Name is required."));
      return;
    }
    if (!canSubmitApiKey) {
      setApiKeyError(t("Expiration is required."));
      return;
    }
    const apiKey = createGeneratedApiKey(apiKeyDraft);

    const next = buildConfigUpdate((config) => {
      const keys = normalizeApiKeys([...config.APIKEYS, apiKey], config.APIKEY);
      config.APIKEYS = keys;
      config.APIKEY = keys[0]?.key ?? "";
      return config;
    });
    setConfigDraft(next);
    if (await persistApiKeys(next.APIKEYS, setApiKeyError)) {
      setApiKeyAddOpen(false);
      setCreatedApiKey(apiKey);
    }
  }

  async function submitApiKeyEditDraft() {
    if (apiKeyEditIndex === undefined) {
      return;
    }
    if (!canSubmitApiKeyEdit) {
      setApiKeyError(t("Expiration is required."));
      return;
    }

    const next = buildConfigUpdate((config) => {
      const keys = normalizeApiKeys(config.APIKEYS, config.APIKEY).map((apiKey, index) =>
        index === apiKeyEditIndex ? updateApiKeyEditableConfig(apiKey, apiKeyEditDraft) : apiKey
      );
      config.APIKEYS = keys;
      config.APIKEY = keys[0]?.key ?? "";
      return config;
    });
    setConfigDraft(next);
    if (await persistApiKeys(next.APIKEYS, setApiKeyError)) {
      setApiKeyEditIndex(undefined);
    }
  }

  async function removeApiKey(index: number) {
    const next = buildConfigUpdate((config) => {
      const keys = normalizeApiKeys(config.APIKEYS, config.APIKEY).filter((_, itemIndex) => itemIndex !== index);
      config.APIKEYS = keys;
      config.APIKEY = keys[0]?.key ?? "";
      return config;
    });
    setConfigDraft(next);
    await persistApiKeys(next.APIKEYS, setApiKeyError);
  }

  async function toggleApiKey(index: number) {
    const next = buildConfigUpdate((config) => {
      const keys = normalizeApiKeys(config.APIKEYS, config.APIKEY).map((apiKey, itemIndex) =>
        itemIndex === index ? { ...apiKey, enabled: apiKey.enabled === false } : apiKey
      );
      config.APIKEYS = keys;
      config.APIKEY = keys[0]?.key ?? "";
      return config;
    });
    setConfigDraft(next);
    await persistApiKeys(next.APIKEYS, setApiKeyError);
  }

  return {
    apiKeys,
    apiKeyAddOpen,
    setApiKeyAddOpen,
    apiKeyDraft,
    apiKeyEditDraft,
    apiKeyEditIndex,
    setApiKeyEditIndex,
    apiKeyError,
    createdApiKey,
    setCreatedApiKey,
    apiKeyEditItem,
    canSubmitApiKey,
    canSubmitApiKeyEdit,
    openAddApiKeyDialog,
    updateApiKeyDraft,
    openEditApiKeyDialog,
    updateApiKeyEditDraft,
    submitApiKeyDraft,
    submitApiKeyEditDraft,
    removeApiKey,
    toggleApiKey
  };
}
