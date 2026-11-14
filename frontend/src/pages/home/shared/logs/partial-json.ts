export function parseLogJson(value: string): unknown | undefined {
  try {
    return JSON.parse(value) as unknown;
  } catch {
    return undefined;
  }
}

// ---------------------------------------------------------------------------
// 截断 JSON 的部分树恢复
// ---------------------------------------------------------------------------

type PartialResult =
  | { done: true; value: unknown }
  | { done: false; cause: "eof" | "syntax" };

/** 捕获截断的 JSON 的「部分树恢复」:递归下降解析,文本尽头只可能来自
 *  截断 —— 丢弃截断处未完成的标量/键值对,按已收集条目闭合容器;
 *  任何语法错误(非截断的畸形文本)整体放弃返回 undefined。
 *  仅接受 `{` / `[` 开头的文本,纯文本/垃圾输入不产出垃圾树。 */
export function parsePartialJson(value: string): unknown | undefined {
  const parser = new PartialJsonParser(value);
  const result = parser.parseRoot();
  return result.done ? result.value : undefined;
}

class PartialJsonParser {
  private pos = 0;
  /** 任一层在文本尽头截断(eof)时置位:截断恢复的合法性凭证。
   *  区分「截断文本」与「完整 JSON 后跟垃圾尾巴」—— 前者接受,
   *  后者整体拒绝(不是单个 JSON 文档)。 */
  private hitEof = false;

  constructor(private readonly text: string) {}

  parseRoot(): PartialResult {
    this.skipWs();
    const first = this.peek();
    if (first !== "{" && first !== "[") {
      return { done: false, cause: "syntax" };
    }
    const result = this.parseValue();
    if (!result.done) {
      return result;
    }
    if (this.hitEof) {
      return result;
    }
    // 完整解析(无截断)时根值之后只允许空白:还有残余说明这不是
    // 单个 JSON 文档。
    this.skipWs();
    if (this.pos < this.text.length) {
      return { done: false, cause: "syntax" };
    }
    return result;
  }

  private parseValue(): PartialResult {
    const ch = this.peek();
    if (ch === undefined) {
      this.hitEof = true;
      return { done: false, cause: "eof" };
    }
    if (ch === "{") return this.parseObject();
    if (ch === "[") return this.parseArray();
    if (ch === '"') return this.parseString();
    if (ch === "-" || (ch >= "0" && ch <= "9")) return this.parseNumber();
    if (ch === "t" || ch === "f" || ch === "n") return this.parseLiteral();
    return { done: false, cause: "syntax" };
  }

  /** 对象:截断(自身 eof 或条目值 eof)时按已收集条目收尾;语法错误传播。 */
  private parseObject(): PartialResult {
    this.pos++; // consume '{'
    const entries: Record<string, unknown> = {};
    for (;;) {
      this.skipWs();
      const ch = this.peek();
      if (ch === undefined) {
        this.hitEof = true;
        return { done: true, value: entries };
      }
      if (ch === "}") {
        this.pos++;
        return { done: true, value: entries };
      }
      if (ch === ",") {
        // 条目分隔;开头出现逗号( '{"a":1,,' )按语法错误对待。
        if (Object.keys(entries).length === 0 && this.lastMeaningfulWasOpener()) {
          return { done: false, cause: "syntax" };
        }
        this.pos++;
        continue;
      }
      if (ch !== '"') return { done: false, cause: "syntax" };
      const key = this.parseString();
      if (!key.done) {
        if (key.cause === "eof") {
          this.hitEof = true;
          return { done: true, value: entries };
        }
        return key;
      }
      this.skipWs();
      if (this.peek() === undefined) {
        this.hitEof = true;
        return { done: true, value: entries };
      }
      if (this.peek() !== ":") return { done: false, cause: "syntax" };
      this.pos++;
      this.skipWs();
      const value = this.parseValue();
      if (!value.done) {
        // 截断:丢弃该条目(键一并丢);语法错误整体失败。
        if (value.cause === "eof") {
          this.hitEof = true;
          return { done: true, value: entries };
        }
        return value;
      }
      entries[key.value as string] = value.value;
    }
  }

  /** 数组:语义同 parseObject。 */
  private parseArray(): PartialResult {
    this.pos++; // consume '['
    const items: unknown[] = [];
    let expectValue = true;
    for (;;) {
      this.skipWs();
      const ch = this.peek();
      if (ch === undefined) {
        this.hitEof = true;
        return { done: true, value: items };
      }
      if (ch === "]") {
        if (expectValue && items.length > 0) return { done: false, cause: "syntax" };
        this.pos++;
        return { done: true, value: items };
      }
      if (ch === ",") {
        if (expectValue) return { done: false, cause: "syntax" };
        this.pos++;
        expectValue = true;
        continue;
      }
      if (!expectValue) return { done: false, cause: "syntax" };
      const value = this.parseValue();
      if (!value.done) {
        if (value.cause === "eof") {
          this.hitEof = true;
          return { done: true, value: items };
        }
        return value;
      }
      items.push(value.value);
      expectValue = false;
    }
  }

  /** 字符串:未闭合即截断(eof);转义按 JSON 语义解码(\n/\uXXXX 等)。 */
  private parseString(): PartialResult {
    this.pos++; // consume '"'
    let out = "";
    while (this.pos < this.text.length) {
      const ch = this.text[this.pos];
      if (ch === "\\") {
        const escape = this.decodeEscape();
        if (escape === "eof") {
          this.hitEof = true;
          return { done: false, cause: "eof" };
        }
        if (escape === "syntax") {
          return { done: false, cause: "syntax" };
        }
        out += escape;
        continue;
      }
      if (ch === '"') {
        this.pos++;
        return { done: true, value: out };
      }
      out += ch;
      this.pos++;
    }
    this.hitEof = true;
    return { done: false, cause: "eof" };
  }

  /** 消费一个以 `\\` 起始的转义序列,返回解码值;文本在序列中途结束 →
   *  "eof";非法转义 → "syntax"。 */
  private decodeEscape(): string | "eof" | "syntax" {
    const kind = this.pos + 1 < this.text.length ? this.text[this.pos + 1] : undefined;
    if (kind === undefined) return "eof";
    if (kind === "u") {
      const hex = this.text.slice(this.pos + 2, this.pos + 6);
      if (!/^[0-9a-fA-F]{4}$/.test(hex)) {
        // 不足 4 位:文本也到此为止按截断,否则按语法错误。
        return this.pos + 2 + hex.length >= this.text.length ? "eof" : "syntax";
      }
      this.pos += 6;
      return String.fromCharCode(parseInt(hex, 16));
    }
    const decoded = kind === "n" ? "\n"
      : kind === "t" ? "\t"
      : kind === "r" ? "\r"
      : kind === "b" ? "\b"
      : kind === "f" ? "\f"
      : kind === '"' || kind === "\\" || kind === "/" ? kind
      : undefined;
    if (decoded === undefined) {
      return "syntax";
    }
    this.pos += 2;
    return decoded;
  }

  private parseNumber(): PartialResult {
    const rest = this.text.slice(this.pos);
    const match = /^-?[0-9][0-9.eE+-]*/.exec(rest);
    if (!match) return { done: false, cause: "syntax" };
    const raw = match[0];
    const value = Number(raw);
    if (Number.isNaN(value)) {
      // `1e`(指数中途截断)在文本尽头按截断丢弃;畸形(如 `--3`)按语法错误。
      if (this.pos + raw.length >= this.text.length) {
        this.hitEof = true;
        return { done: false, cause: "eof" };
      }
      return { done: false, cause: "syntax" };
    }
    this.pos += raw.length;
    return { done: true, value };
  }

  private parseLiteral(): PartialResult {
    const rest = this.text.slice(this.pos);
    for (const literal of ["true", "false", "null"]) {
      if (rest.startsWith(literal)) {
        this.pos += literal.length;
        return { done: true, value: literal === "true" ? true : literal === "false" ? false : null };
      }
    }
    // 字面量真前缀且文本到此为止:截断(如 `tru`);否则语法错误。
    for (const literal of ["true", "false", "null"]) {
      if (literal.startsWith(rest) && rest.length < literal.length) {
        this.hitEof = true;
        return { done: false, cause: "eof" };
      }
    }
    return { done: false, cause: "syntax" };
  }

  /** 对象首条目前的逗号判定辅助:`{,` 形态。 */
  private lastMeaningfulWasOpener(): boolean {
    return this.text.slice(0, this.pos).trimEnd().endsWith("{");
  }

  private skipWs(): void {
    while (this.pos < this.text.length && /\s/.test(this.text[this.pos])) {
      this.pos++;
    }
  }

  private peek(): string | undefined {
    return this.pos < this.text.length ? this.text[this.pos] : undefined;
  }
}
