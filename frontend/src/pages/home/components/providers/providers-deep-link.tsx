import { useEffect, useState } from "react";
import { Box } from "lucide-react";
import { cn } from "@/lib/utils";

// Provider icon renderer: shows an explicit icon URL, falling back to a Box
// placeholder when the URL is missing or fails to load. The preset-driven icon
// lookup was removed together with the preset mechanism.
export function ProviderPresetIcon({ className, iconUrl }: { className?: string; iconUrl?: string }) {
  const [failed, setFailed] = useState(false);
  const iconUrlValue = !failed ? iconUrl?.trim() : "";

  useEffect(() => {
    setFailed(false);
  }, [iconUrl]);

  if (iconUrlValue) {
    return (
      <span className={cn("flex shrink-0 items-center justify-center overflow-hidden border border-border bg-background", className)}>
        <img
          alt=""
          className="h-full w-full object-cover"
          draggable={false}
          onError={() => setFailed(true)}
          src={iconUrlValue}
        />
      </span>
    );
  }

  return (
    <span className={cn("flex shrink-0 items-center justify-center border border-border bg-muted text-caption font-semibold text-muted-foreground", className)}>
      <Box className="h-3.5 w-3.5" />
    </span>
  );
}
