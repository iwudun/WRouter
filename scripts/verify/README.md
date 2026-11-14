# scripts/verify — golden 防回归基线工具链

原 `scripts/rebuild/`(20261013_01 D1 改名)。20261016_02 重构:**基线 =
现行 Rust 行为快照**(防回归锚点),不再是 Python parity(后者已于 R7 退役)。

## 依赖

```bash
python -m pip install -r scripts/verify/requirements.txt
```

锁定版本见 [`requirements.txt`](./requirements.txt)(fastapi / uvicorn / httpx / openai / anthropic);这些包只服务回放与 SDK 冒烟,运行时后端不依赖。

## 一条命令

```bash
python scripts/verify/replay_golden.py        # 构建 → mock 上游 → 22 case 回放 → 逐字段 diff
```

- 自动 `cargo build --target-dir target/replay -p wr-cli`(杜绝陈旧二进制),`--skip-build` 跳过;
- 自动探活/拉起 mock 上游(3999),结束时自动清理;
- `--only <case_id>` 单 case 回放;报告落 `testdata/golden/parity-report.json`(已 gitignore);
- **22/22 match、退出码 0** 为通过;
- **端口自动选择**:默认网关/管理面/代理端口是 3460/3462/3463,但本机常被占用或在
  Windows 保留区间(`netsh interface ipv4 show excludedportrange protocol=tcp`)→
  不可用时**自动改用一组空闲端口**并在输出里说明;用 `--gateway-port/--web-port/--proxy-port`
  可手动指定。**没有这个预检时,端口被别人占着会让回放静默打到别人的实例上**
  (表现为满屏 401 之类莫名其妙的 diff,极难定位)。

**使用纪律:改转换层前跑一遍、改完再跑一遍**——红的 case 就是行为变化面,
要么是回归(修代码),要么是有意变更(重采该 case,见下)。

## 22 case 锚点(形状/语义各锁一个,不重复)

| 组 | case | 锚定 |
|---|---|---|
| 形状 | `openai-chat-basic` | 非流 JSON 直通基线 |
| | `openai-chat-stream-text` | SSE 流式基线 |
| | `openai-chat-stream-tool-first-fragment` | 工具参数跨 chunk 分片(C7) |
| | `openai-chat-stream-usage-tail` | 流尾 usage 信封 |
| | `openai-embeddings` | embeddings 协议 |
| | `auth-failed` | 网关认证 401 |
| 转换 | `openai-client-anthropic-upstream` | OpenAI → Anthropic 双向转换 |
| | `anthropic-client-openai-upstream-stream` | Anthropic ← OpenAI 流式事件映射 |
| | `anthropic-native-stream-tool-use` | Anthropic 直通流式 + 工具 |
| | `openai-responses-basic` | Responses → chat 非流转换 |
| | `openai-responses-native-provider` | Responses 直通(502 拒绝语义) |
| | `openai-responses-stream` | Responses 流式 → Anthropic |
| | `gemini-client-openai-upstream` | Gemini → OpenAI 非流转换 |
| | `gemini-native-stream` | Gemini 直通流式 |
| gemini 深水区 | `gemini-interactions-basic` | interactions 新协议 |
| | `thinking-gemini3-thinking-level` | thinkingLevel 分代映射 |
| 语义 | `upstream-401` / `upstream-429` | 凭据失效/限流错误改写 |
| | `upstream-400-no-retry` | 不可重试状态码不重试 |
| | `upstream-503-fallback` | 5xx 触发候选链推进 |
| | `stream-midway-disconnect` | 上游断流 → 不完整流语义 |
| | `thinking-openai-to-anthropic` | reasoning effort 跨协议编码 |

## 2026-11-07 P5 原生基线重录

P3 已声明同协议原生流式改为**原记录透传**(执行器为 SSE 时不再重建帧)。以下四个 case 的旧基线是 P3 前「网关重建帧」形状,于 P5 按 Raw 行为重录并复核:

- `openai-chat-stream-tool-first-fragment`
- `openai-chat-stream-usage-tail`
- `anthropic-native-stream-tool-use`
- `gemini-native-stream`

重录不改变「22 个 case 是防回归锚点、不是覆盖充分证明」的定位；新行为由 `wire_lossless`、`stream_conformance`、`tests/gateway/` 的原生 ByteExact 用例共同支撑。

## 真实 SDK 冒烟(P5 / C14)

```bash
python -m pip install -r scripts/verify/requirements.txt
python scripts/verify/sdk_smoke.py
```

脚本启动本地 `wr-cli` + 回环 mock 上游,用 OpenAI / Anthropic Python SDK 覆盖 **同协议/跨协议 × 非流/流** 八条链路,校验客户端解析的文本、工具调用与终态。本次记录版本:openai 3.46.0、anthropic 2.6.0；不访问公网供应商。

## 上游字节基线(2026-11-05 B-5)

样本不只钉「客户端看到什么」,还钉**「网关发给厂商的字节是什么」**。协议转换的错误
在客户端侧往往看不出来(响应被改写后看着还挺正常),但厂商会拒收或按错误语义计费。

每个 case 目录下多两个文件:

| 文件 | 内容 |
|---|---|
| `upstream_request.json` | 该 case 期间网关**发往上游的请求**清单(按到达顺序):方法 / 路径 / 语义头(`anthropic-version`、`accept`)/ body 原文 / body 的 sha256 / body 字节数 |
| `upstream_request.sha256` | 上一个文件自身的 sha256(`sha256sum` 格式)—— 手改样本却不更新校验会被回放当场抓住 |

回放时三层断言:① `upstream_request.sha256` 与文件实际哈希相符;② 上游请求条数一致
(回退链的 `-a`/`-b` 两个上游按序可比);③ 每条的路径/头/**body 逐字节**与 sha256 都一致。

- **取字节的链路**:mock 上游按**原始字节**记录收到的请求(`GET /__upstream_log/<case_id>` 取回,
  `DELETE` 清空),不解析后序列化 → 字段顺序、空格、转义都留在样本里。
- **`spec_ref`**:每个样本带一个**规格锚点**(该 case 在协议转换矩阵里的位置,
  见 `cases.py` 的 `SPEC_REFS`),相当于 magpie 的「issue 号」—— 本仓库没有 issue
  tracker,锚点就是「客户端协议 × 上游协议 × 面向」那一格。
- **断网**:回放前会校验所有 case 的上游 base_url 必须是回环地址(硬门禁),并给网关子进程
  注入 `HTTP(S)_PROXY` 断网环境变量 + 把 `HOME`/`APPDATA` 指向临时目录。

> **补样本前先人工过一遍基线** —— 样本即真相,当前实现若已有错,补样本等于把错固化。

## 保真比较口径(2026-11-06 P1 固定)

方案 `20261106_01` 把保真分成三级,回放脚本必须说清自己用的是哪一级,**不能把「事件级相同」写成 ByteExact**。

| 等级 | 比较对象 | 当前工具状态 |
|---|---|---|
| **ByteExact** | 未修改的 HTTP 实体正文原始字节(不含 TCP/HTTP chunk、TLS、头大小写) | 请求侧 `upstream_request.json` 已做 body 逐字节 + sha256;响应侧回放**尚未**做原文比较 |
| **StructureExact** | JSON 值/字段存在性/数组顺序/事件身份引用;允许声明过的重序列化 | `replay_golden.py:379-410` 的事件/JSON 比较 + 易变键剔除,属于这一级 |
| **SemanticEquivalent** | 异协议映射契约下的内容、行为、生命周期等价 | 尚无独立 oracle;P3/P4 按协议方向补 |

> 22 个 golden case 是防回归锚点,不是覆盖充分的证明;golden 也不是协议规范。基线本身有错时,先用规范或独立客户端证据确认,再更新期望并记录原因。
>
> P1 无损基座的样本与性质测试在 `backend/crates/wr-domain/tests/wire_lossless.rs` 与 `tests/fixtures/wire/`;它验证分帧/记录层任意 chunk 切分不变量,不代表生产转发已经 ByteExact。

## 脚本与重采

| 脚本 | 用途 |
|---|---|
| `replay_golden.py` | 回放入口(一条命令,见上) |
| `cases.py` | 22 case 定义 + `selector_for`/`explicit_request`(显式 `provider/model` 契约唯一构造点)+ `SPEC_REFS`(规格锚点表) |
| `mock_upstream.py` | Mock 上游(3999):按 case-id 脚本化响应/断流/反射;**并记录网关发来的原始字节** |
| `seed_config.py` | 给回放数据目录种 provider/密钥配置 |
| `sdk_smoke.py` | 真实 OpenAI/Anthropic SDK 冒烟(同协议/跨协议 × 非流/流;依赖见 `requirements.txt`) |
| `_capture_cases.py` | 单/多 case 重采:`python _capture_cases.py <case_id...>`(须先 cargo build);同时写响应样本与**上游字节样本** |

重采即「有意变更后更新基线」:确认行为变化符合预期后
`_capture_cases.py <case_id>` 覆写该 case 样本。删掉的 8 个旧 case 与
Python parity 时代工具在 git 历史(20261016_02 之前)可考。
