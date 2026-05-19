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

尽力保证正确。

### 2. 不追加，直接改


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
