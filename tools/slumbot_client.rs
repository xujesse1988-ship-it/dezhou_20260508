//! Slumbot API client + always-check/call smoke agent.
//!
//! Slumbot API 规格（来源：<https://slumbot.com/sample_api.py>，2026-05-19 抓取）：
//!
//! - host：`slumbot.com`，3 个 endpoint 全部 HTTPS POST + JSON body：
//!   - `/slumbot/api/login`        — `{username, password}`        → `{token, error_msg?}`
//!   - `/slumbot/api/new_hand`     — `{token?}`                     → `HandResponse`
//!   - `/slumbot/api/act`          — `{token, incr}`                → `HandResponse`
//! - 牌局规格：SB = 50 / BB = 100 / stack = 20,000（200 BB），4 条街，HU NLHE，
//!   每手 stack 重置。
//! - action notation：`k` = check，`c` = call，`f` = fold，`bN` = bet/raise to N
//!   chips on this street（注意：`N` 是**本街累计** put-in，**不是** total pot）；
//!   街之间 `/` 分隔；all-in 后允许尾随空街（如 `b20000c///`）。
//! - `client_pos`：`0` = client 是 BB（preflop 后手 / postflop 先手），
//!   `1` = client 是 SB（preflop 先手 / postflop 后手）。
//! - token：每个响应都可能返回新 token（会话超时后会滚），always 用最新一份。
//!
//! 调用示例：
//!
//! ```bash
//! cargo run --release --bin slumbot_client -- --hands 5
//! cargo run --release --bin slumbot_client -- --username U --password P --hands 100
//! ```
//!
//! 当前 binary 走 sample_api.py 的 naive "always check/call" 策略，仅作 smoke：
//! 验证 HTTPS 连通性、token 流转、`ParseAction` 端口在真实 action 字符串上不报错。
//! 接入项目 blueprint checkpoint 走 inference 是后续 step（cooldown branch 不做）。

use std::time::Duration;

use serde::{Deserialize, Serialize};

const HOST: &str = "slumbot.com";

pub const SMALL_BLIND: u64 = 50;
pub const BIG_BLIND: u64 = 100;
pub const STACK_SIZE: u64 = 20_000;
pub const NUM_STREETS: u32 = 4;

// ============================================================================
// HTTP client
// ============================================================================

#[derive(Debug, Clone)]
pub enum ClientError {
    Http(String),
    Json(String),
    Server(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Http(s) => write!(f, "http: {s}"),
            ClientError::Json(s) => write!(f, "json: {s}"),
            ClientError::Server(s) => write!(f, "server: {s}"),
        }
    }
}

impl std::error::Error for ClientError {}

#[derive(Serialize)]
struct LoginReq<'a> {
    username: &'a str,
    password: &'a str,
}

#[derive(Serialize)]
struct NewHandReq<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<&'a str>,
}

#[derive(Serialize)]
struct ActReq<'a> {
    token: &'a str,
    incr: &'a str,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct HandResponse {
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub old_action: String,
    #[serde(default)]
    pub client_pos: Option<u8>,
    #[serde(default)]
    pub hole_cards: Vec<String>,
    #[serde(default)]
    pub board: Vec<String>,
    #[serde(default)]
    pub bot_hole_cards: Option<Vec<String>>,
    #[serde(default)]
    pub winnings: Option<i64>,
    #[serde(default)]
    pub error_msg: Option<String>,
}

#[derive(Deserialize)]
struct LoginResponse {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    error_msg: Option<String>,
}

pub struct SlumbotClient {
    agent: ureq::Agent,
    pub token: Option<String>,
}

impl Default for SlumbotClient {
    fn default() -> Self {
        Self::new()
    }
}

impl SlumbotClient {
    pub fn new() -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build();
        Self { agent, token: None }
    }

    pub fn login(&mut self, username: &str, password: &str) -> Result<(), ClientError> {
        let url = format!("https://{HOST}/slumbot/api/login");
        let resp: LoginResponse = self.post_json(&url, &LoginReq { username, password })?;
        if let Some(err) = resp.error_msg {
            return Err(ClientError::Server(err));
        }
        let token = resp
            .token
            .ok_or_else(|| ClientError::Server("login: no token in response".into()))?;
        self.token = Some(token);
        Ok(())
    }

    pub fn new_hand(&mut self) -> Result<HandResponse, ClientError> {
        let url = format!("https://{HOST}/slumbot/api/new_hand");
        let req = NewHandReq {
            token: self.token.as_deref(),
        };
        let resp: HandResponse = self.post_json(&url, &req)?;
        self.absorb_token(&resp)?;
        Ok(resp)
    }

    pub fn act(&mut self, incr: &str) -> Result<HandResponse, ClientError> {
        let token = self.token.clone().ok_or_else(|| {
            ClientError::Server("act() requires token; call new_hand() first".into())
        })?;
        let url = format!("https://{HOST}/slumbot/api/act");
        let resp: HandResponse = self.post_json(
            &url,
            &ActReq {
                token: &token,
                incr,
            },
        )?;
        self.absorb_token(&resp)?;
        Ok(resp)
    }

    fn absorb_token(&mut self, resp: &HandResponse) -> Result<(), ClientError> {
        if let Some(err) = &resp.error_msg {
            return Err(ClientError::Server(err.clone()));
        }
        if let Some(t) = &resp.token {
            self.token = Some(t.clone());
        }
        Ok(())
    }

    fn post_json<Req, Resp>(&self, url: &str, body: &Req) -> Result<Resp, ClientError>
    where
        Req: Serialize,
        Resp: for<'de> Deserialize<'de>,
    {
        let payload =
            serde_json::to_value(body).map_err(|e| ClientError::Json(format!("serialize: {e}")))?;
        let resp = self
            .agent
            .post(url)
            .send_json(payload)
            .map_err(|e| ClientError::Http(format!("{url}: {e}")))?;
        resp.into_json::<Resp>()
            .map_err(|e| ClientError::Json(format!("{url}: {e}")))
    }
}

// ============================================================================
// ParseAction 端口（sample_api.py::ParseAction → Rust）
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAction {
    /// 当前所在街 (0 = preflop, 1 = flop, 2 = turn, 3 = river)。
    /// 若返回 `pos == -1` 表示手牌结束，`st` 是最后一条街。
    pub st: u32,
    /// 下一个该 act 的位置：`0` = BB seat，`1` = SB seat，`-1` = 牌局已结束。
    pub pos: i32,
    /// 当前 aggressor 在**本街**累计 put-in 的 chip 数。Street 切换后归零。
    pub street_last_bet_to: u64,
    /// 当前 aggressor 跨所有街累计 put-in 的 chip 数。
    pub total_last_bet_to: u64,
    /// 本街最后一笔 raise 的"增量"大小（用于 min-raise 校验）。
    pub last_bet_size: u64,
    /// 本街最后下注/加注者位置（`-1` 表示本街尚无 aggression，例如 limp 后 check）。
    pub last_bettor: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    IllegalCheck,
    IllegalCall,
    IllegalFold,
    MissingSlash,
    MissingBetSize,
    BetSizeNotInteger,
    BetTooSmall,
    BetTooBig,
    UnexpectedChar(char),
    UnexpectedError,
    ExtraCharsAtEnd,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::IllegalCheck => write!(f, "Illegal check"),
            ParseError::IllegalCall => write!(f, "Illegal call"),
            ParseError::IllegalFold => write!(f, "Illegal fold"),
            ParseError::MissingSlash => write!(f, "Missing slash"),
            ParseError::MissingBetSize => write!(f, "Missing bet size"),
            ParseError::BetSizeNotInteger => write!(f, "Bet size not an integer"),
            ParseError::BetTooSmall => write!(f, "Bet too small"),
            ParseError::BetTooBig => write!(f, "Bet too big"),
            ParseError::UnexpectedChar(c) => write!(f, "Unexpected character in action: {c:?}"),
            ParseError::UnexpectedError => write!(f, "Unexpected error"),
            ParseError::ExtraCharsAtEnd => write!(f, "Extra characters at end of action"),
        }
    }
}

impl std::error::Error for ParseError {}

/// 1:1 端口 sample_api.py 的 `ParseAction`。任何已知合法 Slumbot action 字符串都应
/// 与 Python 实现产出同 `ParsedAction`；任何 Python 检测的非法形式都应返回相同语义的
/// `ParseError`。
pub fn parse_action(action: &str) -> Result<ParsedAction, ParseError> {
    let mut st: u32 = 0;
    let mut street_last_bet_to: u64 = BIG_BLIND;
    let mut total_last_bet_to: u64 = BIG_BLIND;
    let mut last_bet_size: u64 = BIG_BLIND - SMALL_BLIND;
    let mut last_bettor: i32 = 0;
    let mut pos: i32 = 1;

    let bytes = action.as_bytes();
    let sz = bytes.len();
    if sz == 0 {
        return Ok(ParsedAction {
            st,
            pos,
            street_last_bet_to,
            total_last_bet_to,
            last_bet_size,
            last_bettor,
        });
    }

    let mut check_or_call_ends_street = false;
    let mut i = 0usize;
    while i < sz {
        if st >= NUM_STREETS {
            return Err(ParseError::UnexpectedError);
        }
        let c = bytes[i] as char;
        i += 1;
        match c {
            'k' => {
                if last_bet_size > 0 {
                    return Err(ParseError::IllegalCheck);
                }
                if check_or_call_ends_street {
                    if st < NUM_STREETS - 1 && i < sz {
                        if bytes[i] as char != '/' {
                            return Err(ParseError::MissingSlash);
                        }
                        i += 1;
                    }
                    if st == NUM_STREETS - 1 {
                        pos = -1;
                    } else {
                        pos = 0;
                        st += 1;
                    }
                    street_last_bet_to = 0;
                    check_or_call_ends_street = false;
                } else {
                    pos = (pos + 1) % 2;
                    check_or_call_ends_street = true;
                }
            }
            'c' => {
                if last_bet_size == 0 {
                    return Err(ParseError::IllegalCall);
                }
                if total_last_bet_to == STACK_SIZE {
                    // All-in 被 call：允许 0 个 '/' 或刚好补齐到 river 前的所有 '/'。
                    if i != sz {
                        let mut s = st;
                        while s < NUM_STREETS - 1 {
                            if i == sz {
                                return Err(ParseError::MissingSlash);
                            }
                            let ch = bytes[i] as char;
                            i += 1;
                            if ch != '/' {
                                return Err(ParseError::MissingSlash);
                            }
                            s += 1;
                        }
                    }
                    if i != sz {
                        return Err(ParseError::ExtraCharsAtEnd);
                    }
                    st = NUM_STREETS - 1;
                    pos = -1;
                    last_bet_size = 0;
                    return Ok(ParsedAction {
                        st,
                        pos,
                        street_last_bet_to,
                        total_last_bet_to,
                        last_bet_size,
                        last_bettor,
                    });
                }
                if check_or_call_ends_street {
                    if st < NUM_STREETS - 1 && i < sz {
                        if bytes[i] as char != '/' {
                            return Err(ParseError::MissingSlash);
                        }
                        i += 1;
                    }
                    if st == NUM_STREETS - 1 {
                        pos = -1;
                    } else {
                        pos = 0;
                        st += 1;
                    }
                    street_last_bet_to = 0;
                    check_or_call_ends_street = false;
                } else {
                    pos = (pos + 1) % 2;
                    check_or_call_ends_street = true;
                }
                last_bet_size = 0;
                last_bettor = -1;
            }
            'f' => {
                if last_bet_size == 0 {
                    return Err(ParseError::IllegalFold);
                }
                if i != sz {
                    return Err(ParseError::ExtraCharsAtEnd);
                }
                pos = -1;
                return Ok(ParsedAction {
                    st,
                    pos,
                    street_last_bet_to,
                    total_last_bet_to,
                    last_bet_size,
                    last_bettor,
                });
            }
            'b' => {
                let j = i;
                while i < sz && (bytes[i] as char).is_ascii_digit() {
                    i += 1;
                }
                if i == j {
                    return Err(ParseError::MissingBetSize);
                }
                // ASCII digits → 直接 &str 切片安全（不跨 multi-byte char 边界）。
                let new_street_last_bet_to: u64 = action[j..i]
                    .parse()
                    .map_err(|_| ParseError::BetSizeNotInteger)?;
                let new_last_bet_size = new_street_last_bet_to.saturating_sub(street_last_bet_to);
                let remaining = STACK_SIZE.saturating_sub(total_last_bet_to);
                let mut min_bet_size = if last_bet_size > 0 {
                    last_bet_size.max(BIG_BLIND)
                } else {
                    BIG_BLIND
                };
                if min_bet_size > remaining {
                    min_bet_size = remaining;
                }
                if new_last_bet_size < min_bet_size {
                    return Err(ParseError::BetTooSmall);
                }
                if new_last_bet_size > remaining {
                    return Err(ParseError::BetTooBig);
                }
                last_bet_size = new_last_bet_size;
                street_last_bet_to = new_street_last_bet_to;
                total_last_bet_to += last_bet_size;
                last_bettor = pos;
                pos = (pos + 1) % 2;
                check_or_call_ends_street = true;
            }
            other => return Err(ParseError::UnexpectedChar(other)),
        }
    }

    Ok(ParsedAction {
        st,
        pos,
        street_last_bet_to,
        total_last_bet_to,
        last_bet_size,
        last_bettor,
    })
}

// ============================================================================
// Smoke agent — always check / call
// ============================================================================

#[derive(Debug, Clone, Copy)]
struct Args {
    username: Option<&'static str>,
    password: Option<&'static str>,
    hands: u32,
}

fn parse_argv() -> Args {
    // 不引入 clap，避免给 smoke binary 加新依赖。
    let mut username: Option<String> = None;
    let mut password: Option<String> = None;
    let mut hands: u32 = 100;
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--username" => username = iter.next(),
            "--password" => password = iter.next(),
            "--hands" => {
                hands = iter
                    .next()
                    .and_then(|s| s.parse().ok())
                    .expect("--hands needs a u32 argument");
            }
            other => {
                eprintln!("[slumbot_client] unknown flag: {other}");
                std::process::exit(2);
            }
        }
    }
    // Leak Strings to satisfy 'static lifetime — args 只解析一次，进程生命期内常驻。
    Args {
        username: username.map(|s| &*Box::leak(s.into_boxed_str())),
        password: password.map(|s| &*Box::leak(s.into_boxed_str())),
        hands,
    }
}

fn play_hand(client: &mut SlumbotClient, hand_idx: u32) -> Result<i64, Box<dyn std::error::Error>> {
    let mut r = client.new_hand()?;
    println!("=== hand {hand_idx} ===");
    println!(
        "  token       : {}",
        client.token.as_deref().unwrap_or("(none)")
    );
    loop {
        println!("  action      : {:?}", r.action);
        if let Some(pos) = r.client_pos {
            println!(
                "  client_pos  : {pos}  ({})",
                if pos == 0 { "BB" } else { "SB" }
            );
        }
        println!("  hole_cards  : {:?}", r.hole_cards);
        println!("  board       : {:?}", r.board);
        if let Some(w) = r.winnings {
            println!("  winnings    : {w}");
            if let Some(b) = &r.bot_hole_cards {
                println!("  bot_hole    : {b:?}");
            }
            return Ok(w);
        }
        let a = parse_action(&r.action)
            .map_err(|e| format!("parse_action({:?}) failed: {e}", r.action))?;
        // sample_api.py naive policy：有 outstanding bet 就 call，否则 check。
        let incr = if a.last_bettor != -1 { "c" } else { "k" };
        println!("  → sending   : {incr}");
        r = client.act(incr)?;
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_argv();
    let mut client = SlumbotClient::new();
    if let (Some(u), Some(p)) = (args.username, args.password) {
        client.login(u, p)?;
        println!("[slumbot_client] logged in as {u}");
    } else {
        println!("[slumbot_client] playing as guest (no --username/--password)");
    }
    let mut total: i64 = 0;
    for h in 0..args.hands {
        let w = play_hand(&mut client, h)?;
        total += w;
        println!("  running total: {total} chips after {} hands", h + 1);
    }
    println!(
        "[slumbot_client] total winnings over {} hands: {total} chips",
        args.hands
    );
    Ok(())
}

// ============================================================================
// ParseAction 单元测试（本地 cargo test 可跑，不依赖网络）
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(a: &str) -> ParsedAction {
        parse_action(a).expect("expected legal action")
    }

    #[test]
    fn empty_action_means_sb_to_act_preflop_facing_bb() {
        let a = ok("");
        assert_eq!(a.st, 0);
        assert_eq!(a.pos, 1, "SB acts first preflop");
        assert_eq!(a.street_last_bet_to, BIG_BLIND);
        assert_eq!(a.total_last_bet_to, BIG_BLIND);
        assert_eq!(a.last_bet_size, BIG_BLIND - SMALL_BLIND);
        assert_eq!(
            a.last_bettor, 0,
            "BB is implicit aggressor due to forced blind"
        );
    }

    #[test]
    fn limp_then_bb_to_act() {
        // SB limp（c），轮到 BB。还在 preflop（pre-river check 才 ends street）。
        let a = ok("c");
        assert_eq!(a.st, 0);
        assert_eq!(a.pos, 0, "BB to act after SB limp");
        assert_eq!(a.last_bet_size, 0);
        assert_eq!(a.last_bettor, -1, "last_bettor cleared by call");
    }

    #[test]
    fn limp_check_advances_to_flop() {
        let a = ok("ck");
        assert_eq!(a.st, 1, "flop");
        assert_eq!(a.pos, 0, "BB acts first postflop");
        assert_eq!(a.street_last_bet_to, 0);
        assert_eq!(a.last_bet_size, 0);
    }

    #[test]
    fn open_bet_to_200_preflop() {
        let a = ok("b200");
        assert_eq!(a.st, 0);
        assert_eq!(a.pos, 0, "BB to act after SB raise");
        assert_eq!(a.street_last_bet_to, 200);
        assert_eq!(a.total_last_bet_to, 200);
        assert_eq!(
            a.last_bet_size, 100,
            "raise size = 200 - SB-already-in-pot-100"
        );
        assert_eq!(a.last_bettor, 1);
    }

    #[test]
    fn full_runout_river_bet_facing_bb() {
        // 文档示例：`b200c/kk/kk/kb200` — preflop SB raise 被 call，flop/turn 双 check，
        // 河面 BB(pos=0) check 后 SB(pos=1) 下 200，下一个该 act 的是 BB(pos=0)。
        let a = ok("b200c/kk/kk/kb200");
        assert_eq!(a.st, 3, "river");
        assert_eq!(a.pos, 0, "BB to act facing river bet from SB");
        assert_eq!(a.street_last_bet_to, 200);
        assert_eq!(a.last_bet_size, 200);
        assert_eq!(a.last_bettor, 1, "SB just bet on river");
    }

    #[test]
    fn allin_call_jumps_to_river_with_trailing_slashes() {
        // 文档示例：`b20000c///` — 全压 + call，街位被 '/' 补齐。
        let a = ok("b20000c///");
        assert_eq!(a.st, NUM_STREETS - 1, "all-in call jumps to river");
        assert_eq!(a.pos, -1, "hand over");
        assert_eq!(a.total_last_bet_to, STACK_SIZE);
        assert_eq!(a.last_bet_size, 0);
    }

    #[test]
    fn allin_call_without_trailing_slashes_also_legal() {
        let a = ok("b20000c");
        assert_eq!(a.st, NUM_STREETS - 1);
        assert_eq!(a.pos, -1);
        assert_eq!(a.total_last_bet_to, STACK_SIZE);
    }

    #[test]
    fn fold_ends_hand() {
        let a = ok("f");
        assert_eq!(a.pos, -1);
        assert_eq!(a.st, 0);
    }

    #[test]
    fn illegal_check_when_facing_bet() {
        // 开局 SB 面对 BB 的盲注（last_bet_size = 50），check 不合法。
        assert_eq!(parse_action("k"), Err(ParseError::IllegalCheck));
    }

    #[test]
    fn illegal_call_when_no_bet() {
        // limp + check → flop。flop 开局没有 outstanding bet，call 不合法。
        assert_eq!(parse_action("ck/c"), Err(ParseError::IllegalCall));
    }

    #[test]
    fn missing_bet_size_after_b() {
        assert_eq!(parse_action("b"), Err(ParseError::MissingBetSize));
    }

    #[test]
    fn bet_below_min_raise_rejected() {
        // SB 已经放 50，已有 last_bet_size = 50；raise 必须至少 +100（BB），即 to ≥ 200。
        // to=150 等价于 raise size 50 < min(100) → 拒。
        assert_eq!(parse_action("b150"), Err(ParseError::BetTooSmall));
    }

    #[test]
    fn bet_above_stack_rejected() {
        assert_eq!(parse_action("b20001"), Err(ParseError::BetTooBig));
    }

    #[test]
    fn unknown_char_rejected() {
        assert_eq!(parse_action("x"), Err(ParseError::UnexpectedChar('x')));
    }

    #[test]
    fn three_bet_pot_preflop() {
        // SB b200 → BB b600 → SB to act, facing raise.
        let a = ok("b200b600");
        assert_eq!(a.st, 0);
        assert_eq!(a.pos, 1);
        assert_eq!(a.street_last_bet_to, 600);
        assert_eq!(a.total_last_bet_to, 600);
        assert_eq!(a.last_bet_size, 400);
        assert_eq!(a.last_bettor, 0);
    }
}
