import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

/**
 * Global test setup (R10 UI smoke tests).
 *
 * jsdom does not implement window.matchMedia, but App.tsx calls it directly
 * during initial state (App.tsx:57) and in effects (App.tsx:338,367) — without
 * this stub the app throws on mount. The stub returns matches: false, so
 * prefers-color-scheme / compact-layout queries resolve to the "light/full"
 * defaults, which is fine for smoke assertions.
 */
Object.defineProperty(window, "matchMedia", {
  writable: true,
  value: (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false
  })
});

// jsdom does not implement ResizeObserver; chart views (recharts) construct
// one on mount. A no-op stub keeps the usage-stats view from crashing.
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}
globalThis.ResizeObserver = ResizeObserverStub as unknown as typeof ResizeObserver;

afterEach(() => {
  cleanup();
});
