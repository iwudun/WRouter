// R8 (2026-08-01): contracts/app.ts was a 2,088-line monolith mixing IPC
// request/result types, runtime constants, and normalize helpers.
// It is now a pure aggregate barrel — domain types live in the sibling files:
//   providers.ts  – gateway/provider/credential/account/probe/IPC
//   config.ts     – app/IPC surface, router, runtime-types, proxy-types, api-keys,
//                    appconfig, browser, usage-stats
//   logs.ts       – proxy network capture + request logs
//   runtime.ts    – runtime constants + value/validator helpers
// See contracts/README.md for the type-ownership convention. All 264 original
// exports are re-exported here for backward compatibility — 38 importers that
// use `@/contracts/app` need no changes.

export * from "./providers";
export * from "./config";
export * from "./logs";
export * from "./runtime";
// 20261030_01 阶段二:模型指纹检测与指纹库管理(方案 §3.1 新契约文件)。
export * from "./fingerprint";
