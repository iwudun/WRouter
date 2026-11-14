import { useState } from "react";
import {
  Activity, Database, FileText, LoaderCircle, Power
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { api, hasBridge } from "@/lib/api";
import {
  AppConfig, AppCopy, AppInfo, AppLanguagePreference, endpointFromHostPort, Field, formatAppError,  formatSystemOption,
       languageDisplayName,
        numberValue,
   ResolvedLanguage, ResolvedTheme, SelectControl,
  SettingsPageId, themeDisplayName,
} from "../../shared/index";
import { AboutSettingsPage } from "./settings-about";
// 20260906_01 F2:代理分区(默认策略 + 命名预设 CRUD)独立成文件。
import { ProxySettingsSection } from "./settings-proxy";
// 20261101_02 T3-B:成本节省分区(全局 savings 开关 + 全局回退)独立成文件。
import { SavingsSettingsSection } from "./settings-savings";
// 20261102_01 P6:服务控制分区(暂停 / 恢复 / 退出服务)独立成文件。
import { ServiceControlSection } from "./settings-service";

const settingsPageContentWidthClassName = "mx-auto w-full max-w-[900px]";

export function SettingsView({
  appInfo,
  config,
  copy,
  languagePreference,
  launchAtLogin,
  notify,
  onChangeLaunchAtLogin,
  onChangeObservability,
  onChangeProxy,
  onChangeLanguage,
  onChangeTheme,
  onServiceStopping,
  observability,
  page,
  systemLanguage,
  systemTheme,
  themePreference,
  updateConfig
}: {
  appInfo: AppInfo;
  config: AppConfig;
  copy: AppCopy;
  languagePreference: AppLanguagePreference;
  launchAtLogin: boolean;
  /** 20261102_01 P6:toast 提示(服务控制等分区共用)。 */
  notify?: (message: string) => void;
  onChangeLaunchAtLogin: (checked: boolean) => void;
  onChangeObservability: (patch: Partial<AppConfig["observability"]>) => void;
  onChangeProxy: (patch: Partial<AppConfig["proxy"]>) => void;
  onChangeLanguage: (value: string) => void;
  onChangeTheme: (value: string) => void;
  /** 20261102_01 P6:「停止服务」**已被受理** → 切「正在停止」落地页(2026-11-03)。 */
  onServiceStopping?: (restartHint: string) => void;
  observability: AppConfig["observability"];
  /** 20261101_03:设置分区菜单上移到主侧栏(设置模式覆盖首页菜单),
   *  本视图受控只渲染激活分区,不再持有内部页签列。 */
  page: SettingsPageId;
  systemLanguage: ResolvedLanguage;
  systemTheme: ResolvedTheme;
  themePreference: AppConfig["theme"];
  updateConfig: (mutator: (config: AppConfig) => AppConfig) => void;
}) {
  return (
    // 与其它视图一样占满 main 区域(滚动由本视图自管,见 viewUsesInternalScroll)。
    <section className="h-full min-h-0 overflow-y-auto p-5 max-[720px]:p-3">
      <div className={settingsPageContentWidthClassName}>
        {page === "general" ? (
          <GeneralSettingsPage
            appInfo={appInfo}
            config={config}
            copy={copy}
            launchAtLogin={launchAtLogin}
            launchAtLoginSupported={appInfo.launchAtLoginSupported}
            notify={notify}
            onChangeLaunchAtLogin={onChangeLaunchAtLogin}
            onChangeProxy={onChangeProxy}
            onServiceStopping={onServiceStopping}
            updateConfig={updateConfig}
          />
        ) : null}
        {page === "appearance" ? (
          <AppearanceSettingsPage
            copy={copy}
            languagePreference={languagePreference}
            onChangeLanguage={onChangeLanguage}
            onChangeTheme={onChangeTheme}
            systemLanguage={systemLanguage}
            systemTheme={systemTheme}
            themePreference={themePreference}
          />
        ) : null}
        {page === "observability" ? (
          <ObservabilitySettingsPage
            copy={copy}
            observability={observability}
            onChange={onChangeObservability}
          />
        ) : null}
        {page === "about" ? (
          <AboutSettingsPage
            appInfo={appInfo}
            copy={copy}
          />
        ) : null}
      </div>
    </section>
  );
}

function AppearanceSettingsPage({
  copy,
  languagePreference,
  onChangeLanguage,
  onChangeTheme,
  systemLanguage,
  systemTheme,
  themePreference
}: {
  copy: AppCopy;
  languagePreference: AppLanguagePreference;
  onChangeLanguage: (value: string) => void;
  onChangeTheme: (value: string) => void;
  systemLanguage: ResolvedLanguage;
  systemTheme: ResolvedTheme;
  themePreference: AppConfig["theme"];
}) {
  const themeOptions = [
    { label: formatSystemOption(copy.settings.themeSystem, themeDisplayName(systemTheme, copy)), value: "system" },
    { label: copy.settings.themeLight, value: "light" },
    { label: copy.settings.themeDark, value: "dark" }
  ];
  const languageOptions = [
    { label: formatSystemOption(copy.settings.languageSystem, languageDisplayName(systemLanguage, copy)), value: "system" },
    { label: copy.settings.languageChinese, value: "zh" },
    { label: copy.settings.languageEnglish, value: "en" }
  ];

  return (
    <div className={cn(settingsPageContentWidthClassName, "grid grid-cols-1 gap-5")}>
      <section className="grid grid-cols-1 gap-3">
        <h4 className="text-body-lg font-semibold text-foreground">{copy.settings.theme}</h4>
        <SelectControl onChange={onChangeTheme} options={themeOptions} value={themePreference} />
      </section>
      <section className="grid grid-cols-1 gap-3">
        <h4 className="text-body-lg font-semibold text-foreground">{copy.settings.language}</h4>
        <SelectControl onChange={onChangeLanguage} options={languageOptions} value={languagePreference} />
      </section>
    </div>
  );
}

function GeneralSettingsPage({
  appInfo,
  config,
  copy,
  launchAtLogin,
  launchAtLoginSupported,
  notify,
  onChangeLaunchAtLogin,
  onChangeProxy,
  onServiceStopping,
  updateConfig
}: {
  appInfo: AppInfo;
  config: AppConfig;
  copy: AppCopy;
  launchAtLogin: boolean;
  launchAtLoginSupported: boolean;
  notify?: (message: string) => void;
  onChangeLaunchAtLogin: (checked: boolean) => void;
  onChangeProxy: (patch: Partial<AppConfig["proxy"]>) => void;
  onServiceStopping?: (restartHint: string) => void;
  updateConfig: (mutator: (config: AppConfig) => AppConfig) => void;
}) {
  return (
    <div className={cn(settingsPageContentWidthClassName, "grid grid-cols-1 gap-5")}>
      <ServerSettingsSection config={config} copy={copy} updateConfig={updateConfig} />
      {launchAtLoginSupported ? (
        <SettingsSwitchRow
          checked={launchAtLogin}
          description={copy.settings.launchAtLoginDescription}
          icon={Power}
          label={copy.settings.launchAtLogin}
          onChange={onChangeLaunchAtLogin}
        />
      ) : null}
      {/* 20261102_01 P6:服务控制(暂停 / 恢复 / 退出服务)——通用能力,所有运行时通用。 */}
      <ServiceControlSection
        notify={notify}
        onServiceStopping={onServiceStopping}
        t={(value) => copy.text[value] ?? value}
      />
      <ProxySettingsSection config={config} copy={copy} onChange={onChangeProxy} />
      <SavingsSettingsSection config={config} t={(value) => copy.text[value] ?? value} updateConfig={updateConfig} />
      <DataSettingsSection appInfo={appInfo} copy={copy} />
    </div>
  );
}

function ServerSettingsSection({
  config,
  copy,
  updateConfig
}: {
  config: AppConfig;
  copy: AppCopy;
  updateConfig: (mutator: (config: AppConfig) => AppConfig) => void;
}) {
  const t = (value: string) => copy.text[value] ?? value;

  return (
    <section className="grid grid-cols-1 gap-3">
      <h4 className="text-body-lg font-semibold text-foreground">{t("Router address")}</h4>
      <div className="grid grid-cols-1 gap-3 rounded-md border border-border/70 bg-card/70 p-3 md:grid-cols-2">
        <Field label={t("Host")}>
          <Input
            value={config.HOST}
            onChange={(event) => updateConfig((next) => {
              const host = event.target.value;
              return {
                ...next,
                HOST: host,
                gateway: { ...next.gateway, host },
                routerEndpoint: endpointFromHostPort(host, next.PORT)
              };
            })}
          />
        </Field>
        <Field label={t("Port")}>
          <Input
            type="number"
            value={String(config.PORT)}
            onChange={(event) => updateConfig((next) => {
              const port = numberValue(event.target.value);
              return {
                ...next,
                PORT: port,
                gateway: { ...next.gateway, port },
                routerEndpoint: endpointFromHostPort(next.HOST, port)
              };
            })}
          />
        </Field>
      </div>
    </section>
  );
}

function ObservabilitySettingsPage({
  copy,
  observability,
  onChange
}: {
  copy: AppCopy;
  observability: AppConfig["observability"];
  onChange: (patch: Partial<AppConfig["observability"]>) => void;
}) {
  return (
    <div className={cn(settingsPageContentWidthClassName, "grid grid-cols-1 gap-5")}>
      <div className="grid grid-cols-1 gap-3">
        <SettingsSwitchRow
          checked={observability.callLogs}
          description={copy.settings.callLogsDescription}
          icon={Activity}
          label={copy.settings.callLogs}
          onChange={(callLogs) => onChange({ callLogs })}
        />
        <SettingsSwitchRow
          checked={observability.captureBodies}
          description={copy.settings.captureBodiesDescription}
          icon={FileText}
          label={copy.settings.captureBodies}
          onChange={(captureBodies) => onChange({ captureBodies })}
        />
        <div className="rounded-md border border-border bg-muted/20 px-3 py-3">
          <Field label={copy.settings.bodyRetention}>
            <Input
              min={0}
              onChange={(event) => {
                const days = Number(event.target.value);
                if (Number.isFinite(days) && days >= 0) {
                  onChange({ bodyRetentionDays: Math.floor(days) });
                }
              }}
              type="number"
              value={String(observability.bodyRetentionDays)}
            />
          </Field>
          <p className="mt-1.5 text-caption leading-4 text-muted-foreground">{copy.settings.bodyRetentionDescription}</p>
        </div>
      </div>
    </div>
  );
}


export function SettingsSwitchRow({
  checked,
  description,
  icon: Icon,
  label,
  onChange
}: {
  checked: boolean;
  description: string;
  icon: typeof Activity;
  label: string;
  onChange: (checked: boolean) => void;
}) {
  return (
    <div className="flex min-w-0 items-center gap-3 rounded-md border border-border bg-muted/20 px-3 py-3">
      <span className={cn(
        "flex h-8 w-8 shrink-0 items-center justify-center rounded-md",
        checked ? "bg-primary/10 text-primary" : "bg-muted text-muted-foreground"
      )}>
        <Icon className="h-4 w-4" />
      </span>
      <div className="min-w-0 flex-1">
        <div className="text-body-lg font-semibold text-foreground">{label}</div>
        <div className="mt-0.5 text-caption leading-4 text-muted-foreground">{description}</div>
      </div>
      <Switch checked={checked} onCheckedChange={onChange} />
    </div>
  );
}

function DataSettingsSection({
  appInfo,
  copy
}: {
  appInfo: AppInfo;
  copy: AppCopy;
}) {
  const t = (value: string) => copy.text[value] ?? value;
  const [exporting, setExporting] = useState(false);
  const [error, setError] = useState("");
  const [exportedFile, setExportedFile] = useState("");

  async function exportData() {
    if (exporting) {
      return;
    }
    if (!hasBridge()) {
      setError(t("Data export is only available in the desktop app."));
      return;
    }
    setExporting(true);
    setError("");
    setExportedFile("");
    try {
      const result = await api.exportData();
      if (!result.canceled) {
        setExportedFile(result.file || "");
      }
    } catch (exportError) {
      setError(formatAppError(copy, exportError));
    } finally {
      setExporting(false);
    }
  }

  return (
    <section className="grid grid-cols-1 gap-3">
      <div className="min-w-0">
        <h4 className="text-body-lg font-semibold text-foreground">{copy.settings.data}</h4>
      </div>

      <div className="grid gap-2 rounded-md border border-border bg-background p-3">
        <DataPathRow label={t("Config database")} value={appInfo.appConfigDbFile} />
        <DataPathRow label={t("API key database")} value={appInfo.apiKeysDbFile} />
        <DataPathRow label={t("Request log database")} value={appInfo.requestLogsDbFile} />
        <DataPathRow label={t("Usage database")} value={appInfo.usageDbFile} />
      </div>

      <div className="rounded-md border border-border bg-muted/20 px-3 py-3">
        <div className="flex min-w-0 items-start gap-3">
          <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-primary/10 text-primary">
            <Database className="h-4 w-4" />
          </span>
          <div className="min-w-0 flex-1">
            <div className="text-body-lg font-semibold text-foreground">{t("Export data")}</div>
            <div className="mt-0.5 text-caption leading-4 text-muted-foreground">
              {t("Export current configuration and SQLite data into a single JSON backup file. The export contains API keys and should be stored securely.")}
            </div>
          </div>
          <Button disabled={exporting} onClick={exportData} size="sm" type="button">
            {exporting ? <LoaderCircle className="h-4 w-4 animate-spin" /> : <Database className="h-4 w-4" />}
            {exporting ? t("Exporting") : t("Export")}
          </Button>
        </div>
        {exportedFile ? (
          <div className="mt-3 rounded-md border border-emerald-200 bg-emerald-50 px-3 py-2 text-caption text-emerald-800 dark:border-emerald-900/60 dark:bg-emerald-950/30 dark:text-emerald-200">
            <span className="font-medium">{t("Exported")}:</span> <span className="break-all">{exportedFile}</span>
          </div>
        ) : null}
        {error ? (
          <div className="mt-3 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-caption text-destructive">
            {error}
          </div>
        ) : null}
      </div>
    </section>
  );
}

function DataPathRow({
  label,
  value
}: {
  label: string;
  value: string;
}) {
  return (
    <div className="grid min-w-0 gap-1 rounded-md bg-muted/20 px-2 py-2 sm:grid-cols-[160px_minmax(0,1fr)] sm:items-center">
      <div className="text-caption font-medium text-muted-foreground">{label}</div>
      <div className="min-w-0 break-all font-mono text-caption text-foreground">{value}</div>
    </div>
  );
}


