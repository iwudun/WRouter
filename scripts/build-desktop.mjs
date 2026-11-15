// WRouter Windows 桌面安装包构建(20261102_01 P5-1 / P5-5)。
//
// P5-5 起 Windows 交付物由 **Tauri v2** 产出(原先是 electron-builder NSIS)。
// 本脚本是 `npm run tauri:build` / `scripts/build.mjs --win` 的落地实现:
//
//   1. 预置 NSIS 工具链(缺失时调 packaging/fetch-tauri-nsis.ps1)
//   2. 注入 WR_VERSION(根 package.json 的版本 → 编译期常量 wr_server::VERSION)
//   3. tauri build --target x86_64-pc-windows-msvc
//      (beforeBuildCommand 会先构建 frontend/dist)
//   4. 把 NSIS 安装器拷到输出目录并打印体积 / sha256
//
// 用法:
//   node scripts/build-desktop.mjs                # 打包到 build/dist/release-v<版本>
//   node scripts/build-desktop.mjs --out <dir>    # 指定输出目录
//   node scripts/build-desktop.mjs --no-bundle    # 只出 exe,不打 NSIS 安装包

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream, existsSync, mkdirSync, readdirSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { platform } from "node:os";

const projectRoot = resolve(import.meta.dirname ?? ".", "..");
const isWindows = platform() === "win32";
const TARGET = "x86_64-pc-windows-msvc";

const log = (m) => console.log(`[build-desktop] ${m}`);
const fail = (m) => { console.error(`[build-desktop] ERROR: ${m}`); process.exit(1); };

function run(cmd, args, opts = {}) {
  const result = spawnSync(cmd, args, { stdio: "inherit", shell: isWindows, ...opts });
  if (result.status !== 0) {
    fail(`command failed (exit ${result.status}): ${cmd} ${args.join(" ")}`);
  }
  return result;
}

async function sha256(filePath) {
  return new Promise((resolveHash) => {
    const hash = createHash("sha256");
    const stream = createReadStream(filePath);
    stream.on("data", (chunk) => hash.update(chunk));
    stream.on("end", () => resolveHash(hash.digest("hex").toUpperCase()));
    stream.on("error", () => resolveHash("(sha256 unavailable)"));
  });
}

// ---------------------------------------------------------------------------
// 1. NSIS 工具链
// ---------------------------------------------------------------------------
function ensureNsisToolchain() {
  if (!isWindows) return;
  const nsisExe = join(process.env.LOCALAPPDATA || "", "tauri", "NSIS", "makensis.exe");
  if (existsSync(nsisExe)) {
    log(`NSIS toolchain present: ${nsisExe}`);
    return;
  }
  log("NSIS toolchain missing -> running packaging/fetch-tauri-nsis.ps1 ...");
  run("powershell.exe", [
    "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
    join(projectRoot, "packaging", "fetch-tauri-nsis.ps1"),
  ]);
  if (!existsSync(nsisExe)) {
    fail(`NSIS toolchain still missing after fetch: ${nsisExe}`);
  }
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------
const argv = process.argv.slice(2);
const noBundle = argv.includes("--no-bundle");
const outIdx = argv.indexOf("--out");
const outDirArg = outIdx >= 0 ? argv[outIdx + 1] : null;

async function main() {
  const version = JSON.parse(
    await import("node:fs").then((fs) => fs.readFileSync(join(projectRoot, "package.json"), "utf-8")),
  ).version;
  log(`version: ${version}`);
  log(`target: ${TARGET}`);

  const desktopDir = join(projectRoot, "desktop");
  const cliEntry = join(desktopDir, "node_modules", "@tauri-apps", "cli", "tauri.js");
  if (!existsSync(cliEntry)) {
    fail(`Tauri CLI not found: ${cliEntry}\n先跑 npm run setup(或 cd desktop && npm install)`);
  }

  ensureNsisToolchain();

  const args = [cliEntry, "build", "--target", TARGET];
  if (noBundle) args.push("--no-bundle");

  // WR_VERSION 是**编译期**常量(option_env!):不注入的话
  // wr_server::VERSION 会退化成 "0.0.0-dev",设置页显示的版本号就是错的。
  const env = { ...process.env, WR_VERSION: version };
  log("running: tauri build --target " + TARGET + (noBundle ? " --no-bundle" : ""));
  run(process.execPath, args, { cwd: desktopDir, env });

  const outDir = resolve(projectRoot, outDirArg || join("build", "dist", `release-v${version}`));
  mkdirSync(outDir, { recursive: true });

  const bundleNsisDir = join(
    projectRoot, "desktop", "src-tauri", "target", TARGET, "release", "bundle", "nsis",
  );
  const artifacts = [];
  if (!noBundle) {
    if (!existsSync(bundleNsisDir)) {
      fail(`NSIS bundle dir not found: ${bundleNsisDir}`);
    }
    for (const file of readdirSync(bundleNsisDir)) {
      if (!file.endsWith(".exe")) continue;
      const src = join(bundleNsisDir, file);
      const dest = join(outDir, file);
      spawnSync("cmd", ["/c", "copy", "/Y", src, dest], { stdio: "inherit", shell: false });
      artifacts.push(dest);
    }
  }
  // 裸 exe 也拷一份(便于免安装直接跑)。
  const rawExe = join(projectRoot, "desktop", "src-tauri", "target", TARGET, "release", "wrouter-desktop.exe");
  if (existsSync(rawExe)) {
    const dest = join(outDir, "wrouter-desktop.exe");
    spawnSync("cmd", ["/c", "copy", "/Y", rawExe, dest], { stdio: "inherit", shell: false });
  }

  if (artifacts.length === 0) {
    fail(`no installer artifact produced in ${bundleNsisDir}`);
  }

  console.log("\n========================================");
  console.log("[build-desktop] artifacts:");
  for (const file of artifacts) {
    const size = statSync(file).size;
    console.log(`  ${file}`);
    console.log(`    size:  ${(size / 1024 / 1024).toFixed(2)} MB (${size} bytes)`);
    console.log(`    sha256: ${await sha256(file)}`);
  }
  console.log("========================================");
}

main().catch((err) => {
  console.error("[build-desktop] fatal:", err);
  process.exit(1);
});
