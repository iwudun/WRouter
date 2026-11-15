// WRouter unified packaging orchestrator.
//
// Builds installers for the requested target(s).
//
// 20261102_01 P5-5:Windows 侧改由 **Tauri v2** 出包(原先 electron-builder),
// Linux 侧仍是 rpmbuild(容器内)。两条链的共同点:
//   - 都不再需要 Electron runtime / 缓存目录;
//   - 都以「单次构建 → build/dist/release-v<版本>/」的方式归档产物。
//
// Usage:
//   node scripts/build.mjs --win                 # Windows NSIS .exe (unsigned)
//   node scripts/build.mjs --win --sign          # Windows .exe with code signing
//   node scripts/build.mjs --rpm                 # Linux .rpm only (via Docker)
//   node scripts/build.mjs --linux               # Linux .rpm (one container run)
//   node scripts/build.mjs --win --linux         # everything
//
// Code signing is OFF by default for faster local builds. Pass --sign to
// enable signtool (requires a valid code-signing certificate on the machine).
//
// Notes:
//   - 20261102_01 P2-5: Linux 只产 RPM (决策 D1: 不产 AppImage)。Linux 交付
//     改为纯后端 (单 musl 静态二进制 + 前端 SPA + systemd 单元), 不再走
//     electron-builder —— 容器内直接 rpmbuild (镜像内无 nfpm)。
//   - 20261102_01 P5-1: Windows 走 scripts/build-desktop.mjs(内部调 tauri CLI,
//     目标固定 x86_64-pc-windows-msvc)。
//   - Linux builds require Docker with the wrouter-linux-builder:v2 image.

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync, statSync } from "node:fs";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { join, resolve } from "node:path";
import { platform } from "node:os";

const projectRoot = resolve(import.meta.dirname ?? ".", "..");
const isWindows = platform() === "win32";

const log = (m) => console.log(`[build] ${m}`);
const fail = (m) => { console.error(`[build] ERROR: ${m}`); process.exit(1); };

// 产物目录名:`release-v<版本>`。版本唯一来源是根 package.json(与 WR_VERSION 同源)。
function readReleaseVersion() {
  return JSON.parse(readFileSync(join(projectRoot, "package.json"), "utf-8")).version;
}

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

// Locate a bash for packaging/build-linux.sh. On Windows the bare `bash` on
// PATH is usually WSL's System32 bash, whose distro typically has no docker
// CLI (Docker Desktop WSL integration is off by default), so the script dies
// with "The command 'docker' could not be found in this WSL 2 distro".
// Git Bash is the intended host: it inherits the Windows PATH, so `docker`
// inside the script resolves to Docker Desktop's Windows CLI. Git only puts
// <GitRoot>\cmd on PATH (git.exe, no bash.exe), so probe explicitly.
function findBash() {
  if (!isWindows) return "bash";
  const override = process.env.WROUTER_BASH;
  if (override) {
    if (existsSync(override)) return override;
    fail(`WROUTER_BASH points to a missing file: ${override}`);
  }
  // Derive from git.exe: <GitRoot>\cmd\git.exe -> <GitRoot>\bin\bash.exe.
  const whereGit = spawnSync("where.exe", ["git"], { encoding: "utf-8" });
  for (const line of (whereGit.stdout || "").split(/\r?\n/)) {
    const gitExe = line.trim();
    if (!gitExe) continue;
    const bash = resolve(gitExe, "..", "..", "bin", "bash.exe");
    if (existsSync(bash)) return bash;
  }
  // Common install locations (covers git.exe not on PATH).
  const candidates = [
    join(process.env.ProgramFiles || "C:\\Program Files", "Git", "bin", "bash.exe"),
    join(process.env["ProgramFiles(x86)"] || "C:\\Program Files (x86)", "Git", "bin", "bash.exe"),
    join(process.env.LOCALAPPDATA || "", "Programs", "Git", "bin", "bash.exe"),
  ];
  for (const c of candidates) {
    if (existsSync(c)) return c;
  }
  log("WARNING: Git Bash not found; falling back to bare `bash` (likely WSL). " +
      "If the build fails on docker, install Git for Windows or set " +
      "WROUTER_BASH to a bash.exe path.");
  return "bash";
}

// Check whether a Docker image exists locally. Uses `image ls --quiet`
// rather than `inspect` because some Docker Desktop versions list an image
// but fail inspect on it.
function imageExists(imageRef) {
  const r = spawnSync("docker", ["image", "ls", imageRef, "--quiet"],
                      { encoding: "utf-8", shell: isWindows });
  return r.status === 0 && (r.stdout || "").trim().length > 0;
}

// Parse target flags. Default to --win on Windows, --linux elsewhere.
const argv = process.argv.slice(2);
const wantWin = argv.includes("--win");
const wantRpm = argv.includes("--rpm");
const wantLinux = argv.includes("--linux");
// 20261102_01 P5-5:代码签名。Tauri 的签名配置在 tauri.conf.json
// (bundle.windows.certificateThumbprint / digestAlgorithm),命令行没有
// `--sign` 对应物 —— 保留这个开关只为兼容旧的调用方式,命中时给出提示。
const SIGN = argv.includes("--sign");
if (SIGN) {
  console.log("[build] NOTE: Tauri 签名走 tauri.conf.json(bundle.windows.certificateThumbprint);--sign 已退化为提示。");
}
const noTarget = !wantWin && !wantRpm && !wantLinux;
const TARGET_WIN = wantWin || (noTarget && isWindows);
const TARGET_LINUX = wantLinux || wantRpm || (noTarget && !isWindows);
// 20261102_01 P2-5: Linux 只产 RPM (决策 D1: AppImage 已下线)。
const LINUX_TARGETS = ["rpm"];

if (!TARGET_WIN && !TARGET_LINUX) {
  fail("no target selected. Use --win, --rpm, or --linux.");
}

// ---------------------------------------------------------------------------
// Pre-flight checks
// ---------------------------------------------------------------------------
function checkEnv() {
  log("pre-flight checks...");
  const checks = [
    ["node_modules/", existsSync(join(projectRoot, "node_modules"))],
    ["frontend/node_modules/", existsSync(join(projectRoot, "frontend", "node_modules"))],
    // Tauri CLI 必须装在 desktop/(app dir)—— 装在仓库根会找不到 src-tauri/。
    ["desktop/node_modules/ (@tauri-apps/cli)", existsSync(join(projectRoot, "desktop", "node_modules"))],
  ];
  let ok = true;
  for (const [label, exists] of checks) {
    console.log(`  ${exists ? "OK" : "MISSING"}  ${label}`);
    if (!exists) ok = false;
  }
  if (!ok) {
    fail("dependencies missing. Run `npm run setup` first.");
  }
  // 后端不再走 PyInstaller(backend/.venv 非必需)。Windows 侧后端**同进程**
  // 跑在 Tauri 壳内(不需要独立二进制);Linux 构建由 build-linux.sh 在
  // 宿主侧调 build-backend.sh 产 musl 静态二进制。
}

// ---------------------------------------------------------------------------
// Windows build (.exe NSIS —— 20261102_01 P5-5 起由 Tauri 产出)
// ---------------------------------------------------------------------------
async function buildWindows(releaseDir) {
  console.log("\n=== Windows Tauri NSIS build ===");
  checkEnv();

  // 产物统一归档到 build/dist/release-v<版本>/,不再按时间戳与平台分目录。
  const outDir = join("build", "dist", releaseDir);
  mkdirSync(join(projectRoot, outDir), { recursive: true });
  log(`output directory: ${outDir}`);

  // 1. icons (build/icons/ + desktop/src-tauri/icons/) + build-info
  log("generating icons + build-info...");
  run("npm", ["run", "build:icons"], { env: process.env });
  run("npm", ["run", "build:info"], { env: process.env });

  // 2. Tauri 打包:内部会先跑 beforeBuildCommand 构建 frontend/dist,
  //    再 `tauri build --target x86_64-pc-windows-msvc` 出 NSIS 安装器。
  //    WR_VERSION 的注入与 NSIS 工具链预置都在 build-desktop.mjs 里。
  log("building desktop installer (tauri build)...");
  run(process.execPath, [resolve(projectRoot, "scripts", "build-desktop.mjs"), "--out", outDir], {
    env: process.env,
  });

  await reportArtifacts(outDir, [".exe"]);
}

// ---------------------------------------------------------------------------
// Linux build (.rpm and/or .AppImage via Docker)
// ---------------------------------------------------------------------------
async function buildLinux(releaseDir, targets) {
  console.log(`\n=== Linux build (${targets.join(" + ")} via Docker) ===`);

  // Docker must be running.
  const dockerCheck = spawnSync("docker", ["info"], { encoding: "utf-8", shell: isWindows });
  if (dockerCheck.status !== 0) {
    fail("Docker daemon is not running. Start Docker Desktop, then re-run.");
  }

  // Pick builder image: env override > cached v2 > auto-build v2 > hard fail.
  // 20261102_01 P2:容器内只跑 rpmbuild(不再需要 electron-builder / GUI 依赖栈);
  // v2 镜像沿用(跨应用共享, 禁止删除/裁剪/重建)。
  const override = process.env.WROUTER_BUILDER_IMAGE;
  let image;
  if (override) {
    image = override;
    log(`builder image: ${image} (WROUTER_BUILDER_IMAGE override)`);
  } else if (imageExists("wrouter-linux-builder:v2")) {
    image = "wrouter-linux-builder:v2";
    log(`builder image: ${image} (v2 cached -- rpmbuild pre-installed)`);
  } else if (imageExists("wrouter-rpm-builder:v1")) {
    log("builder image: wrouter-linux-builder:v2 not cached; building it on first use...");
    log("  (one-time ~3min cold build, then cached; saves ~50s per future build:linux run)");
    const buildArgs = ["build", "-f", "packaging/Dockerfile.linux-builder",
                       "-t", "wrouter-linux-builder:v2", "."];
    const build = spawnSync("docker", buildArgs,
                            { stdio: "inherit", shell: isWindows });
    if (build.status !== 0) {
      fail("Auto-build of wrouter-linux-builder:v2 failed. Fix the Dockerfile or " +
           "set WROUTER_BUILDER_IMAGE=wrouter-rpm-builder:v1 to fall back to v1, then re-run.");
    }
    image = "wrouter-linux-builder:v2";
    log(`builder image: ${image} (just-built)`);
  } else {
    fail("Docker image 'wrouter-rpm-builder:v1' not found. This is the base layer " +
         "required for ALL Linux builds (including the v2 builder auto-build). " +
         "Build it once with: docker build -t wrouter-rpm-builder:v1 . " +
"(see packaging/ for the vendor Dockerfile).");
  }

  // 与 Windows 共用 build/dist/release-v<版本>/,两个平台的产物落在同一目录。
  const outDir = join("build", "dist", releaseDir);
  mkdirSync(join(projectRoot, outDir), { recursive: true });
  log(`output directory: ${outDir}`);

  // build-linux.sh does the whole Linux build in one container run (host-side
  // musl backend + vite frontend, container-side rpmbuild).
  // 把目录名(纯字符串)传给 build-linux.sh,由它用自己的 PROJECT_ROOT 拼路径,
  // 避免 Docker 在 Git Bash 下做 Windows/Unix 路径转换。
  log(`starting container build (targets: ${targets.join(", ")})...`);
  const env = { ...process.env, WROUTER_RELEASE_DIR: releaseDir, WROUTER_BUILDER_IMAGE: image };
  const bash = findBash();
  log(`bash: ${bash}`);
  // shell:false -- the absolute Git Bash path may contain spaces, and cmd.exe
  // quoting would mangle it; spawnSync resolves the .exe directly.
  // cwd 必须显式指到仓库根:脚本路径是相对路径(packaging/build-linux.sh),
  // 继承调用方的 cwd 会在「从别处调用 node scripts/build.mjs」时找不到脚本。
  run(bash, ["packaging/build-linux.sh"], { env, shell: false, cwd: projectRoot });

  return reportArtifacts(outDir, [".rpm"]);
}

// ---------------------------------------------------------------------------
// Report artifacts (path, size, sha256)
// ---------------------------------------------------------------------------
async function reportArtifacts(dir, extensions) {
  const distDir = join(projectRoot, dir);
  if (!existsSync(distDir)) {
    fail(`output directory not found: ${distDir}`);
  }
  const files = readdirSync(distDir)
    .filter((f) => extensions.some((ext) => f.endsWith(ext)))
    .map((f) => join(distDir, f))
    .filter((f) => statSync(f).isFile())
    .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);

  if (files.length === 0) {
    fail(`no ${extensions.join("/")} artifacts found in ${distDir}`);
  }

  console.log("\n========================================");
  console.log("[build] artifacts:");
  for (const f of files) {
    const size = statSync(f).size;
    const sizeMb = (size / 1024 / 1024).toFixed(1);
    const hash = await sha256(f);
    console.log(`  ${f}`);
    console.log(`    size:  ${sizeMb} MB (${size} bytes)`);
    console.log(`    sha256: ${hash}`);
  }
  console.log("========================================");
}

function sha256(filePath) {
  // Use Node's own crypto stream -- no dependency on certutil/shasum locale.
  return new Promise((resolveHash) => {
    const hash = createHash("sha256");
    const stream = createReadStream(filePath);
    stream.on("data", (chunk) => hash.update(chunk));
    stream.on("end", () => resolveHash(hash.digest("hex").toUpperCase()));
    stream.on("error", () => resolveHash("(sha256 unavailable)"));
  });
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------
async function main() {
  // 同一次构建的多个平台共用一个 release 目录。
  const releaseDir = `release-v${readReleaseVersion()}`;
  log(`project: ${projectRoot}`);
  log(`output dir: build/dist/${releaseDir}`);
  log(`targets: ${[
    TARGET_WIN && "win(.exe)",
    TARGET_LINUX && `linux(${LINUX_TARGETS.join("+")})`,
  ].filter(Boolean).join(", ")}`);
  console.log("");

  // 版本不同步直接失败, 提示先跑 version:sync (版本唯一手改处是根 package.json).
  run(process.execPath, [resolve(projectRoot, "scripts", "sync-version.mjs"), "--check"]);

  if (TARGET_WIN) await buildWindows(releaseDir);
  if (TARGET_LINUX) await buildLinux(releaseDir, LINUX_TARGETS);

  console.log("\n[build] all done.");
}

main().catch((err) => {
  console.error("[build] fatal:", err);
  process.exit(1);
});
