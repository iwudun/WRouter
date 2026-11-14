#!/usr/bin/env node
// WRouter 能力元数据一致性校验(R2.1)
//
// 20261102_01 P5-5:Electron 壳退场 → 校验从「四方」收敛为「三方 + 前端业务桥」:
//
//   1. JSON.managementRpc ↔ 后端 RPC 注册表(Rust dispatch)
//   2. JSON.desktopMethods ↔ Tauri 桥接层 DESKTOP_METHODS
//   3. 前端业务桥(businessBridge)⊆ JSON.managementRpc ∪ JSON.deferredMethods
//   4. JSON.methodStage 键 ⊆ managementRpc ∪ desktopMethods ∪ deferredMethods
//   5. JSON.desktopMethodsByRuntime 只允许 tauri(electron 已退场)
//   6. JSON.desktopMethodsByRuntime.tauri ↔ Tauri 桥接层
//   7. JSON.desktopMethods == 各运行时并集
//
// 用法: node scripts/validate-capabilities.mjs
// 返回: 0 = 一致;1 = 存在差异(输出差异明细)

import { readFileSync, existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const CAPABILITIES = resolve(ROOT, "contracts", "wrouter-capabilities.json");
const RUST_DISPATCH = resolve(ROOT, "backend", "crates", "wr-server", "src", "admin", "config.rs");
const TAURI_BRIDGE = resolve(ROOT, "desktop", "src-tauri", "src", "bridge_core.rs");
const WEB_BRIDGE = resolve(ROOT, "frontend", "src", "web-client-bridge.ts");
// P5-5:Electron 壳已退场 —— 这两个路径若仍存在,说明退场没做完,直接报错。
const RETIRED_PATHS = [
  resolve(ROOT, "electron", "src", "preload", "preload.ts"),
  resolve(ROOT, "electron", "src", "ipc.ts"),
];

const failures = [];

function check(name, detail, actual, expected) {
  const diffActual = [...actual].filter((x) => !expected.has(x));
  const diffExpected = [...expected].filter((x) => !actual.has(x));
  if (diffActual.length || diffExpected.length) {
    failures.push(`[${name}] ${detail}`);
    if (diffExpected.length) failures.push(`  JSON 有、代码无：${diffExpected.sort().join(", ")}`);
    if (diffActual.length) failures.push(`  代码有、JSON 无：${diffActual.sort().join(", ")}`);
  } else {
    console.log(`✓ ${name}：${actual.size} 项一致`);
  }
}

function extractSet(text, pattern) {
  const set = new Set();
  for (const match of text.matchAll(pattern)) set.add(match[1]);
  return set;
}

// ---- 退场守卫:Electron 侧文件不得复活 ----
for (const retired of RETIRED_PATHS) {
  if (existsSync(retired)) {
    failures.push(`[Electron 退场守卫] ${retired.replace(ROOT, ".")} 仍存在 —— P5-5 已删除 Electron 壳`);
  }
}

// ---- 来源 1：JSON ----
const caps = JSON.parse(readFileSync(CAPABILITIES, "utf8"));
const jsonRpc = new Set(caps.managementRpc);
const jsonDesktop = new Set(caps.desktopMethods);
const jsonDeferred = new Set(caps.deferredMethods || []);
const jsonStaged = new Set(Object.keys(caps.methodStage || {}));
// 20261102_01 P3-5：桌面方法按运行时归类；P5-5 起只剩 tauri。
const jsonDesktopByRuntime = caps.desktopMethodsByRuntime || {};
const jsonDesktopTauri = new Set(jsonDesktopByRuntime.tauri || []);

// ---- 来源 2：后端 RPC 注册表（Rust，R7 起 Python 后端退役） ----
// 以 wr-server 的 admin dispatch match 为权威;逐行取 `=>` 之前的带引号
// 标识,天然覆盖 `"a" | "b" => ...` 多臂与 `{` 换行臂。
const dispatchSrc = readFileSync(RUST_DISPATCH, "utf8");
const dispatchStart = dispatchSrc.indexOf("async fn dispatch");
if (dispatchStart < 0) {
  console.error(`未找到 "async fn dispatch"(Rust 注册表结构漂移?请检查 ${RUST_DISPATCH})`);
  process.exit(1);
}
const dispatchBody = dispatchSrc.slice(dispatchStart, dispatchSrc.indexOf("\n}\n", dispatchStart));
const backendRpc = new Set();
for (const line of dispatchBody.split("\n")) {
  if (!line.includes("=>")) continue;
  for (const match of line.split("=>")[0].matchAll(/"([a-zA-Z][a-zA-Z0-9]*)"/g)) {
    backendRpc.add(match[1]);
  }
}
if (backendRpc.size === 0) {
  console.error("Rust dispatch 注册表提取为空(解析逻辑与源码结构漂移?)");
  process.exit(1);
}

// ---- 来源 3：Tauri 桥接层（desktop/src-tauri/src/bridge_core.rs）----
// `pub const DESKTOP_METHODS: &[&str]` 是 Tauri 侧桌面方法的唯一声明点
// (init script 由此生成)。
const tauriBridgeSrc = readFileSync(TAURI_BRIDGE, "utf8");
const tauriDesktopMethods = extractSet(
  (tauriBridgeSrc.match(/pub const DESKTOP_METHODS[^=]*=\s*&\[([\s\S]*?)\];/) || [])[1] || "",
  /"([a-zA-Z][a-zA-Z0-9]*)"/g,
);
if (tauriDesktopMethods.size === 0) {
  console.error(`未找到 Tauri 桥接层的 DESKTOP_METHODS（结构漂移?请检查 ${TAURI_BRIDGE}）`);
  process.exit(1);
}

// ---- 来源 4：前端业务桥（frontend/src/web-client-bridge.ts）----
// businessBridge 是「桌面 / Web 共用同一套 RPC 逻辑」的业务方法集合。
// 桌面方法（exportData / getUpdateStatus 等）不在此列 —— 它们由壳层提供。
const webBridgeSrc = readFileSync(WEB_BRIDGE, "utf8");
const businessBlock = (webBridgeSrc.match(/const businessBridge[^=]*=\s*\{([\s\S]*?)\n\};/) || [])[1] || "";
const frontendBusinessMethods = extractSet(businessBlock, /^\s{2}([a-zA-Z][a-zA-Z0-9]*):/gm);
if (frontendBusinessMethods.size === 0) {
  console.error(`未找到前端 businessBridge（结构漂移?请检查 ${WEB_BRIDGE}）`);
  process.exit(1);
}

// ---- 校验 ----
check("JSON.managementRpc ↔ 后端注册表", "RPC 方法集合漂移", backendRpc, jsonRpc);
check("JSON.desktopMethods ↔ Tauri 桥接层", "桌面方法集合漂移", tauriDesktopMethods, jsonDesktop);

const businessMissing = [...frontendBusinessMethods].filter((x) => !jsonRpc.has(x) && !jsonDeferred.has(x));
if (businessMissing.length) {
  failures.push(`[前端 businessBridge ⊆ JSON.managementRpc ∪ deferredMethods] ${businessMissing.length} 项不在 JSON`);
  failures.push(`  ${businessMissing.sort().join(", ")}`);
} else {
  console.log(`✓ 前端 businessBridge ⊆ JSON.managementRpc ∪ deferredMethods：${frontendBusinessMethods.size} 项均在 JSON`);
}

const stagedUndeclared = [...jsonStaged].filter((x) => !jsonRpc.has(x) && !jsonDesktop.has(x) && !jsonDeferred.has(x));
if (stagedUndeclared.length) {
  failures.push(`[JSON.methodStage 键必须是真实方法] ${stagedUndeclared.join(", ")} 未在 managementRpc/desktopMethods/deferredMethods 中声明`);
} else {
  console.log(`✓ JSON.methodStage 键：${jsonStaged.size} 项均为真实方法`);
}

// P5-5：electron 运行时已退场,契约里不得再声明 electron 桌面方法。
const retiredRuntimes = Object.keys(jsonDesktopByRuntime).filter((k) => k !== "tauri");
if (retiredRuntimes.length) {
  failures.push(`[JSON.desktopMethodsByRuntime] 不得再声明已退场运行时：${retiredRuntimes.join(", ")}（P5-5 Electron 已退场）`);
} else {
  console.log("✓ JSON.desktopMethodsByRuntime：仅剩 tauri（Electron 已退场）");
}

check(
  "JSON.desktopMethodsByRuntime.tauri ↔ Tauri 桥接层",
  "Tauri 桌面方法集合漂移",
  tauriDesktopMethods,
  jsonDesktopTauri,
);
const union = new Set([...jsonDesktopTauri]);
const unionDiff = [...union].filter((x) => !jsonDesktop.has(x));
const unionMissing = [...jsonDesktop].filter((x) => !union.has(x));
if (unionDiff.length || unionMissing.length) {
  failures.push("[JSON.desktopMethods == 各运行时并集] 桌面方法总表与按运行时归类不一致");
  if (unionMissing.length) failures.push(`  总表有、按运行时无：${unionMissing.sort().join(", ")}`);
  if (unionDiff.length) failures.push(`  按运行时有、总表无：${unionDiff.sort().join(", ")}`);
} else {
  console.log(`✓ JSON.desktopMethods == 各运行时并集：${union.size} 项一致`);
}

if (failures.length) {
  console.error("\n❌ 能力元数据校验失败：");
  console.error(failures.join("\n"));
  process.exit(1);
}
console.log("\n✓ 三方一致：JSON ↔ 后端 RPC ↔ Tauri 桥接层（前端业务桥 ⊆ JSON）");
