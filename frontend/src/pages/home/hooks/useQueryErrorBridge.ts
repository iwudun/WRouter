import { useEffect, useRef } from "react";

import { setQueryErrorBridge } from "@/lib/queryClient";

/** F2a.1: QueryCache 级错误桥接到 action error(经 formatError 归一)。 */
export function useQueryErrorBridge(
  formatError: (error: unknown) => string,
  setActionError: (message: string) => void
) {
  const formatErrorRef = useRef(formatError);
  formatErrorRef.current = formatError;
  useEffect(() => {
    setQueryErrorBridge((error) => setActionError(formatErrorRef.current(error)));
    return () => setQueryErrorBridge(null);
  }, []);
}
