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
