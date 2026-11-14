// 模型指纹库状态(方案 20261030_01 F9)。
//
// 指纹库走**独立的管理 RPC**(不是配置 blob):
// - `listModelFingerprints`:用户自建库条目(L2/L3);
// - `enrollModelFingerprint`:采集并入库;
// - `deleteModelFingerprint`:删除用户自建条目;
// - `exportModelFingerprints` / `importModelFingerprint`:导入导出。
//
// 状态刻意**不进全局 config**:指纹库体积随采集增长,塞进配置 blob 会拖慢
// 每次配置读写与导出(方案 §3.5)。
import { useCallback, useEffect, useState } from "react";
import { api, hasBridge } from "@/lib/api";
import type {
  FingerprintEnrollRequest,
  FingerprintEnrollResponse,
  FingerprintExportResponse,
  FingerprintImportResponse,
  FingerprintListResponse
} from "@/contracts/app";

export type FingerprintState = {
  /** 采集 / 删除 / 导入导出进行中。 */
  busy: boolean;
  /** 采集并入库(后端内部已完成 X7 查重,重复时直接抛错)。 */
  enroll: (request: FingerprintEnrollRequest) => Promise<FingerprintEnrollResponse>;
  /** 最近的错误文案(inline 展示)。 */
  error: string;
  /** 导出用户自建库;失败返回 undefined。 */
  exportLibrary: () => Promise<FingerprintExportResponse | undefined>;
  /** 导入指纹库文件;失败返回 undefined。 */
  importLibrary: (json: string) => Promise<FingerprintImportResponse | undefined>;
  /** 库内容(用户自建条目)。 */
  library?: FingerprintListResponse;
  /** 列表加载中。 */
  loading: boolean;
  /** 重新拉取库内容。 */
  refresh: () => Promise<void>;
  /** 删除用户自建条目;内置库条目不可删(后端也不允许)。 */
  removeEntry: (id: string) => Promise<boolean>;
};

export function useFingerprintState(): FingerprintState {
  const [library, setLibrary] = useState<FingerprintListResponse>();
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const refresh = useCallback(async () => {
    if (!hasBridge()) {
      return;
    }
    setLoading(true);
    try {
      setLibrary(await api.listModelFingerprints({}));
      setError("");
    } catch (loadError) {
      setError(loadError instanceof Error ? loadError.message : String(loadError));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const removeEntry = useCallback(
    async (id: string) => {
      setBusy(true);
      try {
        const result = await api.deleteModelFingerprint({ id });
        await refresh();
        setError("");
        return result.ok;
      } catch (deleteError) {
        setError(deleteError instanceof Error ? deleteError.message : String(deleteError));
        return false;
      } finally {
        setBusy(false);
      }
    },
    [refresh]
  );

  const enroll = useCallback(
    async (request: FingerprintEnrollRequest) => {
      setBusy(true);
      try {
        const result = await api.enrollModelFingerprint(request);
        await refresh();
        setError("");
        return result;
      } catch (enrollError) {
        setError(enrollError instanceof Error ? enrollError.message : String(enrollError));
        throw enrollError;
      } finally {
        setBusy(false);
      }
    },
    [refresh]
  );

  const exportLibrary = useCallback(async () => {
    setBusy(true);
    try {
      const result = await api.exportModelFingerprints({});
      setError("");
      return result;
    } catch (exportError) {
      setError(exportError instanceof Error ? exportError.message : String(exportError));
      return undefined;
    } finally {
      setBusy(false);
    }
  }, []);

  const importLibrary = useCallback(
    async (json: string) => {
      setBusy(true);
      try {
        const result = await api.importModelFingerprint({ json });
        await refresh();
        setError("");
        return result;
      } catch (importError) {
        setError(importError instanceof Error ? importError.message : String(importError));
        return undefined;
      } finally {
        setBusy(false);
      }
    },
    [refresh]
  );

  return {
    busy,
    enroll,
    error,
    exportLibrary,
    importLibrary,
    library,
    loading,
    refresh,
    removeEntry
  };
}
