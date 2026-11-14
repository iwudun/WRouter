// Tauri `beforeBuildCommand` 用的前端构建入口(20261102_01 P5-1)。
//
// 为什么不用一行 `npm --prefix ../frontend run build`:
//   Tauri v2 执行 `beforeBuildCommand` 时的 cwd 取决于调用方式(app dir /
//   仓库根),相对路径 `../frontend` 在其中一种 cwd 下会指到错误的位置。
//   本脚本**按自身路径**解析仓库根 → 无论 cwd 在哪都指向同一个 frontend。
//
// 用法: node build-frontend.mjs
// 由 desktop/src-tauri/tauri.conf.json 的 `build.beforeBuildCommand` 调用。

import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
// desktop/ 与 frontend/ 是仓库根下的两个兄弟目录。
const frontendDir = resolve(here, "..", "frontend");
const npm = process.platform === "win32" ? "npm.cmd" : "npm";

console.error(`[build-frontend] frontend dir -> ${frontendDir}`);

const result = spawnSync(npm, ["--prefix", frontendDir, "run", "build"], {
  stdio: "inherit",
  shell: process.platform === "win32",
});

if (result.error) {
  console.error(`[build-frontend] failed to spawn npm: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
