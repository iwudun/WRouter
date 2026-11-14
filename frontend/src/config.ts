import {
  type AppConfig,
  type ProxyRouteTarget
} from "@/contracts/app";

export const DEFAULT_PROXY_TARGETS: ProxyRouteTarget[] = [
  { host: "api.anthropic.com", paths: ["/v1/messages", "/v1/messages/count_tokens"] },
  { host: "api.openai.com", paths: ["/v1/chat/completions", "/v1/responses", "/v1/models"] },
  { host: "generativelanguage.googleapis.com", paths: ["/v1beta/models", "/v1/models"] },
  { host: "openrouter.ai", paths: ["/api/v1/chat/completions", "/api/v1/responses", "/api/v1/models"] },
  { host: "api.deepseek.com", paths: ["/chat/completions", "/v1/chat/completions", "/models", "/v1/models"] },
  { host: "api.mistral.ai", paths: ["/v1/chat/completions", "/v1/models"] }
];

export type DefaultAppConfigOptions = {
  generatedConfigFile: string;
};

export function createDefaultAppConfig(options: DefaultAppConfigOptions): AppConfig {
  return {
    APIKEY: "",
    APIKEYS: [],
    HOST: "127.0.0.1",
    PORT: 3460,
    Providers: [],
    Router: {
      fallback: {
        mode: "off",
        models: [],
        retryCount: 1
      }
    },
    autoStart: false,
    gateway: {
      enabled: true,
      generatedConfigFile: options.generatedConfigFile,
      host: "127.0.0.1",
      port: 3460
    },
    launchAtLogin: false,
    modelGroups: [],
    observability: {
      bodyRetentionDays: 7,
      callLogs: true,
      captureBodies: false
    },
    preferredProvider: "",
    savings: {
      compression: {
        mode: "off",
        thresholdTokens: 32000
      },
      prefixPin: {
        enabled: false
      },
      responseCache: {
        diskMaxEntries: 1000,
        diskTtlSecs: 3600,
        enabled: false,
        memoryMaxBytes: 5 * 1024 * 1024,
        memoryMaxEntries: 64,
        memoryTtlSecs: 1800
      },
      sticky: {
        enabled: false,
        ttlSecs: 900,
        cacheColdSecs: 300
      }
    },
    proxy: {
      browserMode: true,
      host: "127.0.0.1",
      mode: "gateway",
      port: 7890,
      presets: [],
      targets: DEFAULT_PROXY_TARGETS,
      upstream: {
        mode: "none",
        presetId: ""
      }
    },
    routerEndpoint: "http://127.0.0.1:3460",
    theme: "system"
  };
}
