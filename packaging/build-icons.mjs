/**
 * WRouter 应用图标生成器.
 *
 * 从 frontend/src/assets/logo.png 生成三种安装包图标:
 *   build/icons/icon.ico   — Windows NSIS + exe 图标 (多尺寸 ICO)
 *   build/icons/icon.png   — Linux AppImage + RPM 图标 (512×512 PNG)
 *   build/icons/icon.icns  — macOS 预留 (512×512 ICNS)
 *
 * 20261102_01 P3-1:同时产出 **Tauri 桌面壳**需要的图标集(
 *   desktop/src-tauri/icons/{32x32.png,128x128.png,128x128@2x.png,
 *   icon.ico,icon.icns})—— Tauri 的 `bundle.icon` 用的是这套固定命名,
 *   与 electron-builder 的 `build/icons/icon.*` 不通用。
 *
 * 策略 (按优先级):
 *   1. ImageMagick 的 magick/convert (最可靠, 多尺寸 ICO 原生支持)
 *   2. sharp (npm 包, 按需安装)
 *   3. 纯 Node fallback: 复制原图+无缩放 ICO/ICNS 容器 (最低保底)
 *
 * 幂等: 目标文件已存在且源文件未变则跳过.
 */

import { existsSync, statSync, mkdirSync, copyFileSync, writeFileSync, readFileSync } from "node:fs";
import { spawnSync, execSync } from "node:child_process";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(__dirname, "..");
const srcPng = resolve(projectRoot, "frontend", "src", "assets", "logo.png");
const outDir = resolve(projectRoot, "build", "icons");
const outIco = resolve(outDir, "icon.ico");
const outPng = resolve(outDir, "icon.png");
const outIcns = resolve(outDir, "icon.icns");
// 20261102_01 P3-1:Tauri 桌面壳的图标集(固定命名,与 electron-builder 不通用)。
const tauriIconDir = resolve(projectRoot, "desktop", "src-tauri", "icons");
const tauriPngIcons = [
  { file: "32x32.png", size: 32 },
  { file: "128x128.png", size: 128 },
  { file: "128x128@2x.png", size: 256 },
];

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

function log(msg) {
  console.error(`[build-icons] ${msg}`);
}

function isUpToDate(target, source) {
  if (!existsSync(target)) return false;
  return statSync(target).mtimeMs >= statSync(source).mtimeMs;
}

// ---------------------------------------------------------------------------
// ImageMagick 方案 (最可靠, 多尺寸 ICO 原生支持)
// ---------------------------------------------------------------------------

function tryImageMagick() {
  // 检测 magick (Windows) 或 convert (Linux/macOS)
  const magick = spawnSync("magick", ["--version"], { encoding: "utf-8", timeout: 5000 });
  const exe = magick.status === 0 ? "magick" : null;
  if (!exe) {
    const convert = spawnSync("convert", ["--version"], { encoding: "utf-8", timeout: 5000 });
    if (convert.status !== 0) return false;
  }
  const cmd = exe || "convert";
  log(`using ImageMagick (${cmd})`);

  // PNG 512×512
  const r1 = spawnSync(cmd, [srcPng, "-resize", "512x512", outPng], { timeout: 30000 });
  if (r1.status !== 0) {
    log(`PNG resize failed: ${r1.stderr?.toString() || r1.stdout?.toString()}`);
    return false;
  }

  // ICO 多尺寸 (16/32/48/64/128/256)
  const icoSizes = ["16", "32", "48", "64", "128", "256"];
  // 先生成各尺寸临时 PNG, 再用 convert 合成 ICO
  // 更简单: 直接用 magick 一次生成
  const r2 = spawnSync(cmd, [
    srcPng, "-background", "none",
    "-define", `icon:auto-resize=${icoSizes.join(",")}`,
    outIco,
  ], { timeout: 30000 });
  if (r2.status !== 0) {
    log(`ICO generation failed: ${r2.stderr?.toString() || r2.stdout?.toString()}`);
    // 尝试后备: 单尺寸 ICO
    const r2b = spawnSync(cmd, [srcPng, "-resize", "256x256", outIco], { timeout: 30000 });
    if (r2b.status !== 0) return false;
  }

  // ICNS 512×512
  const r3 = spawnSync(cmd, [srcPng, "-resize", "512x512", outIcns], { timeout: 30000 });
  if (r3.status !== 0) {
    log(`ICNS generation failed (non-fatal, macOS only): ${r3.stderr?.toString()}`);
    // 不返回 false, ICNS 不是必须的
  }

  return true;
}

// ---------------------------------------------------------------------------
// sharp 方案 (npm 包)
// ---------------------------------------------------------------------------

async function trySharp() {
  let sharp;
  try {
    sharp = await import("sharp");
  } catch {
    // 尝试安装
    log("sharp not found, attempting to install...");
    const r = spawnSync("npm", ["install", "--no-save", "sharp"], {
      cwd: projectRoot, timeout: 60000, stdio: "pipe",
    });
    if (r.status !== 0) {
      log("sharp install failed, skipping");
      return false;
    }
    try {
      sharp = await import("sharp");
    } catch {
      return false;
    }
  }

  log("using sharp");

  // PNG 512×512
  await sharp.default(srcPng).resize(512, 512).png().toFile(outPng);

  // ICO: sharp 不支持 ICO 输出, 用 PNG 嵌入 ICO 容器
  const sizes = [16, 32, 48, 64, 128, 256];
  const buffers = [];
  for (const size of sizes) {
    const buf = await sharp.default(srcPng).resize(size, size).png().toBuffer();
    buffers.push({ size, data: buf });
  }
  writeIcoFile(outIco, buffers);

  // ICNS: 用 PNG 嵌入 ICNS 容器
  const icnsBuf = await sharp.default(srcPng).resize(512, 512).png().toBuffer();
  writeIcnsFile(outIcns, icnsBuf);

  return true;
}

// ---------------------------------------------------------------------------
// python+Pillow 方案 (多尺寸 ICO/ICNS, 无 ImageMagick/sharp 时的规范兜底)
// ---------------------------------------------------------------------------

function tryPillow() {
  const pyCheck = spawnSync("python", ["-c", "import PIL"], { encoding: "utf-8", timeout: 5000 });
  if (pyCheck.status !== 0) {
    log("python/PIL not available, skipping");
    return false;
  }
  log("using python+Pillow");
  const script = [
    "from PIL import Image",
    "import sys",
    "src, out = sys.argv[1], sys.argv[2]",
    "img = Image.open(src).convert('RGBA')",
    "img.resize((512,512), Image.LANCZOS).save(out + '/icon.png')",
    "img.save(out + '/icon.ico', sizes=[(16,16),(32,32),(48,48),(64,64),(128,128),(256,256)])",
    "img.resize((512,512), Image.LANCZOS).save(out + '/icon.icns', sizes=[(512,512)])",
    "print('Pillow icons ok')",
  ].join("\n");
  const r = spawnSync("python", ["-c", script, srcPng, outDir], { encoding: "utf-8", timeout: 60000 });
  if (r.status !== 0) {
    log(`Pillow failed: ${r.stderr || r.stdout}`);
    return false;
  }
  log((r.stdout || "").trim());
  return true;
}

// ---------------------------------------------------------------------------
// 纯 Node fallback: 手写 ICO/ICNS 容器
// ---------------------------------------------------------------------------

function writeIcoFile(path, entries) {
  // ICO header: reserved(2) + type(2=ico) + count(2)
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);     // reserved
  header.writeUInt16LE(1, 2);     // type: ICO
  header.writeUInt16LE(entries.length, 4); // count

  const dirEntries = [];
  let dataOffset = 6 + entries.length * 16;
  const dataBuffers = [];

  for (const { size, data } of entries) {
    const dirEntry = Buffer.alloc(16);
    dirEntry.writeUInt8(size >= 256 ? 0 : size, 0);   // width
    dirEntry.writeUInt8(size >= 256 ? 0 : size, 1);   // height
    dirEntry.writeUInt8(0, 2);   // colors
    dirEntry.writeUInt8(0, 3);   // reserved
    dirEntry.writeUInt16LE(1, 4); // planes
    dirEntry.writeUInt16LE(32, 6); // bpp
    dirEntry.writeUInt32LE(data.length, 8); // size
    dirEntry.writeUInt32LE(dataOffset, 12); // offset
    dirEntries.push(dirEntry);
    dataBuffers.push(data);
    dataOffset += data.length;
  }

  const fd = writeFileSync(path, Buffer.concat([header, ...dirEntries, ...dataBuffers]));
  log(`ICO written: ${path} (${entries.length} sizes)`);
}

function writeIcnsFile(path, pngData) {
  // ICNS header: magic(4) + totalSize(4)
  const iconType = "ic07"; // 128x128 PNG
  const iconType2 = "ic08"; // 256x256 PNG
  const iconType3 = "ic09"; // 512x512 PNG

  const chunks = [
    makeIcnsChunk(iconType3, pngData), // 512×512
  ];

  const totalSize = 8 + chunks.reduce((s, c) => s + c.length, 0);
  const header = Buffer.alloc(8);
  header.write("icns", 0, "ascii");
  header.writeUInt32BE(totalSize, 4);

  writeFileSync(path, Buffer.concat([header, ...chunks]));
  log(`ICNS written: ${path}`);
}

function makeIcnsChunk(type, data) {
  const chunk = Buffer.alloc(8 + data.length);
  chunk.write(type, 0, "ascii");
  chunk.writeUInt32BE(8 + data.length, 4);
  data.copy(chunk, 8);
  return chunk;
}

function fallbackMinimal() {
  log("fallback: copying source PNG as icon (no resize)");
  mkdirSync(outDir, { recursive: true });
  copyFileSync(srcPng, outPng);
  // 写一个最小 ICO (256x256 PNG 嵌入)
  const data = readFileSync(srcPng);
  writeIcoFile(outIco, [{ size: 256, data }]);
  writeIcnsFile(outIcns, data);
}

// ---------------------------------------------------------------------------
// Tauri 图标集(20261102_01 P3-1)
// ---------------------------------------------------------------------------

/** 探测 ImageMagick 可执行文件(有则返回路径)。 */
function detectImageMagick() {
  for (const candidate of ["magick", "convert"]) {
    const probe = spawnSync(candidate, ["--version"], { encoding: "utf-8", timeout: 5000 });
    if (probe.status === 0) return candidate;
  }
  return null;
}

/** Python(Pillow)缩放脚本:python resize.py <size> <src> <dest> */
const PYTHON_RESIZE = [
  "import sys",
  "from PIL import Image",
  "size = int(sys.argv[1]); src = sys.argv[2]; dest = sys.argv[3]",
  "im = Image.open(src).convert('RGBA')",
  "im.resize((size, size), Image.LANCZOS).save(dest)",
].join("\n");

/** 把源图缩放到指定边长并写入 dest(依次尝试 ImageMagick / sharp / Pillow)。 */
async function resizePng(size, dest) {
  const im = detectImageMagick();
  if (im) {
    const r = spawnSync(im, [srcPng, "-resize", `${size}x${size}`, dest], { timeout: 30000 });
    if (r.status === 0) return true;
  }
  try {
    const sharp = await import("sharp");
    await sharp.default(srcPng).resize(size, size).png().toFile(dest);
    return true;
  } catch {
    // sharp 不可用 → 继续 Pillow
  }
  const r = spawnSync("python", ["-c", PYTHON_RESIZE, String(size), srcPng, dest], { timeout: 60000 });
  return r.status === 0;
}

/** 产出 Tauri 桌面壳的图标集(固定命名 + 与主产出同源的 ico/icns)。 */
async function writeTauriIcons() {
  mkdirSync(tauriIconDir, { recursive: true });
  for (const { file, size } of tauriPngIcons) {
    const dest = resolve(tauriIconDir, file);
    if (await resizePng(size, dest)) {
      log(`tauri icon ${file} (${size}x${size})`);
    } else {
      // 兜底:复制原图(不缩放)—— 打包图标会偏大,但不阻塞构建。
      copyFileSync(srcPng, dest);
      log(`tauri icon ${file}: fallback (no resize)`);
    }
  }
  if (existsSync(outIco)) {
    copyFileSync(outIco, resolve(tauriIconDir, "icon.ico"));
  }
  if (existsSync(outIcns)) {
    copyFileSync(outIcns, resolve(tauriIconDir, "icon.icns"));
  }
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------

async function main() {
  mkdirSync(outDir, { recursive: true });

  if (!existsSync(srcPng)) {
    log(`ERROR: source logo not found: ${srcPng}`);
    process.exit(1);
  }

  // 幂等检查: 所有目标(含 Tauri 图标集)已生成且最新
  const targets = [
    outIco,
    outPng,
    outIcns,
    ...tauriPngIcons.map(({ file }) => resolve(tauriIconDir, file)),
    resolve(tauriIconDir, "icon.ico"),
    resolve(tauriIconDir, "icon.icns"),
  ];
  if (targets.every((t) => isUpToDate(t, srcPng))) {
    log("all icons are up to date, skipping");
    return;
  }

  // 尝试 ImageMagick
  if (tryImageMagick()) {
    log("icons generated via ImageMagick");
    await writeTauriIcons();
    return;
  }

  // 尝试 sharp
  try {
    if (await trySharp()) {
      log("icons generated via sharp");
      await writeTauriIcons();
      return;
    }
  } catch (e) {
    log(`sharp approach failed: ${e.message}`);
  }

  // 尝试 python+Pillow (多尺寸 ICO/ICNS, 无 IM/sharp 时的规范方案)
  if (tryPillow()) {
    log("icons generated via python+Pillow");
    await writeTauriIcons();
    return;
  }

  // 纯 Node fallback
  fallbackMinimal();
  log("icons generated via fallback (minimal, no resize)");
  await writeTauriIcons();
}

main().catch((e) => {
  log(`FATAL: ${e.message}`);
  process.exit(1);
});