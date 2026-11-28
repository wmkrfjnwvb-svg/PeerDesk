# 固定的对比基准：12 条微软轨迹 × 12 个种子（144 个世界）

改了传输、码率控制、帧率调节或编码，合并之前在这批世界上跑一遍，和存下的基线逐对比较：

```bash
scripts/bench-ms144.sh
```

- `worlds/`：144 个世界文件（格式见 `crates/e2e/src/world.rs`，文件名是内容哈希）。
  - 由 `microsoft_traces` 测试导出：`PEERDESK_SEEDS=12 PEERDESK_E2E_WORLDS=<目录> cargo test --release -p peerdesk-e2e --test ms_traces microsoft_traces -- --ignored --exact`，2026-11-28。
  - 世界文件自包含：以后测试代码怎么改，这批世界都不变。
  - 内联的带宽轨迹来自 `crates/e2e/traces/`（Microsoft MMSys 2024 挑战赛仓库自带的数据，MIT / CC BY 4.0，见 `THIRD_PARTY_NOTICES.md`）。
- `baseline.csv`：在这批世界上跑出来的结果，批量运行器的 CSV 格式（`crates/e2e/examples/batch.rs`）。
  - 当前基线是 #71 第 2 步修完的版本（帧率码率稳定，`docs/research/P-fps-bitrate.md` 第 10 节）。
  - 和它之前的版本比：冻结 2.1 → 1.7，帧率换档 5.0 → 3.6，码率 5% 分位 +93%。

**规则**（和 `crates/e2e/tests/ms_traces.rs` 的回归门槛一致，只升不降）：
- 判好坏看配对比较：差的均值加减两倍标准误不跨 0 才算有差别。脚本会按场景名分组列出来，任何一条轨迹变差都要先查原因。
- 只有正式的改进合并时才换 `baseline.csv`，提交信息里写上这次的配对比较结果。不按“看不出变差”的结果换基线，否则一连串小退步会累积。
- 换基线时同时把新的汇总存进 `docs/research/data/`，和以前的基线文件放在一起。
