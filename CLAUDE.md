# CLAUDE.md

This file gives Claude Code guidance for working in this repository.

## 项目是什么

8-stage Pluribus 风格 6-max NLHE 扑克 AI。Rust 实现。

## 每次开始工作前必读

1. `docs/status.md` — 当前代码真实状态 + 下一步唯一允许的工作。**先看算法正确性表，再决定要不要动代码**。
2. `docs/invariants.md` — 代码层硬约束。违反这些规则的 PR 不通过。
3. `docs/pluribus_path.md` — 8 阶段 roadmap 和量化门槛。

历史决策、carve-out、流程叙事一律不在文档里保留。需要查走 `git log` / `git show`。

## 核心工作规则（覆盖默认行为）

### 1. 正确性大于一切

任何 CFR / MCCFR / 抽象层 / 评估器的代码改动，**必须附外部对照证据**（OpenSpiel / `leduc_mccfr.py` / 论文已知 Nash value）。

- "按论文重写" 不算证据 —— 论文里 outcome sampling 和 external sampling 经常同一节，搞错过一次。
- BLAKE3 byte-equal 复现也不算证据 —— 一个错算法可以完美重复出错的输出。
- 唯一证据：在已知 Nash 解的小博弈（Kuhn / Leduc）上输出对得上。

### 2. 不追加，直接改

文档错了 → 改文档。代码错了 → 改代码。`git` 自己有历史。

不写"修订历史" / "carve-out" / "已废弃保留" / "已知偏离 stage X+1 修"。
这些机制在本项目史上已被证明会让错误滚雪球（详见 `git show HEAD project_post_mortem.md` 如果还在）。

### 3. `closed` 不接受 "with known deviations"

stage 验收门槛全部 hard pass 才能 close。
偏离阈值 10× 以上 → 停下来怀疑算法，不是写 carve-out。

### 4. 反模式

- 优化在前于正确性 —— 算法没在 Leduc 上对照外部 ref 通过之前，不写 `perf` / `rayon` / `AVX2` / `quantize` 类改动。
- 用内部 doc ID（D-NNN）替代外部 ground truth。
- 提前抽象 —— Vanilla CFR + Kuhn 200 行能写完，先把这个写到对，再去抽 trait。
- 跟 PokerKit 行为不一致默认是 PokerKit 对，先怀疑自己。

## 构建 / 测试

```bash
./scripts/setup-rust.sh                          # 一次性，装 rustup + 锁工具链 1.95.0
cargo build --all-targets
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test                                       # 默认套件
cargo test --release -- --ignored                # 长跑套件
```

可选 PokerKit 跨验证：

```bash
uv venv --python 3.11 .venv-pokerkit
uv pip install --python .venv-pokerkit/bin/python "pokerkit==0.4.14"
PATH=".venv-pokerkit/bin:$PATH" cargo test
```

`.venv-pokerkit/` 在 `.gitignore` 内。

## 工作语言

`docs/` 中文，commit message 中文，code identifier + 行内注释英文（Rust 习惯）。
