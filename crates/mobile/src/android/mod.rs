//! Android 的系统接口胶水：MediaCodec 解码到 Surface（`codec.rs`）、给 Kotlin 调的 JNI 函数（`bridge.rs`）。
//! 只在 Android 上编译；`unsafe` 只在这两个文件里，每处写 SAFETY。逻辑都在平台无关的部分，这里只是接线。

mod bridge;
mod codec;
