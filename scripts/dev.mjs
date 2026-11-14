// WRouter dev orchestrator (20261102_01 P5-5: Electron 壳退场 → 只剩 Tauri 一条链)。
//
// 跑两件事并把输出带前缀转发:
//   [fe]    vite dev server (F1.2)  -> http://127.0.0.1:<port> 带 HMR
//   [tauri] cargo run (desktop/src-tauri) -> Tauri 桌面壳,**后端同进程**
//           (P3 起不再 spawn `wr-backend` 子进程)
//
// Tauri 壳在 dev 形态下加载 vite 的 dev URL(WROUTER_VITE_DEV_URL),
// 而不是打包后的 frontend/dist。
//
// Env overrides (all optional):
//   WROUTER_WEB_PORT   web/api port (default 3462)
//   WROUTER_*_PORT     other backend ports (see desktop/src-tauri/src/backend.rs)
//   WROUTER_VITE_PORT  vite dev server port (default: probe 5280-5299, first free)
//
// 版本号:本脚本自动从根 package.json 读 version 并以 WR_VERSION 注入给
// cargo(编译期常量 option_env! 的唯一注入方式)—— 不要手改壳里的版本。
//
// Usage: node scripts/dev.mjs            (watch + launch)
//        node scripts/dev.mjs --no-app   (only the vite dev server)

import { spawn, spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import net from "node:net";
import { join } from "node:path";

const projectRoot = process.cwd();
// vite dev server port。5180/5199 先后被 WinNAT(Hyper-V) 动态保留段吞掉，
// 且保留段每次重启都会漂移 —— 所以不再依赖单个硬编码端口：先探测候选
// 列表(双栈 listen,镜像 vite 对 localhost 的绑定)，取第一个可用者。
// 候选段留 20 个:开发机常驻 QQ/输入法等占用 5283~5285,旧的 vite 残留
// 也会占住前几个;范围太窄会频繁出现「全部候选被占」。
// ⚠️ 改这个范围必须同步 desktop/src-tauri/capabilities/default.json 的
// remote.urls(否则新端口上的页面调不动 window.wr)。
// WROUTER_VITE_PORT 显式设置时固定用该端口(不回退，失败即报错)。
const VITE_PORT_START = 5280;
const VITE_PORT_COUNT = 20;
const VITE_PORT_CANDIDATES = Array.from(
  { length: VITE_PORT_COUNT },
  (_, index) => VITE_PORT_START + index
);

function canListenOn(host, port) {
  return new Promise((resolve) => {
    const server = net.createServer();
    server.once("error", () => resolve(false));
    server.listen({ port, host }, () => server.close(() => resolve(true)));
  });
}

async function pickVitePort() {
  const pinned = Number(process.env.WROUTER_VITE_PORT || 0);
  if (pinned) return pinned;
  for (const port of VITE_PORT_CANDIDATES) {
    if ((await canListenOn("::1", port)) && (await canListenOn("127.0.0.1", port))) {
      return port;
    }
  }
  console.error(
    `[dev] no free vite port among ${VITE_PORT_START}-${VITE_PORT_START + VITE_PORT_COUNT - 1}`
  );
  console.error("[dev] close stale vite/node processes, or set WROUTER_VITE_PORT to a free port");
  console.error("[dev] check reserved ranges: netsh interface ipv6 show excludedportrange protocol=tcp");
  process.exit(1);
}

// 版本唯一来源:根 package.json 的 "version"(唯一手改处)。
// 非法版本直接停 —— 宁可起不来,也不要让壳里跑一个说不清版本的产物。
function readProjectVersion() {
  const pkg = JSON.parse(readFileSync(join(projectRoot, "package.json"), "utf-8"));
  if (typeof pkg.version !== "string" || !/^\d+\.\d+\.\d+(-[\w.]+)?$/.test(pkg.version)) {
    console.error(`[dev] FATAL: 根 package.json 的 version 非法: ${JSON.stringify(pkg.version)}`);
    process.exit(1);
  }
  return pkg.version;
}

// 所有子进程共用一套退出清理。Windows 上 `child.kill()` 只杀 cmd.exe
// 包装层，npx 底下真正的 vite/node 会变成孤儿（5281/5282 残留就是这样
// 来的）→ Windows 必须 taskkill /T 杀整棵树；POSIX 用进程组信号。
const children = new Set();

function killTree(child) {
  if (!child?.pid || child.exitCode !== null || child.signalCode !== null) {
    return;
  }
  if (process.platform === "win32") {
    spawnSync("taskkill", ["/PID", String(child.pid), "/T", "/F"], {
      stdio: "ignore",
      windowsHide: true
    });
    return;
  }
  try {
    process.kill(-child.pid, "SIGTERM");
  } catch {
    try {
      child.kill("SIGTERM");
    } catch {
      // already gone
    }
  }
}

for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.once(signal, () => {
    console.log(`[dev] ${signal}: stopping child processes...`);
    process.exit(0);
  });
}
if (process.platform === "win32") {
  process.once("SIGBREAK", () => {
    console.log("[dev] SIGBREAK: stopping child processes...");
    process.exit(0);
  });
}
// process.exit() 会同步走到这里；必须用同步的 taskkill/kill，
// 不能留 setTimeout/异步清理，否则父进程先退、子进程照样成孤儿。
process.once("exit", () => {
  for (const child of children) {
    killTree(child);
  }
});

function spawnTagged(tag, command, args, opts = {}) {
  const child = spawn(command, args, {
    stdio: ["ignore", "pipe", "pipe"],
    shell: process.platform === "win32",
    // POSIX 单独建进程组，退出时可整组回收（npx -> vite）。
    detached: process.platform !== "win32",
    ...opts,
  });
  children.add(child);
  const prefix = `[${tag}] `;
  child.stdout?.on("data", (chunk) => process.stdout.write(chunk.toString().split("\n").map((l) => l ? prefix + l : l).join("\n")));
  child.stderr?.on("data", (chunk) => process.stderr.write(chunk.toString().split("\n").map((l) => l ? prefix + l : l).join("\n")));
  child.on("exit", (code, signal) => {
    children.delete(child);
    console.log(`${prefix}exited code=${code} signal=${signal || "none"}`);
  });
  child.on("error", (error) => {
    children.delete(child);
    console.error(`${prefix}spawn failed: ${error.message}`);
    process.exit(1);
  });
  return child;
}

async function main() {
  const launchApp = !process.argv.includes("--no-app");

  const vitePort = await pickVitePort();
  console.log(`[dev] vite dev server port -> ${vitePort}`);
  console.log("[dev] starting vite dev server...");
  const fe = spawnTagged("fe", "npx", ["vite", "--port", String(vitePort), "--strictPort"], {
    cwd: join(projectRoot, "frontend")
  });

  const waitVite = async () => {
    for (let attempt = 0; attempt < 120; attempt += 1) {
      try {
        const response = await fetch(`http://localhost:${vitePort}/`);
        if (response.ok) {
          console.log(`[dev] vite dev server ready at http://127.0.0.1:${vitePort}/`);
          return;
        }
      } catch {
        // not up yet
      }
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
    console.error("[dev] timed out waiting for vite dev server");
  };

  // Tauri 链路:后端**同进程**跑在壳里(desktop/src-tauri),不再 spawn
  // `wr-backend` 子进程 → 不需要探测后端二进制。
  console.log("[dev] tauri mode: backend runs in-process (no wr-backend child process)");
  if (!launchApp) {
    console.log("[dev] --no-app: only the vite dev server");
    return;
  }
  await waitVite();
  // WR_VERSION 是**编译期**常量(option_env!):编译 wr-server 时没注入,
  // wr_server::VERSION 就退化成 "0.0.0-dev" —— 设置页「当前版本」显示的
  // 是错的(与 packaging/build-rust.sh、scripts/build-desktop.mjs 同一注入点)。
  // cargo 把 option_env! 记成 env-dep(target/**/wr_server-*.d 里的
  // `# env-dep:WR_VERSION`),版本值一变就自动重编 wr-server,无需 cargo clean。
  const version = readProjectVersion();
  console.log(`[dev] WR_VERSION -> ${version}`);
  const env = {
    ...process.env,
    WR_VERSION: version,
    WROUTER_VITE_DEV_URL: `http://localhost:${vitePort}/`
  };
  const tauri = spawnTagged(
    "tauri",
    "cargo",
    ["run", "--manifest-path", join(projectRoot, "desktop", "src-tauri", "Cargo.toml")],
    { cwd: join(projectRoot, "desktop", "src-tauri"), env }
  );
  tauri.on("exit", () => {
    console.log("[dev] tauri exited, stopping vite dev server...");
    // 统一走 exit 钩子的 taskkill /T:Windows 上 fe.kill() 只杀 cmd 包装层,
    // 会把 npx 底下的 node/vite 留成孤儿。
    process.exit(0);
  });
}

main().catch((err) => {
  console.error("[dev] fatal:", err);
  process.exit(1);
});
