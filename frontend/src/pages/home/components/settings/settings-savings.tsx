// 20261101_02 T3-B:成本节省分区 —— 规则编辑器下线后,全局 savings
// (压缩 / 响应缓存 / 会话粘性 / 前缀钉扎)与全局失败回退的唯一配置入口。
// 与 settings-proxy.tsx 同范式:纯受控组件,变更经 updateConfig 单写。
import { Anchor, Database, Pin } from "lucide-react";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { SelectControl } from "../../shared/index";
import { normalizeRouterFallbackConfig } from "../../shared/router-fallback";
import {
  routerCompressionTierOptions,
  routerFallbackModeOptions
} from "../../shared/options";
import { SettingsSwitchRow } from "./settings";
import type { AppConfig, RouterCompressionTier } from "@/contracts/app";

const settingsSavingsContentWidthClassName = "mx-auto w-full max-w-[900px]";

export function SavingsSettingsSection({
  config,
  t,
  updateConfig
}: {
  config: AppConfig;
  t: (value: string) => string;
  updateConfig: (mutator: (config: AppConfig) => AppConfig) => void;
}) {
  const savings = config.savings;
  const fallback = config.Router.fallback;
  const compressionModeOptions = routerCompressionTierOptions.map((option) => ({
    label: t(option.label),
    value: option.value
  }));
  const fallbackModeOptions = routerFallbackModeOptions.map((option) => ({
    label: t(option.label),
    value: option.value
  }));

  const updateSavings = (patch: Partial<AppConfig["savings"]>) => {
    updateConfig((next) => ({
      ...next,
      savings: { ...next.savings, ...patch }
    }));
  };

  const updateFallback = (fallback: AppConfig["Router"]["fallback"]) => {
    updateConfig((next) => ({
      ...next,
      Router: { ...next.Router, fallback }
    }));
  };

  return (
    <section className={settingsSavingsContentWidthClassName + " grid grid-cols-1 gap-3"}>
      <div>
        <h4 className="text-body-lg font-semibold text-foreground">{t("Cost savings")}</h4>
        <p className="mt-0.5 text-caption leading-4 text-muted-foreground">
          {t("Applies to every request routed through the gateway. All actions default to off.")}
        </p>
      </div>

      <div className="grid grid-cols-1 gap-3 rounded-md border border-border bg-card p-3">
        <div className="grid gap-3 md:grid-cols-2">
          <div>
            <label className="text-caption font-medium text-muted-foreground">{t("Token compression")}</label>
            <div className="mt-1.5">
              <SelectControl
                onChange={(mode) => updateSavings({
                  compression: { ...savings.compression, mode: mode as RouterCompressionTier }
                })}
                options={compressionModeOptions}
                value={savings.compression.mode}
              />
            </div>
            <p className="mt-1.5 text-caption leading-4 text-muted-foreground">
              {t("Auto compresses long prompts past the threshold before they leave; Full always compresses; Off never does.")}
            </p>
          </div>
          {savings.compression.mode === "auto" ? (
            <div>
              <label className="text-caption font-medium text-muted-foreground">{t("Auto threshold (estimated tokens)")}</label>
              <div className="mt-1.5">
                <Input
                  min={0}
                  onChange={(event) => {
                    const tokens = Number(event.target.value);
                    if (Number.isFinite(tokens) && tokens >= 0) {
                      updateSavings({
                        compression: {
                          ...savings.compression,
                          thresholdTokens: Math.floor(tokens)
                        }
                      });
                    }
                  }}
                  type="number"
                  value={String(savings.compression.thresholdTokens)}
                />
              </div>
            </div>
          ) : null}
        </div>

        <SettingsSwitchRow
          icon={Database}
          checked={savings.responseCache.enabled}
          description={t("Cache identical responses (temperature 0) and replay them on hit - the upstream is not called again.")}
          label={t("Response cache")}
          onChange={(enabled) => updateSavings({
            responseCache: { ...savings.responseCache, enabled }
          })}
        />

        <SettingsSwitchRow
          icon={Anchor}
          checked={savings.sticky.enabled}
          description={t("Keep one provider across turns of the same session so its prompt cache stays warm. The binding is released when the target turns unhealthy.")}
          label={t("Session stickiness")}
          onChange={(enabled) => updateSavings({
            sticky: { ...savings.sticky, enabled }
          })}
        />
        {savings.sticky.enabled ? (
          <div className="md:pl-11">
            <label className="text-caption font-medium text-muted-foreground">{t("Sticky TTL (seconds)")}</label>
            <div className="mt-1.5 max-w-[220px]">
              <Input
                min={0}
                onChange={(event) => {
                  const secs = Number(event.target.value);
                  if (Number.isFinite(secs) && secs >= 0) {
                    updateSavings({
                      sticky: { ...savings.sticky, ttlSecs: Math.floor(secs) }
                    });
                  }
                }}
                type="number"
                value={String(savings.sticky.ttlSecs)}
              />
            </div>
          </div>
        ) : null}

        <SettingsSwitchRow
          icon={Pin}
          checked={savings.prefixPin.enabled}
          description={t("Pin requests that share the same cache_control prefix to the provider that served them first.")}
          label={t("Prefix pinning")}
          onChange={(enabled) => updateSavings({
            prefixPin: { enabled }
          })}
        />
      </div>

      <div className="grid grid-cols-1 gap-3 rounded-md border border-border bg-card p-3">
        <div>
          <label className="text-caption font-medium text-muted-foreground">{t("Global fallback")}</label>
          <div className="mt-1.5">
            <SelectControl
              onChange={(mode) => updateFallback(normalizeRouterFallbackConfig({ ...fallback, mode }))}
              options={fallbackModeOptions}
              value={fallback.mode}
            />
          </div>
          <p className="mt-1.5 text-caption leading-4 text-muted-foreground">
            {t("What to do when the primary model attempt fails: retry it, walk a fallback model chain, or stop.")}
          </p>
        </div>
        {fallback.mode === "retry" ? (
          <div className="max-w-[220px]">
            <label className="text-caption font-medium text-muted-foreground">{t("Retry count")}</label>
            <div className="mt-1.5">
              <Input
                min={0}
                onChange={(event) => {
                  const count = Number(event.target.value);
                  if (Number.isFinite(count) && count >= 0) {
                    updateFallback(normalizeRouterFallbackConfig({
                      ...fallback,
                      retryCount: Math.floor(count)
                    }));
                  }
                }}
                type="number"
                value={String(fallback.retryCount)}
              />
            </div>
          </div>
        ) : null}
        {fallback.mode === "model-chain" ? (
          <div>
            <label className="text-caption font-medium text-muted-foreground">{t("Fallback models (one per line)")}</label>
            <div className="mt-1.5">
              <Textarea
                onChange={(event) => {
                  updateFallback(normalizeRouterFallbackConfig({
                    ...fallback,
                    models: event.target.value.split(/\r?\n/)
                  }));
                }}
                rows={3}
                value={fallback.models.join("\n")}
              />
            </div>
          </div>
        ) : null}
      </div>
    </section>
  );
}
