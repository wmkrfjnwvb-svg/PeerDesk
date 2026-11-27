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

来源：https://phosphoricons.com ，经 Rust crate `egui-phosphor`（MIT OR Apache-2.0）。客户端只带用到的图标（`crates/client/assets/fonts/phosphor-subset.ttf`，由 `scripts/build-fonts.py` 从原字体裁出）。图标字体许可证 MIT：

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

## Inter、Noto Sans SC（思源黑体）、Noto Emoji（界面字体，打进客户端）

客户端自带界面字体的子集（`crates/client/assets/fonts/`，由 `scripts/build-fonts.py` 生成），
都以 SIL Open Font License 1.1 授权。子集是修改版，仍以 OFL 1.1 授权；没有使用保留字体名
（Inter 没有保留名；思源黑体的保留名是 “Source”，我们的文件叫 Noto Sans SC）。

- Inter 4.0（拉丁字母，Regular、Medium、SemiBold；数字换成了等宽数字）
  来源：https://github.com/rsms/inter
  Copyright 2016 The Inter Project Authors
- Noto Sans CJK SC 2.004（中文 Regular：GB 2312 的全部汉字和符号）
  来源：https://github.com/notofonts/noto-cjk
  © 2014-2021 Adobe (http://www.adobe.com/).
- Noto Sans SC（中文 500、600 字重：只含界面文案里出现的字）
  来源：Google Fonts，经 npm 包 `@fontsource/noto-sans-sc` 5.3.1
  (c) 2014-2021 Adobe (http://www.adobe.com/), with Reserved Font Name 'Source'.
- Noto Emoji（单色表情符号，来自 Rust crate `epaint_default_fonts` 0.32，未修改）
  来源：https://github.com/googlefonts/noto-emoji
  Copyright 2013 Google Inc. All Rights Reserved.

许可证全文：

```
-----------------------------------------------------------
SIL OPEN FONT LICENSE Version 1.1 - 26 February 2007
-----------------------------------------------------------

PREAMBLE
The goals of the Open Font License (OFL) are to stimulate worldwide
development of collaborative font projects, to support the font creation
efforts of academic and linguistic communities, and to provide a free and
open framework in which fonts may be shared and improved in partnership
with others.

The OFL allows the licensed fonts to be used, studied, modified and
redistributed freely as long as they are not sold by themselves. The
fonts, including any derivative works, can be bundled, embedded,
redistributed and/or sold with any software provided that any reserved
names are not used by derivative works. The fonts and derivatives,
however, cannot be released under any other type of license. The
requirement for fonts to remain under this license does not apply
to any document created using the fonts or their derivatives.

DEFINITIONS
"Font Software" refers to the set of files released by the Copyright
Holder(s) under this license and clearly marked as such. This may
include source files, build scripts and documentation.

"Reserved Font Name" refers to any names specified as such after the
copyright statement(s).

"Original Version" refers to the collection of Font Software components as
distributed by the Copyright Holder(s).

"Modified Version" refers to any derivative made by adding to, deleting,
or substituting -- in part or in whole -- any of the components of the
Original Version, by changing formats or by porting the Font Software to a
new environment.

"Author" refers to any designer, engineer, programmer, technical
writer or other person who contributed to the Font Software.

PERMISSION & CONDITIONS
Permission is hereby granted, free of charge, to any person obtaining
a copy of the Font Software, to use, study, copy, merge, embed, modify,
redistribute, and sell modified and unmodified copies of the Font
Software, subject to the following conditions:

1) Neither the Font Software nor any of its individual components,
in Original or Modified Versions, may be sold by itself.

2) Original or Modified Versions of the Font Software may be bundled,
redistributed and/or sold with any software, provided that each copy
contains the above copyright notice and this license. These can be
included either as stand-alone text files, human-readable headers or
in the appropriate machine-readable metadata fields within text or
binary files as long as those fields can be easily viewed by the user.

3) No Modified Version of the Font Software may use the Reserved Font
Name(s) unless explicit written permission is granted by the corresponding
Copyright Holder. This restriction only applies to the primary font name as
presented to the users.

4) The name(s) of the Copyright Holder(s) or the Author(s) of the Font
Software shall not be used to promote, endorse or advertise any
Modified Version, except to acknowledge the contribution(s) of the
Copyright Holder(s) and the Author(s) or with their explicit written
permission.

5) The Font Software, modified or unmodified, in part or in whole,
must be distributed entirely under this license, and must not be
distributed under any other license. The requirement for fonts to
remain under this license does not apply to any document created
using the Font Software.

TERMINATION
This license becomes null and void if any of the above conditions are
not met.

DISCLAIMER
THE FONT SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO ANY WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT
OF COPYRIGHT, PATENT, TRADEMARK, OR OTHER RIGHT. IN NO EVENT SHALL THE
COPYRIGHT HOLDER BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
INCLUDING ANY GENERAL, SPECIAL, INDIRECT, INCIDENTAL, OR CONSEQUENTIAL
DAMAGES, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF THE USE OR INABILITY TO USE THE FONT SOFTWARE OR FROM
OTHER DEALINGS IN THE FONT SOFTWARE.
```

## Microsoft MMSys 2024 带宽估计挑战赛数据（仓库自带的 24 通评测通话）

`crates/e2e/traces/ms-*.csv` 是从该挑战赛仓库自带的 `data/` 目录（24 通仿真评测通话，带真实带宽和丢包）中摘取的轨迹，
由 `scripts/ms-traces.py` 转换：只保留变化最多的 40 秒、只记变化时刻、百分数换成千分比、缺失值沿用上一步。

- 来源：Microsoft，https://github.com/microsoft/RL4BandwidthEstimationChallenge （`data/` 目录）。
- 论文：Khairy et al., "ACM MMSys 2024 Bandwidth Estimation in Real Time Communications Challenge", arXiv:2403.06324。
- 许可：两种说法都照办。
  - 该仓库的 LICENSE 文件是 MIT，原文见下。
  - 仓库 README 写明仓库里的文档和其他内容按 Creative Commons Attribution 4.0 授权（https://creativecommons.org/licenses/by/4.0/）。
- 不在此列：挑战赛另外放在 Azure 上、用脚本下载的仿真数据集（约 8 GB）和测试床数据集（约 35 GB）不在该仓库里。
  README 的 “Dataset licenses” 一节对它们只有免责声明，没有写明授权。PeerDesk 仓库不收录这两份数据的任何内容。

```
MIT License

Copyright (c) Microsoft Corporation.

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
SOFTWARE
```

## cros-codecs、cros-libva（Linux 硬件编解码，编译进客户端）

来源：https://github.com/chromeos/cros-codecs 、https://github.com/chromeos/cros-libva 。许可证 BSD-3-Clause，原文如下：

```
Copyright 2022 The ChromiumOS Authors

Redistribution and use in source and binary forms, with or without modification,
are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its contributors
   may be used to endorse or promote products derived from this software without
   specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR
ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
(INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON
ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

## libva 头文件（编译 Linux 硬件编解码时用）

来源：https://github.com/intel/libva 2.20.0，放在 `third_party/libva/include`，未修改。MIT 许可，原文如下（各文件开头也有）：

```
Copyright (c) 2007-2009 Intel Corporation. All Rights Reserved.

Permission is hereby granted, free of charge, to any person obtaining a
copy of this software and associated documentation files (the
"Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish,
distribute, sub license, and/or sell copies of the Software, and to
permit persons to whom the Software is furnished to do so, subject to
the following conditions:

The above copyright notice and this permission notice (including the
next paragraph) shall be included in all copies or substantial portions
of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NON-INFRINGEMENT.
IN NO EVENT SHALL INTEL AND/OR ITS SUPPLIERS BE LIABLE FOR
ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
/
```

## 系统托盘（关闭退到托盘，编进客户端）

这些是依赖库，没有拷贝它们的代码。按各自许可证随二进制发布，这里写明来源和许可：

| 库 | 用在 | 许可证 | 版权 |
|---|---|---|---|
| `tray-icon` 0.24 及其依赖 `muda` 0.19 | Windows、macOS 的托盘（菜单栏）图标和右键菜单 | MIT OR Apache-2.0（我们按 MIT 使用） | Copyright (c) 2022-2022 Tauri Programme within The Commons Conservancy |
| `ksni` 0.3 | Linux 的托盘图标（StatusNotifierItem，纯 Rust 的 D-Bus） | Unlicense（公有领域） | 无 |

Linux 不用 `tray-icon`，所以不依赖 GTK3 和 libayatana-appindicator（LGPL 系统库）：`ksni` 不需要任何系统库，
运行时只要桌面有 StatusNotifierWatcher（KDE、XFCE、Cinnamon、MATE 自带；GNOME 要 AppIndicator 扩展）。
`Cargo.lock` 里会出现 `tray-icon` 在 Linux 上的可选依赖（gtk、libappindicator 等，MIT）和 `dirs-sys` 用到的 `option-ext`（MPL-2.0），
它们只在 `tray-icon` 的 Linux 实现里用到，我们的构建不编译、不分发。

`tray-icon` 的 MIT 许可证原文：

```
MIT License

Copyright (c) 2022-2022 Tauri Programme within The Commons Conservancy

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
