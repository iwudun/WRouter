# 第三方组件与数据声明

本仓库包含或分发以下第三方作品。各组件的版权与许可如下，使用或再分发时请一并保留。

## 1. ModelTrace（指纹算法来源）

- 来源：https://github.com/xqy2006/ModelTrace
- 版权：Copyright (c) 2026 xqy2006
- 许可：MIT（全文见文末）
- 说明：`backend/crates/wr-domain/src/fingerprint/` 的特征提取、打分与建库逻辑移植自上游 Python 实现；内置参考库、探针套件原件与随仓数据已下线，不再分发。

## 2. Claude Code Router（早期界面参考）

- 来源：https://github.com/musistudio/claude-code-router
- 版权：Copyright (c) 2025 musistudio
- 许可：MIT（全文见文末）
- 说明：`frontend/` 早期 fork 自 `@claude-code-router/ui`，现已按本项目设计重写；保留此声明以尊重原始工作。

## 3. 设计参考（未复制源码）

以下项目仅作为架构、协议口径或交互设计的参考，本仓库未复制其源代码：

| 项目 | 版权 | 许可 |
| --- | --- | --- |
| magpie（yetone） | Copyright (c) 2026 yetone | MIT |
| cc-switch（Jason Young） | Copyright (c) 2025 Jason Young | MIT |
| ai-gateway（musistudio） | Copyright (c) 2026 musistudio | MIT |
| OmniRoute（diegosouzapw） | Copyright (c) 2026 diegosouzapw | MIT |
| WaLiAPI（xiaofuge） | Copyright (c) 2026 xiaofuge | MIT |

本项目未使用上述项目的 Logo，也不暗示其作者对本项目的背书。

## 4. 许可全文

### MIT License（ModelTrace）

```text
MIT License

Copyright (c) 2026 xqy2006

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

### MIT License（Claude Code Router）

```text
MIT License

Copyright (c) 2025 musistudio

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
