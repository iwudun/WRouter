// WRouter one-shot dev environment bootstrap.
//
// Run after a fresh clone. Prepares everything so that
// `npm run dev` works end-to-end:
//   1. root node_modules/        -> npm 工程(20261102_01 P5-5 起根工程已无 Electron 依赖)
//   2. frontend/node_modules/    -> React runtime deps
//   3. desktop/node_modules/     -> @tauri-apps/cli(Tauri v2 桌面壳,P4+
//                                   的 `tauri build` 需要;P3 的 cargo run
//                                   链路不需要)
//   4. backend binary            -> cargo build --release -p wr-cli
//                                   (Rust 是唯一后端形态;dev.mjs 探测
//                                   backend/target/{debug,release}/wr)
//
// Idempotent: safe to re-run; npm installs are left in place unless you pass
// --rebuild (wipes node_modules first); cargo build is incremental.
//
// Usage:
//   node scripts/setup-dev.mjs            # install if missing
//   node scripts/setup-dev.mjs --rebuild  # wipe and reinstall everything
//   node scripts/setup-dev.mjs --no-backend
//   node scripts/setup-dev.mjs --no-frontend

import { spawnSync } from "node:child_process";
import { existsSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";
import { platform } from "node:os";

const projectRoot = resolve(import.meta.dirname ?? ".", "..");
const isWindows = platform() === "win32";
const REBUILD = process.argv.includes("--rebuild");
const SKIP_BACKEND = process.argv.includes("--no-backend");
const SKIP_FRONTEND = process.argv.includes("--no-frontend");

const steps = [];
const log = (m) => console.log(`[setup] ${m}`);
const fail = (m) => { console.error(`[setup] ERROR: ${m}`); process.exit(1); };

function run(cmd, args, opts = {}) {
  const result = spawnSync(cmd, args, {
    stdio: "inherit",
    shell: isWindows,
    ...opts,
  });
  if (result.status !== 0) {
    fail(`command failed (exit ${result.status}): ${cmd} ${args.join(" ")}`);
  }
  return result;
}

function step(name, fn) {
  steps.push({ name, fn });
}

// ---------------------------------------------------------------------------
// Step 1: root node_modules
// ---------------------------------------------------------------------------
step("root dependencies (npm workspace root)", () => {
  const target = join(projectRoot, "node_modules");
  if (REBUILD && existsSync(target)) {
    log(`rebuild: removing ${target}`);
    rmSync(target, { recursive: true, force: true });
  }
  if (existsSync(target)) {
    log("node_modules/ exists, running npm install to sync");
  } else {
    log("installing root dependencies");
  }
  run("npm", ["install"], { cwd: projectRoot });
});

// ---------------------------------------------------------------------------
// Step 2: frontend node_modules
// ---------------------------------------------------------------------------
if (!SKIP_FRONTEND) {
  step("frontend dependencies (React runtime)", () => {
    const feDir = join(projectRoot, "frontend");
    const target = join(feDir, "node_modules");
    if (REBUILD && existsSync(target)) {
      log(`rebuild: removing ${target}`);
      rmSync(target, { recursive: true, force: true });
    }
    if (existsSync(target)) {
      log("frontend/node_modules/ exists, running npm install to sync");
    } else {
      log("installing frontend dependencies");
    }
    run("npm", ["install"], { cwd: feDir });
  });
}

// ---------------------------------------------------------------------------
// Step 3: desktop node_modules (Tauri CLI)
// ---------------------------------------------------------------------------
// 20261102_01 P5 前置:desktop/ 是 Tauri v2 的 app dir(src-tauri/ 在其下),
// @tauri-apps/cli 装在这里才能让 `tauri dev` / `tauri build` 找到配置。
step("desktop dependencies (Tauri CLI)", () => {
  const desktopDir = join(projectRoot, "desktop");
  const target = join(desktopDir, "node_modules");
  if (REBUILD && existsSync(target)) {
    log(`rebuild: removing ${target}`);
    rmSync(target, { recursive: true, force: true });
  }
  if (existsSync(target)) {
    log("desktop/node_modules/ exists, running npm install to sync");
  } else {
    log("installing desktop dependencies (@tauri-apps/cli)");
  }
  run("npm", ["install"], { cwd: desktopDir });
});

// ---------------------------------------------------------------------------
// Step 4: backend binary (Rust)
// ---------------------------------------------------------------------------
if (!SKIP_BACKEND) {
  step("backend binary (cargo build wr; Tauri 壳同进程,此二进制供 wr CLI / Linux 打包用)", () => {
    const beDir = join(projectRoot, "backend");
    const exe = isWindows ? "wr.exe" : "wr";
    const debugBin = join(beDir, "target", "debug", exe);
    const releaseBin = join(beDir, "target", "release", exe);
    if (!REBUILD && (existsSync(debugBin) || existsSync(releaseBin))) {
      log("backend binary exists, running incremental cargo build to sync");
    } else {
      log("building backend binary (cargo build --release -p wr-cli)");
    }
    // Same command dev.mjs recommends when the binary is missing.
    run("cargo", ["build", "--release", "-p", "wr-cli"], { cwd: beDir });
    if (!existsSync(releaseBin) && !existsSync(debugBin)) {
      fail("backend binary not found in backend/target/{release,debug} -- cargo build may have failed");
    }
  });
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------
async function main() {
  log(`project: ${projectRoot}`);
  log(`platform: ${platform()}, rebuild=${REBUILD}, skipBackend=${SKIP_BACKEND}, skipFrontend=${SKIP_FRONTEND}`);
  console.log("");

  const failed = [];
  for (const s of steps) {
    console.log(`\n=== ${s.name} ===`);
    try {
      s.fn();
    } catch (err) {
      failed.push(s.name);
      // run() already called process.exit on non-zero, so this is for safety
      console.error(`[setup] step "${s.name}" threw: ${err}`);
      process.exit(1);
    }
  }

  // Summary
  console.log("\n========================================");
  console.log("[setup] done. Verifying installs...");
  const checks = [
    ["root node_modules", existsSync(join(projectRoot, "node_modules"))],
  ];
  if (!SKIP_FRONTEND) {
    checks.push(["frontend/node_modules", existsSync(join(projectRoot, "frontend", "node_modules"))]);
  }
  checks.push(["desktop/node_modules (Tauri CLI)", existsSync(join(projectRoot, "desktop", "node_modules"))]);
  if (!SKIP_BACKEND) {
    const exe = isWindows ? "wr.exe" : "wr";
    const backendBin = existsSync(join(projectRoot, "backend", "target", "release", exe))
      ? join(projectRoot, "backend", "target", "release", exe)
      : join(projectRoot, "backend", "target", "debug", exe);
    checks.push(["backend binary (wr)", existsSync(backendBin)]);
  }
  let allOk = true;
  for (const [label, ok] of checks) {
    console.log(`  ${ok ? "OK" : "MISSING"}  ${label}`);
    if (!ok) allOk = false;
  }

  if (allOk) {
    console.log("\n[setup] all environments ready. Next:");
    console.log("  npm run dev      # vite dev server + Tauri 桌面壳(后端同进程)");
  } else {
    fail("one or more environments are missing; see messages above");
  }
}

main().catch((err) => {
  console.error("[setup] fatal:", err);
  process.exit(1);
});
