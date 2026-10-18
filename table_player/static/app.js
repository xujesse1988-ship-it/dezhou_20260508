"use strict";

const $ = (id) => document.getElementById(id);
const streetNames = {idle: "等待开局", preflop: "翻前", flop: "翻牌", turn: "转牌", river: "河牌", showdown: "本手结束"};
const actionNames = {fold: "弃牌", check: "过牌", call: "跟注", raise: "加注", bet: "下注", all_in: "全下"};
const suits = {s: "♠", h: "♥", d: "♦", c: "♣"};
let source = null, liveSource = null, following = true, playing = false, useBB = false;
let current = null, timer = null, requestId = 0, controller = null, generation = null;

function element(tag, cls, text) {
  const el = document.createElement(tag);
  if (cls) el.className = cls;
  if (text !== undefined) el.textContent = text;
  return el;
}

function card(code, hidden = false) {
  if (hidden) { const el = element("span", "card back"); el.setAttribute("aria-label", "未公开手牌"); return el; }
  if (!code) { const el = element("span", "card blank", "♠"); el.setAttribute("aria-label", "尚未发牌"); return el; }
  const suit = code.slice(-1), rank = code.slice(0, -1);
  const el = element("span", `card${suit === "h" || suit === "d" ? " red" : ""}`);
  el.dataset.card = code;
  el.setAttribute("aria-label", `${rank}${suits[suit] || ""}`);
  el.append(element("span", "rank", rank === "T" ? "10" : rank), element("span", "suit", suits[suit] || ""));
  return el;
}

function money(value, suffix = false) {
  if (value === null || value === undefined) return "—";
  const bb = current?.state.big_blind || 20;
  return (useBB ? Number((value / bb).toFixed(1)).toLocaleString() : Number(value).toLocaleString()) + (suffix && useBB ? " BB" : "");
}

function position(seat, hero, n, scale = 1) {
  const angle = ((seat - (hero ?? 0) + n) % n) / n * Math.PI * 2;
  const horizontal = window.innerWidth <= 540 ? 39 : 40;
  return {x: 50 - horizontal * Math.sin(angle) * scale, y: 45 + 35 * Math.cos(angle) * scale};
}

function timeValue(value) {
  if (!value) return null;
  const d = new Date(typeof value === "number" ? value * 1000 : value);
  return Number.isNaN(d.getTime()) ? null : d;
}

function renderStatus() {
  if (!current) return;
  const {state: s, meta: m} = current;
  const age = m.mtime ? Math.max(0, Math.floor(Date.now() / 1000 - m.mtime)) : null;
  const fresh = age !== null && age < 30 && s.connection === "connected";
  $("status-dot").className = `status-dot ${fresh && following && !m.demo ? "fresh" : "stale"}`;
  $("status").textContent = m.demo ? "演示回放" : !following ? (playing ? "正在回放" : "回放已暂停") : m.missing ? "等待日志文件" : s.connection === "error" ? "连接错误" : s.connection === "disconnected" ? "连接已关闭" : fresh ? "正在跟随" : "日志暂无更新";
  $("updated").textContent = age === null ? "" : age < 60 ? `${age} 秒前更新` : age < 3600 ? `${Math.floor(age / 60)} 分钟前更新` : `${Math.floor(age / 3600)} 小时前更新`;
  $("follow").classList.toggle("active", following);
  $("follow").textContent = following ? "● 跟随最新" : "↗ 跟随最新";
  $("play").textContent = playing ? "Ⅱ" : "▶";
  $("play").setAttribute("aria-label", playing ? "暂停回放" : "播放回放");
  $("demo-banner").hidden = !m.demo;
  $("return-live").hidden = !liveSource;
}

function renderSeats(s) {
  const seats = document.createDocumentFragment(), wagers = document.createDocumentFragment();
  for (const p of s.players) {
    const hero = p.seat === s.hero_seat, empty = p.status === "empty" && !hero;
    const pos = position(p.seat, s.hero_seat, s.num_seats);
    const el = element("div", `seat${hero ? " hero" : ""}${empty ? " empty" : ""}${p.folded ? " folded" : ""}${s.actor_seat === p.seat ? " acting" : ""}${p.winner ? " winner" : ""}`);
    el.dataset.seat = p.seat;
    el.style.setProperty("--x", `${pos.x}%`); el.style.setProperty("--y", `${pos.y}%`);
    const body = element("div", "seat-body");
    if (empty) {
      body.append(element("div", "seat-number", `SEAT ${p.seat + 1}`), element("div", "empty-label", "空座"));
    } else {
      const hand = element("div", "seat-cards");
      const holes = hero ? s.hole : p.cards;
      for (let i = 0; i < 2; i++) hand.append(card(holes[i], !hero && !holes[i] && p.in_hand));
      // Empty cards only indicate an undealt/waiting player, never invented hole cards.
      if (!hero && !p.in_hand && !holes.length) hand.style.visibility = "hidden";
      el.append(hand);
      body.append(element("span", "seat-position", hero ? "HERO" : `SEAT ${p.seat + 1}`), element("span", "seat-name", p.name || (hero ? s.hero_name || "Hero" : `座位 ${p.seat + 1}`)), element("span", "seat-stack", money(p.stack, true)));
      if (p.seat === s.dealer_seat) { const dealer = element("span", "dealer", "D"); dealer.title = "庄家"; body.append(dealer); }
    }
    el.append(body);
    const action = p.winner ? "获胜" : s.actor_seat === p.seat ? "● 正在行动" : p.folded ? "已弃牌" : p.stack === 0 && p.in_hand ? "ALL IN" : actionNames[p.last_action] || (!p.in_hand && !empty ? "等待发牌" : "");
    el.append(element("div", "seat-action", action)); seats.append(el);
    if (p.bet > 0 && !empty) {
      const at = position(p.seat, s.hero_seat, s.num_seats, .69);
      const wager = element("div", "wager");
      wager.style.setProperty("--x", `${at.x}%`); wager.style.setProperty("--y", `${at.y}%`);
      wager.append(element("span", "chip"), element("span", "", money(p.bet, true))); wagers.append(wager);
    }
  }
  $("seats").replaceChildren(seats); $("wagers").replaceChildren(wagers);
}

function render(data) {
  current = data;
  const {state: s, meta: m} = data;
  $("seat-count").textContent = s.num_seats;
  $("street-title").textContent = streetNames[s.street] || s.street;
  $("street-badge").textContent = streetNames[s.street] || "等待";
  $("blinds").textContent = `${s.small_blind} / ${s.big_blind}`;
  $("table-id").textContent = `TABLE ${s.table_id ? s.table_id.slice(0, 8).toUpperCase() : "—"}`;
  $("table-id").title = s.table_id || "";
  $("hand-id").textContent = `HAND ${s.hand_number ? String(s.hand_number).padStart(3, "0") : "—"} · ${s.hand_id?.slice(0, 8) || "—"}`;
  $("hand-id").title = s.hand_id || "";
  $("pot").replaceChildren(document.createTextNode(money(s.pot) + " "), element("small", "", useBB ? "BB" : "筹码"));
  $("board").replaceChildren(...Array.from({length: 5}, (_, i) => card(s.board[i])));
  let notice = s.notice || (s.actor_seat === s.hero_seat && s.hero_seat !== null ? "轮到你行动" : s.actor_seat !== null ? `${s.players[s.actor_seat]?.name || "玩家"} 行动中` : "等待下一条事件");
  if (s.result?.winners.length) notice = s.result.winners.map(w => w.name || `座位 ${w.seat + 1}`).join(" / ") + " 赢得底池";
  $("table-notice").textContent = notice;
  $("empty-overlay").hidden = Boolean(s.table_id || s.hand_id);
  renderSeats(s);
  const hero = s.players.find(p => p.seat === s.hero_seat);
  $("hero-cards").replaceChildren(card(s.hole[0]), card(s.hole[1]));
  $("hero-name").textContent = hero?.name || s.hero_name || "等待入座";
  $("hero-seat").textContent = s.hero_seat === null ? "尚未识别 Hero" : `座位 ${s.hero_seat + 1} / 索引 ${s.hero_seat}`;
  $("hero-stack").textContent = money(hero?.stack, true);
  $("hero-bet").textContent = money(hero?.bet, true);
  $("hero-status").textContent = hero?.winner ? "本手获胜" : hero?.folded ? "本手已弃牌 · 保留自己的手牌" : s.hero_seat !== null && s.actor_seat === s.hero_seat ? "轮到 Hero 行动" : s.hole.length ? "你的手牌始终可见" : "收到发牌事件后显示手牌";
  $("hero-status").classList.toggle("active", s.hero_seat !== null && s.actor_seat === s.hero_seat);
  const history = document.createDocumentFragment();
  [...s.history].reverse().forEach((item, i) => {
    const li = element("li", item.seat === s.hero_seat ? "hero-row" : "");
    const text = element("span");
    text.append(element("span", "history-name", item.kind === "action" ? s.players[item.seat]?.name || `座位 ${item.seat + 1}` : streetNames[item.street] || "牌局"), element("span", "history-action", actionNames[item.text] || item.text));
    li.append(element("span", "history-number", String(s.history.length - i).padStart(2, "0")), text, element("span", "history-amount", item.amount === null ? "" : money(item.amount, true)));
    history.append(li);
  });
  if (!s.history.length) history.append(element("li", "history-empty", "牌局开始后，行动记录会显示在这里。"));
  $("history").replaceChildren(history);
  $("action-count").textContent = `${s.history.length} 条`;
  $("timeline").max = m.total; $("timeline").value = m.cursor;
  $("cursor-label").textContent = `${m.cursor} / ${m.total}`;
  $("event-time").textContent = timeValue(s.ts)?.toLocaleTimeString("zh-CN", {hour12: false}) || "--:--:--";
  $("prev").disabled = m.cursor <= 0; $("next").disabled = m.cursor >= m.total;
  $("prev-hand").disabled = !m.hand_starts.some(h => h.cursor < m.cursor);
  $("next-hand").disabled = !m.hand_starts.some(h => h.cursor > m.cursor);
  $("play").disabled = m.total === 0;
  $("source-note").textContent = m.invalid_lines ? `已跳过 ${m.invalid_lines} 条无效日志。` : m.partial_line ? "等待当前日志行写入完成。" : "每秒刷新，支持暂停与回放。";
  $("unit").textContent = useBB ? "显示筹码" : "显示 BB";
  renderStatus();
}

function showError(message) { $("error").hidden = !message; $("error").textContent = message || ""; }

async function load(at = "live") {
  if (!source) return;
  controller?.abort(); controller = new AbortController();
  const id = ++requestId;
  try {
    const response = await fetch(`/api/state?source=${encodeURIComponent(source)}&at=${at}`, {signal: controller.signal});
    const data = await response.json();
    if (!response.ok) throw new Error(data.error || "读取日志失败");
    if (id !== requestId) return;
    if (generation !== null && generation !== data.meta.generation && !following) {
      pause(); following = true; generation = data.meta.generation; await load(); return;
    }
    generation = data.meta.generation;
    showError(""); render(data);
  } catch (error) {
    if (error.name !== "AbortError") { showError(`无法读取牌桌：${error.message}`); $("status").textContent = "播放器连接中断"; $("status-dot").className = "status-dot stale"; }
  }
}

function pause() { playing = false; clearTimeout(timer); timer = null; renderStatus(); }

async function choose(id, demo = false) {
  pause(); source = id; generation = null;
  following = !demo;
  $("source").value = id;
  if (!demo) liveSource = id;
  // Open the demo directly at the flop without briefly displaying its future state.
  await load(demo ? 19 : "live");
}

async function refreshSources() {
  try {
    const response = await fetch("/api/sources"), data = await response.json();
    if (!response.ok) throw new Error(data.error || "读取来源失败");
    $("source").replaceChildren(...data.sources.map(s => { const option = element("option", "", s.label); option.value = s.id; return option; }));
    liveSource = liveSource || data.default;
    if (!source && data.default) await choose(data.default);
    else if (source) $("source").value = source;
    else { $("source").prepend(new Option("未找到日志；请选择演示或用 --ws-log 启动", "", true, true)); $("empty-overlay").hidden = false; showError("未找到 WS 日志。可点击“演示牌局”，或启动时指定 --ws-log。"); }
  } catch (error) { showError(`无法获取日志列表：${error.message}`); }
}

async function step(delta) {
  if (!current) return;
  pause(); following = false;
  await load(Math.max(0, Math.min(current.meta.total, current.meta.cursor + delta)));
}

async function jumpHand(delta) {
  if (!current) return;
  const starts = current.meta.hand_starts;
  const target = delta < 0 ? [...starts].reverse().find(h => h.cursor < current.meta.cursor) : starts.find(h => h.cursor > current.meta.cursor);
  if (target) { pause(); following = false; await load(target.cursor); }
}

async function tick() {
  if (!playing || !current) return;
  if (current.meta.cursor >= current.meta.total) { pause(); return; }
  await load(current.meta.cursor + 1);
  if (playing) timer = setTimeout(tick, Number($("speed").value));
}

async function togglePlay() {
  if (playing) { pause(); return; }
  if (!current || !current.meta.total) return;
  following = false; playing = true;
  if (current.meta.cursor >= current.meta.total) await load(current.meta.hand_starts[0]?.cursor || 0);
  renderStatus(); timer = setTimeout(tick, Number($("speed").value));
}

$("source").addEventListener("change", () => choose($("source").value, $("source").value === "demo"));
$("refresh").addEventListener("click", refreshSources);
$("demo").addEventListener("click", () => choose("demo", true));
$("return-live").addEventListener("click", () => { if (liveSource) choose(liveSource); });
$("follow").addEventListener("click", () => { pause(); following = true; load(); });
$("prev").addEventListener("click", () => step(-1));
$("next").addEventListener("click", () => step(1));
$("prev-hand").addEventListener("click", () => jumpHand(-1));
$("next-hand").addEventListener("click", () => jumpHand(1));
$("play").addEventListener("click", togglePlay);
$("timeline").addEventListener("input", () => { pause(); following = false; load(Number($("timeline").value)); });
$("unit").addEventListener("click", () => { useBB = !useBB; if (current) render(current); });
window.addEventListener("resize", () => { if (current) renderSeats(current.state); });
document.addEventListener("keydown", (event) => {
  if (["INPUT", "SELECT", "TEXTAREA"].includes(event.target.tagName)) return;
  if (event.code === "Space" && !["BUTTON", "A"].includes(event.target.tagName)) { event.preventDefault(); togglePlay(); }
  if (event.code === "ArrowLeft") { event.preventDefault(); step(-1); }
  if (event.code === "ArrowRight") { event.preventDefault(); step(1); }
});
setInterval(() => { if (following && !document.hidden) load(); else renderStatus(); }, 1000);
setInterval(() => { if (!source && !document.hidden) refreshSources(); }, 5000);
document.addEventListener("visibilitychange", () => { if (!document.hidden && following) load(); });
refreshSources();
