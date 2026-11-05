//! 8 座 ante blueprint 翻前策略查看器（本地只读网页）。
//!
//! ```bash
//! cargo run --release --bin preflop_viewer -- [--port 8790] [--host 127.0.0.1] [--artifacts artifacts]
//! ```
//!
//! 平时由牌局回放服务拉起并挂在它的 `/preflop/` 下（`python3 -m opponent_analysis history --preflop`，
//! 见 `opponent_analysis/preflop_proxy.py`）：`--port 0` 让系统选端口，实际端口打在 stderr 的「打开」一行；
//! `--exit-on-stdin-eof` 让本进程随父进程退出。页面用相对路径请求接口，所以挂在子路径下也能用。
//!
//! 启动时逐个 blueprint 建全树、校验 crop 指纹和整文件 trailer，只留下翻前骨架后释放全树；页面请求时
//! 再从磁盘读路径上的节点（[`CropStrategyReader`]），所以两张值表都不进内存。建树只用
//! `(TableConfig, reshape)`，不加载桶表（翻前行与桶表无关）。
//!
//! 接口：`GET /` 页面；`GET /api/meta` blueprint 列表；`GET /api/node?bp=<id>&line=F,F,R333`
//! 沿动作线走到某个决策点，返回该行动者 169 手的策略、到达概率和累计访问权重。动作线走不通时退回最长
//! 可走前缀（两个 blueprint 的动作不完全相同，切换时会遇到）。
//!
//! 动作记号：`F` 弃 / `X` 过 / `C` 跟 / `A` 全下 / `R<milli>` 按底池比例加注（`R333` = 1/3 pot）。
//!
//! 到达概率 = 该座位沿路径上自己每一步选对应动作的概率之积；总频率按「组合数 × 到达概率」加权，不算牌阻断。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use poker::abstraction::action::{AbstractAction, ActionAbstraction, StreetActionAbstraction};
use poker::core::rng::ChaCha20Rng;
use poker::core::PlayerStatus;
use poker::training::nlhe_betting_tree::{
    six_max_reshape, AbstractActionTag, Child, NodeId, PublicBettingTree,
};
use poker::training::nlhe_dense_crop::{CropNodeRows, CropStrategyReader};
use poker::training::nlhe_replay::find_tag;
use poker::{Card, ChipAmount, GameState, PreflopLossless169, Rank, StreetTag, Suit, TableConfig};
use serde_json::{json, Value};

const PAGE: &str = include_str!("preflop_viewer.html");

const SEATS: u8 = 8;
const STACK_BB: u32 = 200;
const ANTE_CHIPS: u64 = 100;
const POSTFLOP_CAP: u8 = 4;
const CHIPS_PER_BB: f64 = 100.0;
/// 翻前 169 类；翻后桶数只参与全树指纹（500 桶训练）。
const BUCKETS: [u32; 4] = [169, 500, 500, 500];
const HANDS: usize = 169;
/// 按座位号：BTN=0 SB=1 BB=2 UTG=3 ...
const POSITIONS: [&str; SEATS as usize] = ["BTN", "SB", "BB", "UTG", "UTG1", "LJ", "HJ", "CO"];
/// 翻前行动顺序（页面座位表用）。
const ACTION_ORDER: [usize; SEATS as usize] = [3, 4, 5, 6, 7, 0, 1, 2];

struct BlueprintSpec {
    id: &'static str,
    label: &'static str,
    note: &'static str,
    run_dir: &'static str,
    crop_file: &'static str,
    reshape: &'static str,
}

const BLUEPRINTS: [BlueprintSpec; 2] = [
    BlueprintSpec {
        id: "open2",
        label: "3bet + open2",
        note: "开池 1/3 pot 或 1 pot；3bet 1/2 pot 或 1 pot；4bet+ 1 pot",
        run_dir: "run_8max_ante100_200bb_antefl2_3bet_open2_cap4_b500",
        crop_file: "nlhe_8max_ante100_200bb_antefl2_3bet_open2_cap4_b500_9b_preflop_stratonly.crop",
        reshape: "ante-flop2-3bet-open2",
    },
    BlueprintSpec {
        id: "3bet",
        label: "3bet",
        note: "开池只有 1/3 pot；3bet 1/2 pot 或 1 pot；4bet+ 1 pot",
        run_dir: "run_8max_ante100_200bb_antefl2_3bet_cap4_b500",
        crop_file: "nlhe_8max_ante100_200bb_antefl2_3bet_cap4_b500_9b_preflop_stratonly.crop",
        reshape: "ante-flop2-3bet",
    },
];

/// 边的去向：`Flop` / `End` 在翻前查看器里不再往下走。
#[derive(Clone, Copy)]
enum Next {
    Preflop(NodeId),
    Flop,
    End,
}

struct PreNode {
    actor: u8,
    edges: Vec<(AbstractActionTag, Next)>,
}

struct Blueprint {
    spec: &'static BlueprintSpec,
    cfg: TableConfig,
    abs: StreetActionAbstraction,
    reader: CropStrategyReader,
    nodes: HashMap<NodeId, PreNode>,
    root: NodeId,
}

struct Hand {
    label: String,
    bucket: usize,
    combos: f64,
}

struct App {
    blueprints: Vec<Blueprint>,
    /// 13×13 行优先：对角线对子，右上同花，左下不同花。
    hands: Vec<Hand>,
}

fn hand_grid() -> Vec<Hand> {
    use Rank::*;
    let ranks = [
        Ace, King, Queen, Jack, Ten, Nine, Eight, Seven, Six, Five, Four, Three, Two,
    ];
    let names = [
        'A', 'K', 'Q', 'J', 'T', '9', '8', '7', '6', '5', '4', '3', '2',
    ];
    let abstraction = PreflopLossless169::new();
    let mut out = Vec::with_capacity(HANDS);
    for i in 0..13 {
        for j in 0..13 {
            let (hi, lo) = (i.min(j), i.max(j));
            let second_suit = if j > i { Suit::Spades } else { Suit::Hearts };
            let hole = [
                Card::new(ranks[hi], Suit::Spades),
                Card::new(ranks[lo], second_suit),
            ];
            let (suffix, combos) = match i.cmp(&j) {
                std::cmp::Ordering::Equal => ("", 6.0),
                std::cmp::Ordering::Less => ("s", 4.0),
                std::cmp::Ordering::Greater => ("o", 12.0),
            };
            out.push(Hand {
                label: format!("{}{}{}", names[hi], names[lo], suffix),
                bucket: usize::from(abstraction.hand_class(hole)),
                combos,
            });
        }
    }
    let mut buckets: Vec<usize> = out.iter().map(|h| h.bucket).collect();
    buckets.sort_unstable();
    assert_eq!(
        buckets,
        (0..HANDS).collect::<Vec<_>>(),
        "169 手须一一对应 169 个桶"
    );
    out
}

fn token_of(tag: AbstractActionTag) -> String {
    match tag {
        AbstractActionTag::Fold => "F".into(),
        AbstractActionTag::Check => "X".into(),
        AbstractActionTag::Call => "C".into(),
        AbstractActionTag::AllIn => "A".into(),
        AbstractActionTag::Bet(r) | AbstractActionTag::Raise(r) => format!("R{}", r.as_milli()),
    }
}

fn ratio_label(milli: u32) -> String {
    match milli {
        333 => "1/3 pot".into(),
        500 => "1/2 pot".into(),
        1000 => "1 pot".into(),
        m => format!("{:.2} pot", f64::from(m) / 1000.0),
    }
}

fn bb(chips: ChipAmount) -> f64 {
    chips.as_u64() as f64 / CHIPS_PER_BB
}

fn fmt_bb(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    format!("{s}BB")
}

/// `tag` 在 `state` 上对应的具体动作（带真实金额）。
fn concrete(
    bp: &Blueprint,
    state: &GameState,
    tag: AbstractActionTag,
) -> Result<AbstractAction, String> {
    let set = bp.abs.abstract_actions(state);
    find_tag(set.as_slice(), tag)
        .ok_or_else(|| format!("{tag:?} 不在抽象动作集里（树与重放不一致）"))
}

/// 动作在 `actor` 当前局面下的 JSON（类别 / 比例 / 文案 / 金额）。
fn describe(action: &AbstractAction, actor_committed: ChipAmount) -> Value {
    let (kind, ratio, label, to) = match *action {
        AbstractAction::Fold => ("fold", None, "弃牌".to_string(), None),
        AbstractAction::Check => ("check", None, "过牌".to_string(), None),
        AbstractAction::Call { to } => (
            "call",
            None,
            format!("跟注 {}", fmt_bb(bb(to) - bb(actor_committed))),
            Some(bb(to)),
        ),
        AbstractAction::Bet { to, ratio_label: r }
        | AbstractAction::Raise { to, ratio_label: r } => (
            "raise",
            Some(r.as_milli()),
            format!("加注到 {}（{}）", fmt_bb(bb(to)), ratio_label(r.as_milli())),
            Some(bb(to)),
        ),
        AbstractAction::AllIn { to } => (
            "allin",
            None,
            format!("全下（到 {}）", fmt_bb(bb(to))),
            Some(bb(to)),
        ),
    };
    json!({ "kind": kind, "ratio": ratio, "label": label, "to_bb": to })
}

fn round_to(v: f64, digits: i32) -> f64 {
    let p = 10f64.powi(digits);
    (v * p).round() / p
}

fn committed_of(state: &GameState, seat: usize) -> ChipAmount {
    state.players()[seat].committed_this_round
}

/// 按「组合数 × `reach`」加权的各动作频率。返回 `(各动作频率, 总加权组合数)`。未访问的手不计入。
fn range_frequencies(
    hands: &[Hand],
    rows: &CropNodeRows,
    reach: &[f64],
    n_actions: usize,
) -> (Vec<f64>, f64) {
    let mut totals = vec![0.0; n_actions];
    let mut weight = 0.0;
    for hand in hands {
        let r = reach[hand.bucket];
        if r <= 0.0 {
            continue;
        }
        if let Some(p) = rows.average_strategy(hand.bucket) {
            for (t, x) in totals.iter_mut().zip(&p) {
                *t += hand.combos * r * x;
            }
            weight += hand.combos * r;
        }
    }
    if weight > 0.0 {
        for t in &mut totals {
            *t /= weight;
        }
    }
    (totals, weight)
}

/// 按加权组合数取累计访问权重的中位数（范围里一半组合的样本少于它）。
fn weighted_median_visits(hands: &[Hand], rows: &CropNodeRows, reach: &[f64]) -> f64 {
    let mut items: Vec<(f64, f64)> = hands
        .iter()
        .filter(|h| reach[h.bucket] > 0.0)
        .map(|h| (rows.row_total(h.bucket), h.combos * reach[h.bucket]))
        .collect();
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    let half = items.iter().map(|x| x.1).sum::<f64>() / 2.0;
    let mut acc = 0.0;
    for (visits, weight) in items {
        acc += weight;
        if acc >= half {
            return visits;
        }
    }
    0.0
}

/// 沿 `line` 走到决策点并取策略。走不通时停在最长可走前缀。
fn view_node(app: &App, bp: &Blueprint, line: &[&str]) -> Result<Value, String> {
    let read = |id: NodeId| {
        bp.reader
            .read_node(id)
            .map_err(|e| format!("读节点 {id} 失败: {e:?}"))
    };
    let mut rng = ChaCha20Rng::from_seed(0);
    let mut state = GameState::with_rng(&bp.cfg, 0, &mut rng);
    let mut node_id = bp.root;
    let mut reach = vec![vec![1.0_f64; HANDS]; usize::from(SEATS)];
    let mut matched: Vec<String> = Vec::new();
    let mut steps: Vec<Value> = Vec::new();

    for token in line {
        let node = &bp.nodes[&node_id];
        let found = node
            .edges
            .iter()
            .position(|(tag, _)| token_of(*tag) == *token);
        let Some(index) = found else { break };
        let (tag, Next::Preflop(child)) = node.edges[index] else {
            break;
        };
        let actor = usize::from(node.actor);
        check_actor(&state, node.actor)?;
        let rows = read(node_id)?;
        let (freqs, _) = range_frequencies(&app.hands, &rows, &reach[actor], node.edges.len());
        let action = concrete(bp, &state, tag)?;
        let mut step = describe(&action, committed_of(&state, actor));
        step["pos"] = json!(POSITIONS[actor]);
        step["token"] = json!(*token);
        step["freq"] = json!(round_to(freqs[index], 5));
        steps.push(step);

        for hand in &app.hands {
            let r = &mut reach[actor][hand.bucket];
            *r *= rows.average_strategy(hand.bucket).map_or(0.0, |p| p[index]);
        }
        state
            .apply(action.to_concrete())
            .map_err(|e| format!("重放动作 {token} 失败: {e:?}"))?;
        matched.push((*token).to_string());
        node_id = child;
    }

    let node = &bp.nodes[&node_id];
    check_actor(&state, node.actor)?;
    let actor = usize::from(node.actor);
    let rows = read(node_id)?;
    let actor_reach = &reach[actor];
    let (freqs, combos) = range_frequencies(&app.hands, &rows, actor_reach, node.edges.len());
    let actor_committed = committed_of(&state, actor);

    let mut actions = Vec::with_capacity(node.edges.len());
    for (i, (tag, next)) in node.edges.iter().enumerate() {
        let mut a = describe(&concrete(bp, &state, *tag)?, actor_committed);
        a["token"] = json!(token_of(*tag));
        a["freq"] = json!(round_to(freqs[i], 5));
        a["combos"] = json!(round_to(freqs[i] * combos, 2));
        a["next"] = json!(match next {
            Next::Preflop(_) => "preflop",
            Next::Flop => "flop",
            Next::End => "end",
        });
        actions.push(a);
    }

    let mut unvisited = 0;
    let hands: Vec<Value> = app
        .hands
        .iter()
        .map(|h| {
            let r = actor_reach[h.bucket];
            let strategy = rows.average_strategy(h.bucket);
            if strategy.is_none() && r > 0.0 {
                unvisited += 1;
            }
            json!({
                "l": h.label,
                "c": h.combos,
                "r": round_to(r, 5),
                "w": round_to(rows.row_total(h.bucket), 1),
                "s": strategy.map(|p| p.iter().map(|x| round_to(*x, 5)).collect::<Vec<_>>()),
            })
        })
        .collect();

    // 其余人都弃到后面某一座的捷径：沿 Fold 边往下走。
    let mut fold_chain = Vec::new();
    let mut cursor = node_id;
    loop {
        let edge = bp.nodes[&cursor]
            .edges
            .iter()
            .find(|(tag, _)| matches!(tag, AbstractActionTag::Fold));
        let Some((_, Next::Preflop(next))) = edge else {
            break;
        };
        cursor = *next;
        fold_chain.push(POSITIONS[usize::from(bp.nodes[&cursor].actor)]);
    }

    let to_call = state
        .players()
        .iter()
        .map(|p| p.committed_this_round)
        .max()
        .unwrap_or(ChipAmount::ZERO)
        .as_u64()
        - actor_committed.as_u64();
    let seats: Vec<Value> = ACTION_ORDER
        .iter()
        .map(|&seat| {
            let p = &state.players()[seat];
            let status = match p.status {
                PlayerStatus::Folded => "folded",
                PlayerStatus::AllIn => "allin",
                _ => "active",
            };
            json!({
                "pos": POSITIONS[seat],
                "status": status,
                "in_pot_bb": bb(p.committed_total),
                "stack_bb": bb(p.stack),
                "acting": seat == actor,
            })
        })
        .collect();

    Ok(json!({
        "bp": bp.spec.id,
        "line": matched,
        "truncated": matched.len() < line.len(),
        "node": node_id,
        "pos": POSITIONS[actor],
        "pot_bb": bb(state.pot()),
        "to_call_bb": to_call as f64 / CHIPS_PER_BB,
        "stack_bb": bb(state.players()[actor].stack),
        "steps": steps,
        "actions": actions,
        "fold_chain": fold_chain,
        "seats": seats,
        "range": {
            "combos": round_to(combos, 2),
            "unvisited": unvisited,
            "median_visits": round_to(weighted_median_visits(&app.hands, &rows, actor_reach), 1),
        },
        "hands": hands,
    }))
}

fn check_actor(state: &GameState, actor: u8) -> Result<(), String> {
    match state.current_player() {
        Some(seat) if seat.0 == actor => Ok(()),
        other => Err(format!(
            "重放与树不一致：树上轮到座位 {actor}，状态上是 {other:?}"
        )),
    }
}

fn meta(app: &App) -> Value {
    let blueprints: Vec<Value> = app
        .blueprints
        .iter()
        .map(|bp| {
            json!({
                "id": bp.spec.id,
                "label": bp.spec.label,
                "note": bp.spec.note,
                "run": bp.spec.run_dir,
                "updates": bp.reader.update_count(),
                "nodes": bp.reader.num_nodes(),
            })
        })
        .collect();
    json!({
        "blueprints": blueprints,
        "seats": SEATS,
        "stack_bb": STACK_BB,
        "ante_bb": ANTE_CHIPS as f64 / CHIPS_PER_BB,
    })
}

fn load_blueprint(spec: &'static BlueprintSpec, artifacts: &Path) -> Result<Blueprint, String> {
    let started = Instant::now();
    let path: PathBuf = artifacts.join(spec.run_dir).join(spec.crop_file);
    let (abs, rules) = six_max_reshape(spec.reshape, POSTFLOP_CAP)
        .ok_or_else(|| format!("未知 reshape {}", spec.reshape))?;
    let cfg = TableConfig::n_max_at_bb(SEATS, STACK_BB).with_ante(ChipAmount::new(ANTE_CHIPS));
    let tree = PublicBettingTree::build_with_rules(&cfg, &abs, rules);
    eprintln!(
        "[{}] 建树 {} 节点（{:.1}s）",
        spec.id,
        tree.num_nodes(),
        started.elapsed().as_secs_f64()
    );
    let reader = CropStrategyReader::open(&path, &tree, BUCKETS, ANTE_CHIPS, StreetTag::Preflop)
        .map_err(|e| format!("打开 {} 失败: {e:?}", path.display()))?;

    let mut nodes = HashMap::with_capacity(reader.num_nodes());
    for id in 0..tree.num_nodes() as NodeId {
        let node = tree.node(id);
        if node.street != StreetTag::Preflop {
            continue;
        }
        let edges = node
            .legal_actions
            .iter()
            .zip(&node.children)
            .map(|(tag, child)| {
                let next = match *child {
                    Child::Terminal => Next::End,
                    Child::Decision(c) if tree.node(c).street == StreetTag::Preflop => {
                        Next::Preflop(c)
                    }
                    Child::Decision(_) => Next::Flop,
                };
                (*tag, next)
            })
            .collect();
        nodes.insert(
            id,
            PreNode {
                actor: node.player_acting,
                edges,
            },
        );
    }
    let root = tree.root_id();
    eprintln!(
        "[{}] 校验通过：{} 次更新，{} 个翻前节点（累计 {:.1}s）",
        spec.id,
        reader.update_count(),
        nodes.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(Blueprint {
        spec,
        cfg,
        abs,
        reader,
        nodes,
        root,
    })
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    // The peer may have gone away (page refreshed mid-request); nothing useful to do about it.
    let _ = stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(body));
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then_some(v)
    })
}

fn route(app: &App, target: &str) -> (&'static str, &'static str, Vec<u8>) {
    const JSON: &str = "application/json; charset=utf-8";
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let error = |status, msg: String| {
        (
            status,
            JSON,
            json!({ "error": msg }).to_string().into_bytes(),
        )
    };
    match path {
        "/" => (
            "200 OK",
            "text/html; charset=utf-8",
            PAGE.as_bytes().to_vec(),
        ),
        "/api/meta" => ("200 OK", JSON, meta(app).to_string().into_bytes()),
        "/api/node" => {
            let id = query_param(query, "bp").unwrap_or("");
            let Some(bp) = app.blueprints.iter().find(|b| b.spec.id == id) else {
                return error("404 Not Found", format!("没有 blueprint {id:?}"));
            };
            let line: Vec<&str> = query_param(query, "line")
                .unwrap_or("")
                .split(',')
                .filter(|t| !t.is_empty())
                .collect();
            match view_node(app, bp, &line) {
                Ok(v) => ("200 OK", JSON, v.to_string().into_bytes()),
                Err(e) => error("500 Internal Server Error", e),
            }
        }
        _ => error("404 Not Found", format!("没有 {path}")),
    }
}

fn handle(mut stream: TcpStream, app: &App) {
    if stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .is_err()
    {
        return;
    }
    let mut buf = [0u8; 8192];
    let mut used = 0;
    while !buf[..used].windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf[used..]) {
            Ok(0) | Err(_) => return,
            Ok(n) => used += n,
        }
        if used == buf.len() {
            return respond(
                &mut stream,
                "431 Too Large",
                "text/plain",
                b"request too large",
            );
        }
    }
    let request = String::from_utf8_lossy(&buf[..used]);
    let mut first = request.lines().next().unwrap_or("").split_whitespace();
    let (Some("GET"), Some(target)) = (first.next(), first.next()) else {
        return respond(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"GET only",
        );
    };
    let (status, content_type, body) = route(app, target);
    respond(&mut stream, status, content_type, &body);
}

fn run() -> Result<(), String> {
    let (mut host, mut port) = ("127.0.0.1".to_string(), 8790u16);
    let mut artifacts = PathBuf::from("artifacts");
    let mut exit_on_stdin_eof = false;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} 缺参数"));
        match flag.as_str() {
            "--host" => host = value()?,
            "--port" => port = value()?.parse().map_err(|e| format!("--port: {e}"))?,
            "--artifacts" => artifacts = value()?.into(),
            "--exit-on-stdin-eof" => exit_on_stdin_eof = true,
            other => return Err(format!("未知参数 {other}")),
        }
    }
    if exit_on_stdin_eof {
        std::thread::spawn(|| {
            let mut sink = [0u8; 64];
            while matches!(std::io::stdin().read(&mut sink), Ok(n) if n > 0) {}
            std::process::exit(0);
        });
    }
    // Bind first so a busy port fails before the slow tree build.
    let listener =
        TcpListener::bind((host.as_str(), port)).map_err(|e| format!("监听 {host}:{port}: {e}"))?;
    let blueprints = BLUEPRINTS
        .iter()
        .map(|spec| load_blueprint(spec, &artifacts))
        .collect::<Result<Vec<_>, _>>()?;
    let app = Arc::new(App {
        blueprints,
        hands: hand_grid(),
    });
    let addr = listener
        .local_addr()
        .map_err(|e| format!("取监听地址: {e}"))?;
    eprintln!("打开 http://{addr}/");
    for stream in listener.incoming().flatten() {
        let app = Arc::clone(&app);
        std::thread::spawn(move || handle(stream, &app));
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("错误: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_grid_layout_and_combos() {
        let grid = hand_grid();
        assert_eq!(grid.len(), HANDS);
        assert_eq!(grid[0].label, "AA");
        assert_eq!(grid[1].label, "AKs");
        assert_eq!(grid[13].label, "AKo");
        assert_eq!(grid[168].label, "22");
        let total: f64 = grid.iter().map(|h| h.combos).sum();
        assert_eq!(total, 1326.0);
        // Same class whichever suits the hand is dealt in.
        let abstraction = PreflopLossless169::new();
        let aks = [
            Card::new(Rank::King, Suit::Diamonds),
            Card::new(Rank::Ace, Suit::Diamonds),
        ];
        assert_eq!(usize::from(abstraction.hand_class(aks)), grid[1].bucket);
        let ako = [
            Card::new(Rank::Ace, Suit::Clubs),
            Card::new(Rank::King, Suit::Diamonds),
        ];
        assert_eq!(usize::from(abstraction.hand_class(ako)), grid[13].bucket);
    }

    #[test]
    fn bb_label_trims_zeros() {
        assert_eq!(fmt_bb(4.5), "4.5BB");
        assert_eq!(fmt_bb(11.0), "11BB");
        assert_eq!(fmt_bb(199.0), "199BB");
        assert_eq!(fmt_bb(4.25), "4.25BB");
    }
}
