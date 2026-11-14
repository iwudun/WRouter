// R8 (2026-08-01): cross-page number/cost/percent formatters.
//
// Previously formatCompactNumber / formatUsdCost / formatPercent were duplicated
// verbatim in pages/home/shared/usage.ts. They now have a single source here;
// the home barrel re-exports them so downstream consumers are unchanged.

export function formatCompactNumber(value: number, locale?: Intl.LocalesArgument): string {
  return new Intl.NumberFormat(locale, {
    maximumFractionDigits: value >= 1000 ? 1 : 0,
    notation: value >= 10000 ? "compact" : "standard"
  }).format(value);
}

/** 精确整数(千分位分组,不进位):Token 用量详情等不允许约数展示的场景。 */
export function formatExactNumber(value: number, locale?: Intl.LocalesArgument): string {
  return new Intl.NumberFormat(locale, { maximumFractionDigits: 0 }).format(value);
}

export function formatUsdCost(value: number | undefined): string {
  const normalized = Number.isFinite(value) && value && value > 0 ? value : 0;
  if (normalized === 0) {
    return "$0.00";
  }
  if (normalized < 0.01) {
    return `$${normalized.toFixed(6)}`;
  }
  return new Intl.NumberFormat(undefined, {
    currency: "USD",
    maximumFractionDigits: normalized >= 100 ? 0 : 2,
    minimumFractionDigits: normalized >= 100 ? 0 : 2,
    style: "currency"
  }).format(normalized);
}

export function formatPercent(value: number): string {
  return `${Math.round(Math.max(0, Math.min(1, value)) * 100)}%`;
}
