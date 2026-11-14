import type { GatewayProviderProtocol } from "@/contracts/app";
import { cn } from "@/lib/utils";
import {
  BUILTIN_CLIENT_FINGERPRINT_PRESETS, Field, FieldLabelHint, SelectControl, useAppText
} from "../../shared";
import { clientFingerprintPresetCompatible } from "../../shared/providers";

// 方案 20261016_03:供应商「客户端指纹」字段 —— 新建供应商表单(草稿态)与
// 已存供应商详情面板共用同一份交互:
//   [模式:关闭 | 透传真实客户端身份 | 内置预设] [预设下拉]
// 双框恒显示(镜像 ProviderProxyField 二轮范式):未选「内置预设」时预设框
// 冻结。预设与当前 API 格式不兼容时禁选并给原因;权威在后端保存门禁
// (未知 id / 协议不配 → 400)。组件不持久化,档位变化经 onChange 上抛。

export function ProviderClientFingerprintField({
  className,
  labelClassName,
  mode,
  onChange,
  presetId,
  protocol
}: {
  className?: string;
  /** 透传给 Field 的标签样式(详情页章节小标题样式时用)。 */
  labelClassName?: string;
  mode: "off" | "passthrough" | "preset";
  onChange: (mode: "off" | "passthrough" | "preset", presetId: string) => void;
  presetId: string;
  protocol: GatewayProviderProtocol;
}) {
  const t = useAppText();
  const modeOptions = [
    { label: t("Off"), value: "off" },
    { label: t("Pass through client identity"), value: "passthrough" },
    { label: t("Built-in preset"), value: "preset" }
  ];
  // 纯受控:悬空预设引用由父级收敛,这里只做显示回退,不回写。
  const presets = BUILTIN_CLIENT_FINGERPRINT_PRESETS;
  const hasSelection = presets.some((preset) => preset.id === presetId);
  const presetValue = hasSelection ? presetId : "";
  const compatiblePresets = presets.filter((preset) =>
    clientFingerprintPresetCompatible(preset, protocol)
  );
  const presetDisabled = mode !== "preset" || compatiblePresets.length === 0;
  const presetOptions = [
    ...(presetValue ? [] : [{ label: t("No fingerprint preset selected"), value: "" }]),
    ...presets.map((preset) => {
      const compatible = clientFingerprintPresetCompatible(preset, protocol);
      return {
        // 禁选项保留在列表中并给原因(用户能看见「为什么不可选」)。
        label: compatible
          ? preset.label
          : `${preset.label} (${t("not compatible with this API format")})`,
        value: compatible ? preset.id : "",
        disabled: !compatible
      };
    })
  ];

  return (
    <div className={cn("grid grid-cols-1 gap-3", className)}>
      <Field
        label={t("Client fingerprint")}
        labelClassName={labelClassName}
        labelHint={
          <FieldLabelHint>
            {t("Client fingerprint hint")}
          </FieldLabelHint>
        }
      >
        <div className="flex items-center gap-2">
          <div className="w-[180px] shrink-0">
            <SelectControl
              aria-label={t("Client fingerprint")}
              onChange={(value) => {
                const next = value as "off" | "passthrough" | "preset";
                // 切到「内置预设」时默认选中第一个兼容预设(有可选时),省一次点击。
                onChange(next, next === "preset" ? presetId || compatiblePresets[0]?.id || "" : "");
              }}
              options={modeOptions}
              value={mode}
            />
          </div>
          <div className="min-w-0 flex-1">
            <SelectControl
              aria-label={t("Fingerprint presets")}
              disabled={presetDisabled}
              onChange={(value) => value && onChange("preset", value)}
              options={presetOptions}
              value={presetValue}
            />
          </div>
        </div>
      </Field>
    </div>
  );
}
