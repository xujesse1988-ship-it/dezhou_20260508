# dezhou_20260508

6-max NLHE poker AI（Pluribus 风格）。当前处于 8 阶段路径的**阶段 1**（规则环境 + 手牌评估器）。

## 当前进度

- ✅ A0：技术栈与 API 契约锁定（`docs/pluribus_stage1_decisions.md` / `docs/pluribus_stage1_api.md`）
- ✅ A1：API 骨架代码化（`src/`，全部方法 `unimplemented!()`）
- ⏭️ B1：核心场景测试 + harness 骨架（[测试] agent，下一步）

## 快速上手

```bash
# 一次性安装 Rust stable（含 rustfmt + clippy；幂等，已装则跳过）
./scripts/setup-rust.sh

# 当前 shell 加载 cargo 到 PATH（首次安装后需要；之后新 shell 自动）
. "$HOME/.cargo/env"

# A1 出口四件套
cargo build --all-targets
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```

## 文档导航

按权威级别从高到低读：

1. `docs/pluribus_path.md` — 8 阶段总路径与验收门槛
2. `docs/pluribus_stage1_validation.md` — 阶段 1 量化验收标准
3. `docs/pluribus_stage1_decisions.md` — 锁定的技术 / 规则决策（D-NNN）
4. `docs/pluribus_stage1_api.md` — 锁定的 Rust API 契约（API-NNN）
5. `docs/pluribus_stage1_workflow.md` — 13 步多 agent 协作流程（A0 → F3）
6. `CLAUDE.md` — 仓库导航 + 阶段 1 不变量摘要（给 Claude Code 用）

决策 / API 修改流程见 `pluribus_stage1_decisions.md` §10 与 `pluribus_stage1_api.md` §11（D-NNN-revM / API-NNN-revM 追加，不删除原条目）。

## 工作语言

文档与 commit message 中文；Rust 代码标识符与内联注释英文。

## License

MIT OR Apache-2.0。
