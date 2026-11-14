import type {
  AppConfig,
  AppInfo,
  AppUpdateStatus,
  GatewayStatus,
  ProxyCertificateStatus
} from "@/contracts/app";
import { createDefaultAppConfig } from "@/config";

export const fallbackInfo: AppInfo = {
  appConfigDbFile: "Browser preview",
  apiKeysDbFile: "Browser preview",
  configDir: "Browser preview",
  configFile: "Browser preview",
  dataDir: "Browser preview",
  gatewayConfigFile: "Browser preview",
  launchAtLoginSupported: /^Mac|^Win/i.test(navigator.platform),
  name: "WRouter",
  platform: navigator.platform,
  requestLogsDbFile: "Browser preview",
  usageDbFile: "Browser preview",
  version: "0.2.5"
};

export const fallbackUpdateStatus: AppUpdateStatus = {
  canCheck: false,
  canDownload: false,
  canInstall: false,
  currentVersion: fallbackInfo.version,
  state: "idle",
  supported: false
};

export const fallbackConfig: AppConfig = createDefaultAppConfig({
  generatedConfigFile: "Browser preview"
});

export const fallbackGatewayStatus: GatewayStatus = {
  endpoint: "http://127.0.0.1:3460",
  generatedConfigFile: "Browser preview",
  networkEndpoints: [],
  state: "stopped"
};

export const fallbackProxyCertificateStatus: ProxyCertificateStatus = {
  caCertFile: "Browser preview",
  canInstall: false,
  message: "Certificate detection is available in the desktop app.",
  platform: navigator.platform,
  state: "unknown",
  trusted: false
};
