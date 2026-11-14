import { useEffect, useState } from "react";
import { Plus, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { Field, FieldLabelHint, useAppText } from "../../shared";

// 方案 20261016_03:供应商「自定义请求头」编辑器(高级配置)。
// 落库键为 `headers` —— 后端 Provider.headers 直接消费,是三级优先级的
// 显式层(可覆盖指纹预设的单个键,如 UA 跟版本);契约里的 extraHeaders
// 是前后端都不消费的死占位,不用。
// 交互:键值行编辑,行失焦/删除行时提交(收敛为 record,空键行剔除,全空
// = undefined 摘键)。即时逐键落库太频,代理下拉的「onChange 即写」范式
// 在自由文本行上不适用。

type HeaderRow = { id: string; key: string; value: string };

let headerRowSeq = 0;
function nextHeaderRowId(): string {
  headerRowSeq += 1;
  return `hdr-${headerRowSeq}`;
}

function rowsFromRecord(headers: Record<string, string> | undefined): HeaderRow[] {
  return Object.entries(headers ?? {}).map(([key, value]) => ({
    id: nextHeaderRowId(),
    key,
    value
  }));
}

function recordFromRows(rows: HeaderRow[]): Record<string, string> | undefined {
  const record: Record<string, string> = {};
  for (const row of rows) {
    const key = row.key.trim();
    if (!key) {
      continue;
    }
    record[key] = row.value;
  }
  return Object.keys(record).length > 0 ? record : undefined;
}

export function ProviderHeadersEditor({
  className,
  headers,
  labelClassName,
  onChange
}: {
  className?: string;
  headers: Record<string, string> | undefined;
  labelClassName?: string;
  onChange: (next: Record<string, string> | undefined) => void;
}) {
  const t = useAppText();
  const [rows, setRows] = useState<HeaderRow[]>(() => rowsFromRecord(headers));

  // provider 切换/外部回写时重置行(镜像详情面板 name/baseUrl 草稿回读)。
  // 以内容为依赖:落库后 provider 对象恒为新引用,按内容比较避免「提交 →
  // 父级写回 → effect 重置行(行 id 换、输入焦点丢)」的自触发。
  const headersKey = JSON.stringify(headers ?? {});
  useEffect(() => {
    setRows(rowsFromRecord(headers));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [headersKey]);

  /** 行失焦/删除时机提交:只有收敛结果与当前配置不同才上抛,避免空转落库。 */
  function commit(nextRows: HeaderRow[]) {
    const next = recordFromRows(nextRows);
    const current = recordFromRows(rowsFromRecord(headers));
    if (JSON.stringify(next) !== JSON.stringify(current)) {
      onChange(next);
    }
  }

  function updateRow(id: string, patch: Partial<Omit<HeaderRow, "id">>) {
    setRows((current) => current.map((row) => (row.id === id ? { ...row, ...patch } : row)));
  }

  function removeRow(id: string) {
    setRows((current) => {
      const next = current.filter((row) => row.id !== id);
      commit(next);
      return next;
    });
  }

  return (
    <div className={cn("grid grid-cols-1 gap-3", className)}>
      <Field
        action={
          <Button
            className="h-7 gap-1 px-2 text-caption"
            onClick={() => setRows((current) => [...current, { id: nextHeaderRowId(), key: "", value: "" }])}
            size="sm"
            type="button"
            variant="outline"
          >
            <Plus className="h-3.5 w-3.5" />
            {t("Add header")}
          </Button>
        }
        label={t("Custom request headers")}
        labelClassName={labelClassName}
        labelHint={<FieldLabelHint>{t("Custom request headers hint")}</FieldLabelHint>}
      >
        {rows.length === 0 ? (
          <div className="flex items-center rounded-md border border-dashed border-border bg-muted/20 px-3 py-2 text-caption text-muted-foreground">
            {t("No custom headers yet.")}
          </div>
        ) : (
          <div className="space-y-1.5">
            {rows.map((row) => (
              <div className="flex items-center gap-1.5" key={row.id}>
                <Input
                  aria-label={t("Header name")}
                  className="h-8 w-[38%] font-mono text-caption"
                  onBlur={() => commit(rows)}
                  onChange={(event) => updateRow(row.id, { key: event.target.value })}
                  placeholder="User-Agent"
                  spellCheck={false}
                  value={row.key}
                />
                <Input
                  aria-label={t("Header value")}
                  className="h-8 min-w-0 flex-1 font-mono text-caption"
                  onBlur={() => commit(rows)}
                  onChange={(event) => updateRow(row.id, { value: event.target.value })}
                  placeholder="value"
                  spellCheck={false}
                  value={row.value}
                />
                <Button
                  aria-label={`${t("Remove")} ${row.key || t("Header name")}`}
                  className="h-8 w-8 shrink-0 p-0 hover:bg-destructive/20 hover:text-destructive"
                  onClick={() => removeRow(row.id)}
                  size="iconSm"
                  type="button"
                  variant="ghost"
                >
                  <Trash2 className="h-3.5 w-3.5" />
                </Button>
              </div>
            ))}
          </div>
        )}
      </Field>
    </div>
  );
}
