# 第三方声明

PeerDesk 以 Apache-2.0 许可发布。下列代码改编自其他项目，按其许可证要求保留版权和许可声明。

## Airhand

平台层（`crates/platform`）的系统胶水部分改编自 Airhand 的被控端代码：

| PeerDesk 文件 | 改编自 Airhand 的文件 |
|---|---|
| `crates/platform/src/x11.rs` | `client/src-tauri/src/host/x11.rs`、`client/src-tauri/src/host/capture/x11.rs`、`client/src-tauri/src/host/input/x11.rs` |
| `crates/platform/src/windows.rs` | `client/src-tauri/src/host/capture/dxgi.rs`、`client/src-tauri/src/host/input/windows.rs`、`client/src-tauri/src/host/mod.rs` |
| `crates/platform/src/macos.rs` | `client/src-tauri/src/host/sck.rs`、`client/src-tauri/src/host/macos.rs`、`client/src-tauri/src/host/capture/sck.rs`、`client/src-tauri/src/host/input/macos.rs` |

```text
MIT License

Copyright (c) 2026 sqlpxc

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

## libopus（声音编解码，编译进客户端）

来源：https://opus-codec.org/ ，经 Rust crate `opus` / `opusic-sys` 自带源码编译。许可证 BSD-3-Clause，原文如下：

```
Copyright 2001-2023 Xiph.Org, Skype Limited, Octasic,
                    Jean-Marc Valin, Timothy B. Terriberry,
                    CSIRO, Gregory Maxwell, Mark Borgerding,
                    Erik de Castro Lopo, Mozilla, Amazon

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

- Redistributions of source code must retain the above copyright
notice, this list of conditions and the following disclaimer.

- Redistributions in binary form must reproduce the above copyright
notice, this list of conditions and the following disclaimer in the
documentation and/or other materials provided with the distribution.

- Neither the name of Internet Society, IETF or IETF Trust, nor the
names of specific contributors, may be used to endorse or promote
products derived from this software without specific prior written
permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
``AS IS'' AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER
OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

Opus is subject to the royalty-free patent licenses which are
specified at:

Xiph.Org Foundation:
https://datatracker.ietf.org/ipr/1524/

Microsoft Corporation:
https://datatracker.ietf.org/ipr/1914/

Broadcom Corporation:
https://datatracker.ietf.org/ipr/1526/
```

## Phosphor Icons（界面图标）

来源：https://phosphoricons.com ，经 Rust crate `egui-phosphor`（MIT OR Apache-2.0）。图标字体许可证 MIT：

```
MIT License

Copyright (c) 2020 Phosphor Icons

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

## Microsoft MMSys 2024 带宽估计挑战赛数据集（CC BY 4.0）

`crates/e2e/traces/ms-*.csv` 是从该数据集的仿真通话中摘取的真实带宽和丢包轨迹（由 `scripts/ms-traces.py` 转换，
只保留变化最多的 40 秒、只记变化时刻）。来源：Microsoft，
https://github.com/microsoft/RL4BandwidthEstimationChallenge ，
数据集说明 https://www.microsoft.com/en-us/research/academic-program/bandwidth-estimation-challenge/data/ ，
许可 Creative Commons Attribution 4.0（https://creativecommons.org/licenses/by/4.0/）。
论文：Khairy et al., "ACM MMSys 2024 Bandwidth Estimation in Real Time Communications Challenge", arXiv:2403.06324。
已做的改动：截取窗口、百分数换成千分比、缺失值沿用上一步。
