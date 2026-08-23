# AStock 版本化评测

评测完全离线，不访问网页、不调用 MiniMax，也不依赖来源页面永久存在。数据集包含冻结快照的必要结构化事实与执行轨迹，运行器输出可比较的 JSON 和可阅读的 HTML。

运行 Gate 0 基线：

```powershell
cargo run -p astock-evaluation --bin astock-eval -- evaluate `
  --dataset eval/datasets/p0-v1/dataset.json `
  --thresholds eval/datasets/p0-v1/thresholds.json `
  --baseline eval/baselines/p0-v1-test.json `
  --json target/eval-reports/p0-v1-test.json `
  --html target/eval-reports/p0-v1-test.html `
  --split test --check
```

比较两个历史报告：

```powershell
cargo run -p astock-evaluation --bin astock-eval -- compare `
  --from eval/baselines/p0-v1-test.json `
  --to target/eval-reports/p0-v1-test.json `
  --json target/eval-reports/comparison.json
```

数据、解析器、本体、提示词或模型轨迹发生变化时，应创建新版本快照和基线，不得覆盖旧版本。发布说明中的能力描述必须能映射到 `thresholds.json` 中已通过的指标；不能用本评测支持范围之外的“完整”“零遗漏”“保证正确”等表述。

只有在评审新的数据集版本时，才可用 `--establish-baseline` 生成该版本的第一个基线；此模式不能同时传入旧基线。日常 CI 不允许使用该选项，缺失固定基线会按失败处理。
