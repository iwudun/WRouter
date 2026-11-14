import { useState } from "react";
import { Check, Copy, CopyX, Terminal } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { copyTextToClipboard, useAppText } from "../../shared";
import { stripStreamField } from "./curl";

/** curl 预览对话框:复制原样 +
 *  复制非流式变体(去掉 stream 字段)。 */
export function CurlPreviewDialog({ curlCommand, onOpenChange, open }: { curlCommand: string; onOpenChange: (open: boolean) => void; open: boolean }) {
  const t = useAppText();
  const [copied, setCopied] = useState(false);
  const [nonStreamCopied, setNonStreamCopied] = useState(false);

  async function copy(value: string, mark: (next: boolean) => void) {
    await copyTextToClipboard(value);
    mark(true);
    window.setTimeout(() => mark(false), 2000);
  }

  return (
    <Dialog onOpenChange={onOpenChange} open={open}>
      <DialogContent className="flex max-h-[80vh] flex-col sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Terminal className="h-5 w-5" />
            {t("cURL 重放")}
            <Button className="h-8 w-8" onClick={() => void copy(curlCommand, setCopied)} size="icon" title={t("Copy")} variant="ghost">
              {copied ? <Check className="h-4 w-4 text-green-500" /> : <Copy className="h-4 w-4" />}
            </Button>
            <Button className="h-8 w-8" onClick={() => void copy(stripStreamField(curlCommand), setNonStreamCopied)} size="icon" title={t("复制非流式变体")} variant="ghost">
              {nonStreamCopied ? <Check className="h-4 w-4 text-green-500" /> : <CopyX className="h-4 w-4" />}
            </Button>
          </DialogTitle>
        </DialogHeader>
        <div className="flex-1 overflow-auto rounded-lg border border-border/60 bg-muted/30 p-4">
          <pre className="whitespace-pre-wrap break-all font-mono text-body">{curlCommand}</pre>
        </div>
      </DialogContent>
    </Dialog>
  );
}
