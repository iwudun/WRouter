/**
 * WRouter 版本号同步器.
 *
 * 版本唯一手改处: 根 package.json 的 "version". 本脚本把该值同步到所有
 * 派生位置:
 *   1. frontend/package.json                    — npm 元数据
 *   2. package-lock.json                        — 顶层 version + packages[""].version
 *   3. frontend/package-lock.json               — 顶层 version + packages[""]
 *   4. frontend/src/pages/home/shared/fallbacks.ts — 浏览器预览兜底的 version
 *   5. desktop/src-tauri/tauri.conf.json            — Tauri 桌面壳版本(20261102_01 P3)
 *   6. desktop/package.json + desktop/package-lock.json — Tauri app dir 的 npm
 *      元数据(20261102_01 P5 前置:desktop/ 是独立的 npm 工程,装 @tauri-apps/cli)
 *
 * 后端版本号经 WR_VERSION 环境变量在构建时注入(packaging/build-rust.sh 从
 * 本文件读取),无需同步 Cargo.toml。桌面壳同理(壳内嵌 wr-server,版本经
 * scripts/build-desktop.mjs 注入同一来源);开发态经 scripts/dev.mjs 注入。
 *
 * 本脚本同时是**版本注入点守门**:检查每个会编译 wr-server 的入口都注入了
 * WR_VERSION —— 漏注入不会报错,只会让版本号静默退化成 0.0.0-dev。
 *
 * 无需同步的位置: Cargo.toml 里的 workspace.package.version(不是对外版本来源)。
 * (R7: 原 backend/src/wrouter/__init__.py 目标随 Python 后端退役删除;
 *  20261102_01 P5-5: 原 electron/src/update-service.ts 随 Electron 壳退场删除。)
 *
 * 用法:
 *   node scripts/sync-version.mjs           # 同步写入
 *   node scripts/sync-version.mjs --check   # 只校验不写入, 不一致时退出码 1
 */

import { readFileSync, writeFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(__dirname, "..");
const checkOnly = process.argv.includes("--check");

function log(msg) {
  console.error(`[sync-version] ${msg}`);
}

const rootPkg = JSON.parse(readFileSync(resolve(projectRoot, "package.json"), "utf-8"));
const version = rootPkg.version;
if (!/^\d+\.\d+\.\d+(-[\w.]+)?$/.test(version)) {
  log(`FATAL: 根 package.json 的 version 非法: ${JSON.stringify(version)}`);
  process.exit(1);
}

// ---------------------------------------------------------------------------
// WR_VERSION 注入点守门
// ---------------------------------------------------------------------------
// 后端/桌面壳的版本是**编译期**常量(option_env!("WR_VERSION")):编译 wr-server
// 时没注入就退化成 "0.0.0-dev" —— 开发态表现为设置页「当前版本」显示
// 0.0.0-dev,发布态表现为产物版本号是错的。这是**静默**缺陷(不报错,只是
// 数字不对),所以每个会编译 wr-server 的入口都必须注入,且注入点要被守门。
//
// 历史教训:scripts/dev.mjs 曾漏注入,开发态设置页长期显示 0.0.0-dev,
// 而发布链路(build-rust.sh / build-desktop.mjs)是对的 —— 只修发布链会漏掉它。
const injectionPoints = [
  { file: "scripts/dev.mjs", pattern: /WR_VERSION\s*:/ },
  { file: "scripts/build-desktop.mjs", pattern: /WR_VERSION\s*:/ },
  { file: "packaging/build-rust.sh", pattern: /export WR_VERSION=/ },
  { file: "packaging/Dockerfile.runtime", pattern: /export WR_VERSION=/ },
];

for (const point of injectionPoints) {
  const path = resolve(projectRoot, point.file);
  let content;
  try {
    content = readFileSync(path, "utf-8");
  } catch {
    log(`FATAL: 版本注入点缺失: ${point.file} 不存在`);
    process.exit(1);
  }
  if (!point.pattern.test(content)) {
    log(`FATAL: 版本注入点失效: ${point.file} 未注入 WR_VERSION`);
    log(`       (编译期 option_env! 取不到就会退化成 0.0.0-dev —— 版本号是错的)`);
    process.exit(1);
  }
  log(`ok ${point.file} (WR_VERSION 注入点)`);
}

// 每个戳记 = 锚定正则, 捕获组 1/2 保留前后文(含换行风格), 中间替换为版本号.
// applyStamp 断言全文恰好匹配 1 处, 防止文件结构变化后误写.
// ``^`` 行首锚点不能省: 锁文件里每个依赖都有 "version" 字段, 不带锚点时
// 顶层戳记会子串匹配到所有依赖版本(几百处)而中止.
const topJsonVersion = { label: '顶层 "version"', pattern: '(^  "version": ")[^"]*(")' };
const pkgJsonVersion = (name) => ({
  label: `packages[""] 的 version (${name})`,
  pattern: `(      "name": "${name}",\\r?\\n      "version": ")[^"]*(")`,
});

const targets = [
  {
    file: "frontend/package.json",
    read: (c) => [JSON.parse(c).version],
    stamps: [topJsonVersion],
  },
  {
    file: "package-lock.json",
    read: (c) => {
      const lock = JSON.parse(c);
      return [lock.version, lock.packages?.[""]?.version];
    },
    stamps: [topJsonVersion, pkgJsonVersion(rootPkg.name)],
  },
  {
    file: "frontend/package-lock.json",
    read: (c) => {
      const lock = JSON.parse(c);
      // 2026-11-07 W4:frontend 不再依赖 wrouter-desktop,锁文件里的父目录
      // packages[".."] 快照随依赖移除,只剩顶层 version 与 packages[""]。
      return [lock.version, lock.packages?.[""]?.version];
    },
    stamps: [topJsonVersion, pkgJsonVersion("wrouter-ui")],
  },
  {
    file: "frontend/src/pages/home/shared/fallbacks.ts",
    read: (c) => [c.match(/^  version: "(.+?)"\r?$/m)?.[1]],
    stamps: [{ label: "fallbackInfo version", pattern: "(  version: \")[^\"]*(\")" }],
  },
  {
    // 20261102_01 P3:Tauri 桌面壳的 tauri.conf.json 也带版本号。
    file: "desktop/src-tauri/tauri.conf.json",
    read: (c) => [JSON.parse(c).version],
    stamps: [{ label: "tauri.conf.json version", pattern: "(\"version\": \")[^\"]*(\")" }],
  },
  {
    // 20261102_01 P5 前置:desktop/ 是 Tauri 的 app dir,自成一个 npm 工程
    // (@tauri-apps/cli 要装在这里,`tauri dev/build` 才能找到 src-tauri/)。
    file: "desktop/package.json",
    read: (c) => [JSON.parse(c).version],
    stamps: [topJsonVersion],
  },
  {
    file: "desktop/package-lock.json",
    read: (c) => {
      const lock = JSON.parse(c);
      return [lock.version, lock.packages?.[""]?.version];
    },
    stamps: [topJsonVersion, pkgJsonVersion("wrouter-desktop-tauri")],
  },
];

function applyStamp(content, stamp) {
  const matches = [...content.matchAll(new RegExp(stamp.pattern, "gm"))];
  if (matches.length !== 1) {
    throw new Error(`${stamp.label} 匹配到 ${matches.length} 处（期望恰好 1 处），为防误写已中止`);
  }
  const match = matches[0];
  return content.slice(0, match.index) + match[1] + version + match[2] + content.slice(match.index + match[0].length);
}

let failed = false;
let written = 0;

for (const target of targets) {
  const path = resolve(projectRoot, target.file);
  const content = readFileSync(path, "utf-8");
  const current = target.read(content);
  const mismatched = current.filter((value) => value !== version);

  if (checkOnly) {
    if (mismatched.length > 0) {
      log(`MISMATCH ${target.file}: ${mismatched.join(", ")} != ${version}`);
      failed = true;
    } else {
      log(`ok ${target.file}`);
    }
    continue;
  }

  if (mismatched.length === 0) {
    log(`已同步 ${target.file}`);
    continue;
  }

  let next = content;
  for (const stamp of target.stamps) {
    next = applyStamp(next, stamp);
  }
  // 写回前用同一读取函数复核, 保证戳记与结构化读取一致.
  const verify = target.read(next);
  if (verify.some((value) => value !== version)) {
    throw new Error(`${target.file} 写入后复核失败: ${verify.join(", ")}`);
  }
  writeFileSync(path, next, "utf-8");
  written += 1;
  log(`已更新 ${target.file}: ${mismatched.join(", ")} -> ${version}`);
}

if (checkOnly) {
  if (failed) {
    log("版本不同步: 请运行 `npm run version:sync` 后重试");
    process.exit(1);
  }
  log(`全部同步一致 (v${version})`);
} else {
  log(`完成: ${written} 个文件写入, 版本 v${version}`);
}
