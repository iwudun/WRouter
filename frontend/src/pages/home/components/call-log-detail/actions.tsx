import { useState } from "react";
import { Check, Copy, Download } from "lucide-react";

import { Button } from "@/components/ui/button";

import { copyTextToClipboard, useAppText } from "../../shared";

export function TabToolbar({ children }: { children: React.ReactNode }) {
  return <div className="flex shrink-0 flex-wrap items-center gap-2">{children}</div>;
}

export function ModeToggle({ modes, value, onChange }: {
  modes: Array<{ key: string; label: string }>;
  onChange: (key: string) => void;
  value: string;
}) {
  return (
    <div className="flex gap-1 rounded-lg border border-border/60 bg-muted p-1">
      {modes.map((mode) => (
        <button
          className={`rounded-md px-2.5 py-1 text-caption font-semibold transition-colors ${value === mode.key ? "bg-primary text-primary-foreground shadow-sm" : "text-muted-foreground hover:text-foreground"}`}
          key={mode.key}
          onClick={() => onChange(mode.key)}
          type="button"
        >
          {mode.label}
        </button>
      ))}
    </div>
  );
}

export function CopyButton({ getText, label }: { getText: () => string; label: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <Button
      disabled={!getText()}
      onClick={() => void copyTextToClipboard(getText()).then(() => { setCopied(true); window.setTimeout(() => setCopied(false), 1200); })}
      size="sm"
      title={label}
      variant="outline"
    >
      {copied ? <Check className="h-3.5 w-3.5 text-green-500" /> : <Copy className="h-3.5 w-3.5" />}
      {label}
    </Button>
  );
}

export function DownloadButton({ filename, getText }: { filename: string; getText: () => string }) {
  const t = useAppText();
  return (
    <Button
      disabled={!getText()}
      onClick={() => {
        const url = URL.createObjectURL(new Blob([getText()], { type: "application/json" }));
        const link = document.createElement("a");
        link.href = url;
        link.download = filename;
        link.click();
        URL.revokeObjectURL(url);
      }}
      size="sm"
      title={t("Download")}
      variant="outline"
    >
      <Download className="h-3.5 w-3.5" />
      {t("Download")}
    </Button>
  );
}
