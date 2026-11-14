/**
 * WRouter 版本/构建清单生成器.
 *
 * 生成 build/build-info.json, 包含:
 *   - version     — 从 package.json 读取
 *   - buildTime   — ISO8601 构建时间
 *   - gitCommit   — 当前 git commit hash (若有 git)
 *   - platform    — win32 / linux / darwin
 *   - arch        — x64 / arm64
 *   - nodeVersion — process.version
 *   - glibcVersion  — Linux glibc 版本 (若有)
 *   - dependencies — 关键依赖摘要 (从 package-lock.json 版本映射)
 *
 * 用法:
 *   node scripts/generate-build-info.mjs
 *
 * 输出: build/build-info.json (幂等)
 */

import { existsSync, readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { execSync } from "node:child_process";

const __dirname = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(__dirname, "..");
const outDir = resolve(projectRoot, "build");
const outFile = resolve(outDir, "build-info.json");

function log(msg) {
  console.error(`[build-info] ${msg}`);
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf-8"));
}

function tryExec(cmd, timeout = 5000) {
  try {
    return execSync(cmd, { encoding: "utf-8", timeout }).trim();
  } catch {
    return null;
  }
}

function getGlibcVersion() {
  const v = tryExec("ldd --version 2>&1 | head -1");
  if (!v) return null;
  const m = v.match(/ldd\s+\(.*?\)\s+([\d.]+)/);
  return m ? m[1] : v;
}

function getGitCommit() {
  return tryExec("git rev-parse HEAD 2>/dev/null");
}

function getDependencySummary() {
  // 20261102_01 P5-5:Electron 退场 → 桌面壳的构建工具链是 Tauri CLI
  // (装在 desktop/ 这个独立 npm 工程里),不再是 electron / electron-builder。
  try {
    const deps = {};
    const desktopLock = resolve(projectRoot, "desktop", "package-lock.json");
    if (existsSync(desktopLock)) {
      const packages = JSON.parse(readFileSync(desktopLock, "utf-8")).packages || {};
      for (const [name, info] of Object.entries(packages)) {
        const pkgName = name.split("node_modules/").pop();
        if (pkgName && info.version) deps[pkgName] = info.version;
      }
    }
    const keyDeps = {};
    for (const k of ["@tauri-apps/cli"]) {
      if (deps[k]) keyDeps[k] = deps[k];
    }
    return keyDeps;
  } catch {
    return null;
  }
}

function getProductName() {
  // 20261102_01 P5-5:产品名的事实源在 Tauri 配置(原先读 electron-builder 的 build 段)。
  try {
    const conf = JSON.parse(
      readFileSync(resolve(projectRoot, "desktop", "src-tauri", "tauri.conf.json"), "utf-8"),
    );
    return conf.productName || "WRouter";
  } catch {
    return "WRouter";
  }
}

async function main() {
  mkdirSync(outDir, { recursive: true });

  // 读取 package.json
  const pkg = readJson(resolve(projectRoot, "package.json"));

  const info = {
    version: pkg.version || "0.0.0",
    appName: pkg.name || "wrouter-desktop",
    productName: getProductName(),
    buildTime: new Date().toISOString(),
    buildTool: "tauri-cli",
    gitCommit: getGitCommit(),
    platform: process.platform,
    arch: process.arch,
    nodeVersion: process.version,
    glibcVersion: getGlibcVersion(),
    dependencies: getDependencySummary(),
  };

  writeFileSync(outFile, JSON.stringify(info, null, 2) + "\n", "utf-8");
  log(`written: ${outFile} (${JSON.stringify(info).length} bytes)`);
}

main().catch((e) => {
  log(`FATAL: ${e.message}`);
  process.exit(1);
});