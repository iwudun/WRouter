import { useEffect, useMemo, useState } from "react";
import { Check, ChevronLeft, ChevronRight, ChevronsLeft, ChevronsRight, Copy, Download, Layers } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog, DialogContent, DialogHeader, DialogTitle
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { copyTextToClipboard, formatLogDateTime, useAppText } from "../../shared";
import type { RequestLogTraceItem } from "@/contracts/app";

const PAGE_SIZE_OPTIONS = [10, 20, 30, 50];

/** 流事件查看器:分页 + 逐事件 JSON +
 *  全量复制/下载。数据源是 WRouter 的 request_log_trace 逐事件捕获。 */
export function TraceChunksDialog({ items, onOpenChange, open }: { items: RequestLogTraceItem[]; onOpenChange: (open: boolean) => void; open: boolean }) {
  const t = useAppText();
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(20);
  const [pageInput, setPageInput] = useState("1");
  const [copied, setCopied] = useState(false);

  const totalPages = Math.max(1, Math.ceil(items.length / pageSize));
  const paginated = useMemo(() => items.slice((page - 1) * pageSize, page * pageSize), [items, page, pageSize]);
  const allText = useMemo(() => JSON.stringify(items.map((item) => ({ kind: item.kind, ...(typeof item.payload === "object" && item.payload !== null ? item.payload : { payload: item.payload }) })), null, 2), [items]);

  useEffect(() => {
    if (open) {
      setPage(1);
      setPageInput("1");
    }
  }, [open]);

  function changePage(next: number) {
    const clamped = Math.min(Math.max(1, next), totalPages);
    setPage(clamped);
    setPageInput(String(clamped));
  }

  function commitPageInput() {
    const parsed = parseInt(pageInput, 10);
    if (Number.isFinite(parsed) && parsed >= 1 && parsed <= totalPages) changePage(parsed);
    else setPageInput(String(page));
  }

  function downloadAll() {
    const url = URL.createObjectURL(new Blob([allText], { type: "application/json" }));
    const link = document.createElement("a");
    link.href = url;
    link.download = `trace-events-${Date.now()}.json`;
    link.click();
    URL.revokeObjectURL(url);
  }

  return (
    <Dialog onOpenChange={onOpenChange} open={open}>
      <DialogContent className="flex max-h-[80vh] flex-col sm:max-w-4xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Layers className="h-5 w-5" />
            {t("流事件")}
            <Badge variant="secondary">{items.length} {t("条事件")}</Badge>
            <Button className="h-8 w-8" onClick={() => void copyTextToClipboard(allText).then(() => { setCopied(true); window.setTimeout(() => setCopied(false), 2000); })} size="icon" variant="ghost" title={t("Copy")}>
              {copied ? <Check className="h-4 w-4 text-green-500" /> : <Copy className="h-4 w-4" />}
            </Button>
            <Button className="h-8 w-8" onClick={downloadAll} size="icon" variant="ghost" title={t("Download")}>
              <Download className="h-4 w-4" />
            </Button>
          </DialogTitle>
        </DialogHeader>
        {items.length === 0 ? (
          <div className="flex h-48 items-center justify-center text-body text-muted-foreground">{t("未捕获执行尝试或流事件详情。")}</div>
        ) : (
          <>
            <div className="max-h-[52vh] w-full flex-1 space-y-3 overflow-auto rounded-lg border border-border/60 bg-muted/20 p-4">
              {paginated.map((item, index) => (
                <TraceChunkCard index={(page - 1) * pageSize + index} item={item} key={`${(page - 1) * pageSize + index}-${item.kind}`} />
              ))}
            </div>
            {totalPages > 1 ? (
              <div className="flex items-center justify-between border-t pt-3">
                <div className="flex-1 text-caption text-muted-foreground">
                  {t("显示")} {(page - 1) * pageSize + 1}-{Math.min(page * pageSize, items.length)} / {items.length}
                </div>
                <div className="flex items-center gap-4">
                  <div className="flex items-center gap-2">
                    <span className="text-caption font-medium">{t("每页")}</span>
                    <select
                      className="h-8 rounded-md border border-border bg-background px-2 text-caption"
                      onChange={(event) => { setPageSize(Number(event.target.value)); changePage(1); }}
                      value={pageSize}
                    >
                      {PAGE_SIZE_OPTIONS.map((size) => <option key={size} value={size}>{size}</option>)}
                    </select>
                  </div>
                  <div className="flex items-center gap-2">
                    <Input className="h-8 w-12 text-center" onBlur={commitPageInput} onChange={(event) => setPageInput(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") commitPageInput(); }} value={pageInput} />
                    <span className="text-caption text-muted-foreground">/ {totalPages}</span>
                  </div>
                  <div className="flex items-center gap-1">
                    <Button className="h-8 w-8 p-0" disabled={page === 1} onClick={() => changePage(1)} size="icon" variant="outline"><ChevronsLeft className="h-4 w-4" /></Button>
                    <Button className="h-8 w-8 p-0" disabled={page === 1} onClick={() => changePage(page - 1)} size="icon" variant="outline"><ChevronLeft className="h-4 w-4" /></Button>
                    <Button className="h-8 w-8 p-0" disabled={page === totalPages} onClick={() => changePage(page + 1)} size="icon" variant="outline"><ChevronRight className="h-4 w-4" /></Button>
                    <Button className="h-8 w-8 p-0" disabled={page === totalPages} onClick={() => changePage(totalPages)} size="icon" variant="outline"><ChevronsRight className="h-4 w-4" /></Button>
                  </div>
                </div>
              </div>
            ) : null}
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}

function TraceChunkCard({ index, item }: { index: number; item: RequestLogTraceItem }) {
  const [copied, setCopied] = useState(false);
  const text = useMemo(() => {
    try {
      return JSON.stringify(item.payload, null, 2);
    } catch {
      return String(item.payload);
    }
  }, [item.payload]);
  return (
    <div className="rounded-lg border border-border/60 bg-background/80 p-3">
      <div className="flex items-start gap-3">
        <span className="shrink-0 text-caption font-semibold text-muted-foreground">#{index + 1}</span>
        <div className="min-w-0 flex-1">
          <div className="mb-1 flex flex-wrap items-center gap-2">
            <Badge variant="outline">{item.kind}</Badge>
            {item.createdAt ? <span className="font-mono text-caption text-muted-foreground">{formatLogDateTime(item.createdAt)}</span> : null}
          </div>
          <pre className="max-h-[280px] overflow-auto whitespace-pre-wrap break-words font-mono text-caption leading-relaxed text-muted-foreground">{text}</pre>
        </div>
        <div className="flex shrink-0 gap-1">
          <Button
            className="h-8 w-8"
            onClick={() => void copyTextToClipboard(text).then(() => { setCopied(true); window.setTimeout(() => setCopied(false), 2000); })}
            size="icon"
            variant="ghost"
          >
            {copied ? <Check className="h-4 w-4 text-green-500" /> : <Copy className="h-4 w-4" />}
          </Button>
        </div>
      </div>
    </div>
  );
}
