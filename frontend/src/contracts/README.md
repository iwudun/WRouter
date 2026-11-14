# contracts/

IPC/类型契约层。前端与桌面壳之间的请求/结果类型，以及运行时常量与归一化 helper。

R8（2026-08-01）将原本的 `app.ts` 单体（2,088 行 / 264 导出）拆分为按域聚合的文件，`app.ts` 退化为纯 barrel。

## 文件职责

| 文件 | 职责 |
|---|---|
| `app.ts` | **纯聚合 barrel**（`export * from "./<domain>"`）。不在此处添加新定义——新类型归入下方对应域文件。 |
| `providers.ts` | gateway/provider/credential/account/probe/deep-link/IPC：`GatewayProviderConfig`、`ProviderAccount*`、`ProviderDeepLink*`、`GatewayProviderProbe*`、`ProviderIconDetection*`。 |
| `config.ts` | app/IPC surface（`AppInfo`、`AppUpdate*`）、router（`RouterConfig`/`RouterRule*`/`RouterFallback*`）、runtime-types（`GatewayRuntimeConfig`、`Proxy*`、`GatewayMcp*`、`ToolHub*`）、proxy-types（`ProxySystemStatus`/`ProxyCertificate*`）、api-keys（`ApiKey*`）、appconfig/app-level（`AppConfig`/`GatewayStatus`/`ProxyStatus`）、usage-stats（`UsageStats*`/`UsageTotals`/`UsageSeriesPoint`/`UsageComparisonRow`）。 |
| `logs.ts` | proxy network capture + request logs：`ProxyNetworkCaptureState`/`ProxyNetworkBody`/`ProxyNetworkExchange`/`ProxyNetworkSnapshot`、`RequestLog*` 全族、`ProxyCertificateInstallResult`。 |
| `runtime.ts` | 运行时常量 + 值/校验 helper：`CLAUDE_CODE_DEFAULT_ENV`、`CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY_ENV`、`NO_AVAILABLE_GATEWAY_MODELS_MESSAGE`、`assertAvailableGatewayModels`/`hasAvailableGatewayModels`/`availableGatewayModelIds`（含内部 helper）。 |
| `i18n.ts` | error-message i18n：`ErrorI18nLanguage`、`translateErrorMessage`/`formatLocalizedErrorMessage`。 |

## 类型归属约定

新增类型/常量/helper 时，按下表归位：

| 类别 | 归属 |
|---|---|
| IPC 请求/结果类型 | 对应域文件（providers/config/logs） |
| 运行时常量、跨域值/校验 helper | `runtime.ts` |
| error-message i18n | `i18n.ts` |
| UI/draft 类型（view 路由、表单 draft、toast 等） | `pages/home/shared/types.ts`（home 页内） |
| 跨页共享 helper（格式化、语言偏好、图标映射、DOM helper） | `lib/`（见 `lib/format.ts`、`lib/language.ts`、`lib/tray-icons.ts`、`lib/dom.ts`） |

## 向后兼容

`app.ts` barrel 重新导出全部 264 个原始符号，38 个 `@wr/core/contracts/app` 导入站点无需改动。下游应继续从 `@wr/core/contracts/app` 导入（而非直接导入域文件），以保持 barrel 的聚合语义。
