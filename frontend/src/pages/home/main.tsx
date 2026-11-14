import React from "react";
import { createRoot } from "react-dom/client";
// F1.1: styles move from the @tailwindcss/cli step into the Vite graph —
// Vite resolves the @import chain (tailwind + fonts) and emits the hashed css
// that index.html links automatically.
import "../../styles/globals.css";
import App from "./App";

// Catch synchronous render errors and log them to console.
window.addEventListener("error", (event) => {
  console.error("[wr:fatal] uncaught error", event.error || event.message);
});
window.addEventListener("unhandledrejection", (event) => {
  console.error("[wr:fatal] unhandled promise rejection", event.reason);
});

/**
 * A minimal React error boundary that renders the error message into the DOM
 * so the user can see what went wrong instead of a blank/black page.
 */
class RenderErrorBoundary extends React.Component<
  { children: React.ReactNode },
  { error: Error | null }
> {
  constructor(props: { children: React.ReactNode }) {
    super(props);
    this.state = { error: null };
  }
  static getDerivedStateFromError(error: Error) {
    return { error };
  }
  componentDidCatch(error: Error, info: React.ErrorInfo) {
    console.error("[wr:fatal] React render error", error, info.componentStack);
  }
  render() {
    if (this.state.error) {
      return (
        <div
          id="wr-error-boundary"
          style={{
            padding: "40px",
            fontFamily: "ui-monospace, monospace",
            color: "#e11d48",
            background: "#fef2f2",
            minHeight: "100vh",
          }}
        >
          <h2 style={{ margin: 0, fontSize: "18px" }}>WRouter 渲染失败</h2>
          <pre style={{ marginTop: "12px", fontSize: "13px", whiteSpace: "pre-wrap" }}>
            {this.state.error.stack || String(this.state.error)}
          </pre>
        </div>
      );
    }
    return this.props.children;
  }
}

const container = document.getElementById("root");

if (!container) {
  throw new Error("Root element not found");
}

try {
  createRoot(container).render(
    <React.StrictMode>
      <RenderErrorBoundary>
        <App />
      </RenderErrorBoundary>
    </React.StrictMode>
  );
} catch (error) {
  console.error("[wr:fatal] initial render failed", error);
  container.innerHTML = `<div style="padding:40px;font-family:sans-serif;color:#e11d48">
    <h2>WRouter 渲染失败</h2>
    <pre style="margin-top:12px;font-size:13px;white-space:pre-wrap">${error instanceof Error ? error.stack : String(error)}</pre>
  </div>`;
}
