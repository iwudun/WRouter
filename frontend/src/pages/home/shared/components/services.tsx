import { Badge } from "@/components/ui/badge";
import type { GatewayStatus } from "@/contracts/app";
import {
  useAppText,
} from "../i18n";


export function endpointFromHostPort(host: string, port: number): string {
  const trimmedHost = host.trim() || "127.0.0.1";
  const endpointHost = trimmedHost === "0.0.0.0" ? "127.0.0.1" : trimmedHost;
  const formattedHost = endpointHost.includes(":") && !endpointHost.startsWith("[") ? `[${endpointHost}]` : endpointHost;
  return `http://${formattedHost}:${port}`;
}

export function StatusBadge({ state }: { state: GatewayStatus["state"] }) {
  const t = useAppText();
  return <Badge variant={state === "running" ? "success" : state === "error" ? "danger" : state === "starting" ? "warning" : "outline"}>{t(state)}</Badge>;
}

export function clampNumber(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}
