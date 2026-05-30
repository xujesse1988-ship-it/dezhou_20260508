# CLAUDE.md

This file gives Claude Code guidance for working in this repository.

## 项目是什么

Heads-up No-Limit Texas Hold'em 求解器，默认 200BB profile，架构保留 6-max 扩展余地。Rust 实现。

## 每次开始工作前必读

1. `docs/status_v3.md` — 当前代码真实状态。**先看算法正确性表，再决定要不要动代码**。
2. `docs/invariants.md` — 代码层硬约束。违反这些规则的 PR 不通过。
3. `docs/six_max_nlhe_target.md` — **当前主线目标** = 6-max blueprint-only（路线 A / 100BB）阶段 S1–S5 量化门槛 + 代码就绪度。
4. `docs/heads_up_nlhe_solver_target.md` — heads-up 阶段（H1–H5），**已收尾**，仅剩 Slumbot 对战数据采集。

## 核心工作规则（覆盖默认行为）

### 1. 正确性大于一切

把正确性优先级放在第一位。

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
