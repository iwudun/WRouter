// WRouter frontend Vite configuration (方案 #14 F1.1 — esbuild.config.mjs 退役).
//
// Bundles:
//   - Renderer: React app (src/pages/home/main.tsx) -> dist/assets/main-*.js (esm)
//   - Web client bridge: window.wr HTTP fallback -> dist/assets/web-client-bridge-*.js
//     (declared as a second module entry in src/pages/home/index.html; module
//     scripts execute in document order, so window.wr exists before the app
//     bundle's entry chunk runs — same contract as the retired esbuild IIFE)
//   - CSS: Tailwind v4 via @tailwindcss/vite (replaces the @tailwindcss/cli step)
//
// The desktop shell serves this bundle over http (Tauri: the backend's static
// host on 127.0.0.1:3462/, decision D5; Linux RPM: the same SPA from the
// systemd service), therefore:
//   - base stays "./" so the same bundle also works from a sub-path;
//   - Vite's `crossorigin` attributes are stripped: they only ever mattered for
//     the retired Electron shell's file:// loading (no origin -> black screen);
//   - the modulePreload polyfill is off (single-page local app, no preload
//     graph worth the extra request).
//
// Path aliases mirror tsconfig.json (@/* -> ./src/*; the historical
// "@wr/core/*" alias was unified into "@" in 20261013_01 E3).

import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = fileURLToPath(new URL(".", import.meta.url));
const srcDir = join(projectRoot, "src");

/**
 * Dev server port for `npm run dev` (scripts/dev.mjs points the Tauri shell at it).
 * Must stay outside Windows/Hyper-V (WinNAT) reserved port ranges — they drift
 * after reboot; scripts/dev.mjs probes candidates and picks the first free one.
 */
export const VITE_DEV_PORT = 5280;

/**
 * Strip `crossorigin` attributes from emitted HTML tags.
 *
 * Vite adds crossorigin to module scripts/preloads. The attribute applied
 * CORS semantics to the retired Electron shell's file:// origin (no origin ->
 * black screen). Same-origin http loading never needed it — the retired esbuild
 * pipeline emitted none. Kept as-is: harmless under http:// and still protects
 * anyone serving the dist over file://.
 */
function stripCrossOrigin(): Plugin {
  return {
    name: "wr-strip-crossorigin",
    transformIndexHtml(html) {
      return html.replaceAll(" crossorigin", "");
    }
  };
}

/**
 * Dev-only: drop the CSP meta tag from the served index.html.
 *
 * The source template carries a production CSP (`script-src 'self'`, no
 * unsafe-inline). Vite dev injects inline scripts (react-refresh preamble,
 * @vite/client bootstrap) that the CSP would block, killing HMR. Production
 * builds keep the tag untouched.
 */
function dropCspInDev(): Plugin {
  return {
    name: "wr-drop-csp-dev",
    apply: "serve",
    transformIndexHtml(html) {
      return html.replace(/<meta\s+http-equiv="Content-Security-Policy"[\s\S]*?\/>/, "<!-- CSP disabled in dev (vite HMR inline scripts) -->");
    }
  };
}

/**
 * Build-only: inject font preload links into dist/index.html.
 *
 * Fonts are emitted as hashed assets from CSS url() references, so the source
 * template cannot preload them by name. After the bundle is written, parse the
 * emitted CSS @font-face blocks and preload: Inter latin (covers all ASCII UI
 * text) plus the MiSans slice whose unicode-range covers the CJK base block
 * U+4E00 (most common Chinese glyphs) for both weights.
 */
function fontPreload(): Plugin {
  const pickPreloads = (): string[] => {
    const assetsDir = join(projectRoot, "dist", "assets");
    if (!existsSync(assetsDir)) {
      return [];
    }
    const cssFiles = readdirSync(assetsDir).filter((name) => name.endsWith(".css"));
    const blocks: Array<{ url: string; unicodeRange: string }> = [];
    for (const file of cssFiles) {
      const css = readFileSync(join(assetsDir, file), "utf8");
      for (const block of css.match(/@font-face\{[^}]*\}/g) ?? []) {
        const url = block.match(/url\(([^)]+\.woff2)\)/)?.[1];
        const unicodeRange = block.match(/unicode-range:([^;}]+)/)?.[1] ?? "";
        if (url) {
          blocks.push({ url, unicodeRange });
        }
      }
    }
    const picked: Array<{ url: string }> = [];
    const interLatin = blocks.find((block) => block.url.includes("inter-latin-wght-normal"));
    if (interLatin) {
      picked.push(interLatin);
    }
    for (const weight of ["MiSans-Regular", "MiSans-Semibold"]) {
      const cjk = blocks.find((block) => block.url.includes(weight) && /4e00/i.test(block.unicodeRange));
      if (cjk) {
        picked.push(cjk);
      }
    }
    // CSS url() is written relative to the CSS file inside assets/, so the
    // raw url is the bare file name; resolve it against the assets dir.
    return picked.map((item) => `./assets/${item.url.replace(/^\.\//, "")}`);
  };

  return {
    name: "wr-font-preload",
    apply: "build",
    closeBundle() {
      const urls = pickPreloads();
      if (urls.length === 0) {
        return;
      }
      const indexPath = join(projectRoot, "dist", "index.html");
      if (!existsSync(indexPath)) {
        return;
      }
      const html = readFileSync(indexPath, "utf8");
      const links = urls
        .map((url) => `  <link rel="preload" as="font" type="font/woff2" href="${url}" />`)
        .join("\n");
      writeFileSync(indexPath, html.replace("</head>", `${links}\n  </head>`), "utf8");
    }
  };
}

export default defineConfig({
  base: "./",
  plugins: [react(), tailwindcss(), stripCrossOrigin(), dropCspInDev(), fontPreload()],
  resolve: {
    alias: [{ find: "@", replacement: srcDir }]
  },
  // F1.2: dev server consumed by scripts/dev.mjs (Electron loads this URL in
  // dev mode). /api is proxied to the management backend so the pure-browser
  // (web-client-bridge) flow works without Electron.
  server: {
    port: VITE_DEV_PORT,
    strictPort: true,
    proxy: {
      "/api": {
        target: "http://127.0.0.1:3462",
        changeOrigin: true
      }
    }
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
    target: "chrome120",
    chunkSizeWarningLimit: 1600,
    modulePreload: { polyfill: false },
    rollupOptions: {
      // F1.4/F1.5: rolldown default chunking. Explicit advancedChunks groups
      // were tried and reverted: default chunking mis-places a tiny cn()-helper
      // module (clsx/twMerge family) into the recharts group chunk, creating a
      // bogus static edge shared->recharts that pulled recharts (~370KB) into
      // the first-screen graph. With views + view-dir dialogs loaded only via
      // React.lazy (source-level fix), the default chunker already yields the
      // desired shape: lazy per-view chunks + a dynamic recharts chunk.
      output: {}
    }
  }
});
