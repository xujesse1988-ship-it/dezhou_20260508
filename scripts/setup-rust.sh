#!/usr/bin/env bash
#
# 安装 Rust stable toolchain（含 rustfmt + clippy）。
# 在 A1 阶段启动时使用；B1 起的所有 agent 上手前也跑一次。
#
# 用法：
#   ./scripts/setup-rust.sh
#
# 已装则跳过；安装后提示 source ~/.cargo/env 以激活当前 shell。

set -euo pipefail

REQUIRED_COMPONENTS=(rustfmt clippy)

# 即使 PATH 没有 cargo 也尝试 source 一下（典型场景：rustup 刚装完、当前 shell
# 未重启；或 cron / 容器入口跳过 rc 文件）。
if [ -f "$HOME/.cargo/env" ] && ! command -v cargo >/dev/null 2>&1; then
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
fi

if command -v cargo >/dev/null 2>&1 && command -v rustc >/dev/null 2>&1; then
    echo "[setup-rust] cargo / rustc 已存在，跳过 rustup 安装。"
    echo "[setup-rust] cargo:   $(cargo --version)"
    echo "[setup-rust] rustc:   $(rustc --version)"

    # 已装但缺组件时补装
    if command -v rustup >/dev/null 2>&1; then
        for comp in "${REQUIRED_COMPONENTS[@]}"; do
            if ! rustup component list --installed 2>/dev/null | grep -q "^${comp}-"; then
                echo "[setup-rust] 缺组件 ${comp}，rustup 补装。"
                rustup component add "${comp}"
            fi
        done
    fi
else
    echo "[setup-rust] 未检测到 cargo，使用 rustup 非交互安装 stable。"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- \
        -y \
        --default-toolchain stable \
        --profile minimal \
        --component rustfmt \
        --component clippy
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
fi

# 验证四件套
echo "[setup-rust] 验证："
cargo --version
rustc --version
rustfmt --version
cargo clippy --version

cat <<'EOF'

[setup-rust] 完成。如果是首次安装，请在当前 shell 执行：

    . "$HOME/.cargo/env"

或重启 shell 让 rustup 写入的 PATH 生效。
EOF
