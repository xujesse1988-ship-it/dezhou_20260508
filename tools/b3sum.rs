//! 整文件 BLAKE3 校验和，写 `<file>.b3sum`（格式同 `b3sum` CLI：`<hex>  <basename>`）。
//!
//! vultr / 部分 host 没装 `b3sum` CLI，也没有 python `blake3`；artifact 的 `.b3sum`
//! sidecar（transfer / 基线锚点用）由本工具生成。注意这是**整文件** hash，与 bucket
//! table 内部 trailer（`BLAKE3(body[..len-32])`）是两个不同的量。
//!
//! 用法：`cargo run --release --bin b3sum -- FILE [FILE ...]`。
//! 每个 FILE 打印 `<hex>  <basename>` 到 stdout，并写同内容到 `<FILE>.b3sum`。

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: {} FILE [FILE ...]", args[0]);
        std::process::exit(2);
    }
    for path in &args[1..] {
        let bytes = fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let hash = blake3::hash(&bytes);
        let base = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path);
        let line = format!("{}  {}\n", hash.to_hex(), base);
        let sidecar = format!("{path}.b3sum");
        fs::write(&sidecar, &line).unwrap_or_else(|e| panic!("write {sidecar}: {e}"));
        print!("{line}");
    }
}
