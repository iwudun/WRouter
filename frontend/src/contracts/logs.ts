import type { ProxyCertificateStatus } from "./config";

export type ProxyCertificateInstallResult = {
  caCertFile: string;
  manualCommand?: string;
  message: string;
  ok: boolean;
  status: ProxyCertificateStatus;
};

export type ProxyNetworkBody = {
  contentType?: string;
  decodedFrom?: string;
  encoding: "base64" | "utf8";
  error?: string;
  sizeBytes: number;
  text: string;
  truncated: boolean;
};

export type RequestLogDetailRequest =
  | { id: number }
  | { request_id: string };

export type RequestLogBody = ProxyNetworkBody;

export type RequestLogDetail = {
  request_body_text?: string;
  request_body_size_bytes?: number;
  request_body_truncated?: boolean;
  request_body_capture_status?: "captured" | "truncated" | "unavailable";
  request_headers?: Record<string, string | string[]>;
  response_body_text?: string;
  response_body_size_bytes?: number;
  response_body_truncated?: boolean;
  response_body_capture_status?: "captured" | "truncated" | "unavailable";
  response_headers?: Record<string, string | string[]>;
  /** 20261017_02 批5:详情读取时并回 attempt 侧表数据后投影的尝试表
   *  (与列表条目同形);列表条目的 retryAttempts 已瘦身,详情页以此覆盖。 */
  retryAttempts?: CallLogRetryAttempt[];
};

export type RequestLogTraceItem = {
  createdAt?: string;
  kind: string;
  payload: unknown;
};

export type RequestLogTracePage = {
  items: RequestLogTraceItem[];
  nextOffset?: number;
  total: number;
};

export type RequestLogRetryAttempt = {
  attempt: number;
  delayMs: number;
  final: boolean;
  status?: string;
};

/** 一次转换的诊断(阶段 D3/D4:网关语义转换报告)。 */
export type CallLogConversionDiagnostic = {
  code: string;
  path: string;
  detail: string;
  dropped?: unknown;
};

/** 出站转换报告(E1 起含耗时)。 */
export type CallLogConversion = {
  strategy?: "native_passthrough" | "semantic" | "lossy" | "unsupported" | null;
  diagnostics?: CallLogConversionDiagnostic[];
  /** 请求面转换耗时(微秒;网关测量后回填,仅新行有)。 */
  elapsedUs?: number | null;
};

/** 重试尝试明细(2026-10-08 丰富化;2026-10-12 D3 增逐 attempt 采集)。
 *  旧行(合成数据)只有 attempt/delayMs/final/status(且 status 实为
 *  provider id、delayMs 恒 0);新行由网关落库的结构化明细映射。 */
export type CallLogRetryAttempt = {
  /** 命中的凭据池账号(legacy 单 key 为 null)。 */
  account?: string | null;
  /** 序号,1 起。 */
  attempt: number;
  /** 稳定 attempt id(`{provider}#{ordinal}`;仅新行有)。 */
  attemptId?: string | null;
  /** 出站转换报告(策略 + 诊断;仅语义转换格有)。 */
  conversion?: CallLogConversion | null;
  /** 失败后为下次重试等待的毫秒(最终次为 0)。 */
  delayMs: number;
  /** 本次尝试耗时(毫秒;发起 → 判定,排队拒绝 = 排队等待)。 */
  durationMs?: number | null;
  /** 阶段四 C-4:本次尝试在并发槽位上的等待时长(毫秒;未排队为 0)。 */
  queuedMs?: number | null;
  /** 阶段四 C-4:本次尝试出站时刻相对请求起点的偏移(毫秒;未出站为
   * null —— 熔断跳过 / 排队拒绝)。 */
  sentOffsetMs?: number | null;
  /** 错误摘要(传输错误消息 / 上游错误正文 / 熔断跳过等)。 */
  error?: string | null;
  /** 是否最后一次尝试。 */
  final: boolean;
  /** 实际请求的上游模型(聚合场景与请求名不同)。 */
  model?: string | null;
  /** 尝试的供应商 id。 */
  provider?: string | null;
  /** 上游 HTTP 状态;0 = 未拿到响应(传输错误/熔断跳过/排队拒绝)。
   *  旧行(合成)无此键。 */
  statusCode?: number;
  /** 旧行遗留:合成数据的 status 列(实为 provider id 串)。 */
  status?: string;
  /** 等待来源:上游 Retry-After 头或指数退避。 */
  waitSource?: "retry-after" | "backoff" | null;
  /** 尝试发起时刻(ISO)。 */
  startedAt?: string | null;
  /** 20261017_02 批5:列表行的采集正文已拆侧表按需读取,新行带此标记
   *  = 真有采集(详情读取时并回 upstreamRequest 等键);旧行内嵌正文,
   *  无此键,圆点回落键存在性判断。 */
  captured?: boolean;
  /** 本次尝试实际发出的上游请求体(仅捕获开启;已脱敏、可能截断)。 */
  upstreamRequest?: unknown;
  /** 本次尝试实际发出的上游请求头(仅捕获开启;已过滤敏感头,认证键打码)。 */
  upstreamRequestHeaders?: Record<string, string | string[]> | null;
  /** 本次尝试的上游响应正文(仅捕获开启;已脱敏、可能截断)。 */
  upstreamResponse?: string | null;
  /** 上游响应头(仅捕获开启;已过滤敏感头)。 */
  upstreamHeaders?: Record<string, string | string[]> | null;
  /** 上游响应正文原始字节数(截断前)。 */
  upstreamResponseSize?: number | null;
  /** 上游响应正文是否被截断。 */
  upstreamResponseTruncated?: boolean;
};

export type RequestLogEntry = {
  cacheReadTokens: number;
  cacheWriteTokens: number;
  client: string;
  completedAt?: string;
  costUsd?: number;
  createdAt: string;
  credentialChain: string[];
  credentialId?: string;
  credentialSaturated: boolean;
  durationMs: number;
  error?: string;
  id: number;
  inputTokens: number;
  isStream: boolean;
  method: string;
  model: string;
  ok: boolean;
  outputTokens: number;
  path: string;
  provider: string;
  reasoningTokens: number;
  requestBody: RequestLogBody;
  requestHeaders: Record<string, string | string[]>;
  requestId: string;
  retryAttempts: RequestLogRetryAttempt[];
  responseBody?: RequestLogBody;
  responseHeaders: Record<string, string | string[]>;
  statusCode: number;
  totalTokens: number;
  url: string;
};

export type RequestLogFilterOptions = {
  credentials: string[];
  models: string[];
  providers: string[];
};

// --- Call logs (调用日志 audit view, served by the getCallLogs RPC) ---

/** 五档状态:pending/processing 来自网关在途注册表,
 *  completed/failed/canceled 由落库行按 ok + end_reason 推导。 */
export type CallLogEntryStatus = "pending" | "processing" | "completed" | "failed" | "canceled";

export type CallLogStatusFilter = CallLogEntryStatus | "all";

export type CallLogDecodeSource = "full_flow" | "answer_flow" | "legacy_decode_window";

export type CallLogListFilter = {
  credential?: string;
  model?: string;
  page?: number;
  pageSize?: number;
  query?: string;
  status?: CallLogStatusFilter;
};

export type CallLogEntry = {
  apiKeyId?: string;
  apiKeyName?: string;
  baseModel: string;
  // 响应缓存命中标记(方案 20260905_03 §4.4):仅命中请求携带。
  cacheSource?: string;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  // 压缩明细(方案 20260905_03 §4.3):管线进过才携带(含全 skip 形态)。
  compression?: CallLogCompressionReport;
  conversionChain?: string;
  costSavedUsd?: number;
  costUsd?: number;
  createdAt: string;
  durationMs: number;
  endReason?: string;
  endpointPath: string;
  error?: string;
  firstByteMs?: number;
  /** 阶段四 C-4:出站时刻相对请求起点的偏移(毫秒)。`TTFT − sentMs` 即
   * 供应商侧耗时;缺失 = 没有出站记录(旧行 / 未触达上游)。 */
  sentMs?: number | null;
  /** 阶段四 C-4:并发槽位等待累计(毫秒)。 */
  queuedMs?: number | null;
  /** 阶段四 C-4:失败尝试与其退避的累计(毫秒)。 */
  retriedMs?: number | null;
  /** 阶段四 C-7:内容 10%→90% 的时间跨度(毫秒;实测,非推断)。缺失 =
   * 该行没有可测的内容流(旧行 / 非流式)。 */
  flowMs?: number | null;
  /** 20261108_01 §5.2:答案完成时刻(毫秒,自请求起算;观察到 stop
   *  reason)。缺失 = 旧行 / 没有 Finish 事实。`durationMs` 已冻结在该
   *  时刻,协议排空的拖尾不再计入。 */
  answerEndMs?: number | null;
  /** 20261108_01 §5.2:首个**答案** token 相对请求起点(毫秒;不含思考)。 */
  firstAnswerByteMs?: number | null;
  /** 20261108_01 §5.2:答案内容 10%→90% 跨度(毫秒,×10/8 折算;
   *  吞吐分母)。 */
  answerFlowMs?: number | null;
  /** 20261108_01 §5.2:思考内容 10%→90% 跨度(毫秒;仅诊断)。 */
  reasoningDecodeMs?: number | null;
  /** 20261108_01 §5.2:答案完成到协议终态帧/EOF 的拖尾(毫秒;诊断,
   *  不计入 `durationMs`)。 */
  streamTailMs?: number | null;
  /** 20261108_01 §13:解码速度(可见 token ÷ 解码窗口实测秒;流式成功行
   *  且样本可测才非 null)。列表与用量统计用同一公式,前端不再二次计算。 */
  decodeTps?: number | null;
  /** 20261108_01 §13:输出速度(全部上报输出 token ÷ 请求总耗时;成功行
   *  才非 null)。含首字、思考等待、排队与代理。 */
  outputTps?: number | null;
  /** 解码速度的分母来源;无解码样本时为 null。 */
  decodeSource?: CallLogDecodeSource | null;
  id: number;
  inputTokens: number;
  isStream: boolean;
  method: string;
  modelLabel: string;
  ok: boolean;
  outputTokens: number;
  path: string;
  protocol: string;
  provider?: string;
  /** 20260906_01 §4.7:本次请求实际走的出口(`direct` / `system` /
   *  `preset:<名称>@<host:port>`;凭据已在写入侧掩掉)。历史行无此键。 */
  proxy?: string;
  providerAttempts: string[];
  reasoningTokens: number;
  /** 思考强度(归一枚举词 minimal/low/medium/high/xhigh/none);请求未带
   *  思考参数的行缺省。 */
  reasoningEffort?: string;
  /** 五档状态;在途行(pending/processing)来自网关注册表,非落库行。 */
  status: CallLogEntryStatus;
  requestId: string;
  requestedModel?: string;
  retryAttempts: CallLogRetryAttempt[];
  statusCode: number;
  totalTokens: number;
};

export type CallLogCompressionReport = {
  charsAfter: number;
  charsBefore: number;
  engines: Array<{
    changed: boolean;
    charsAfter: number;
    charsBefore: number;
    id: string;
    skipReason?: string | null;
  }>;
  skippedReason?: string | null;
  tokensSavedEstimate: number;
};

export type CallLogCredentialOption = {
  id: string;
  name: string;
};

export type CallLogFilterOptions = {
  credentials: CallLogCredentialOption[];
  models: string[];
};

export type CallLogPage = {
  generatedAt: string;
  items: CallLogEntry[];
  options: CallLogFilterOptions;
  page: number;
  pageSize: number;
  statusCounts: {
    all: number;
    canceled: number;
    completed: number;
    failed: number;
    pending: number;
    processing: number;
  };
  total: number;
  totalRows: number;
  totalPages: number;
  windowSize: number;
};
