// Isolated integration fixture: import the sibling service without its .env/accounts.
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';

const project = process.argv[2];
const { createPokerServer } = await import(pathToFileURL(resolve(project, 'src/server.ts')));
const { defaults } = await import(pathToFileURL(resolve(project, 'src/config.ts')));
const { WebSocket } = await import(pathToFileURL(resolve(project, 'node_modules/ws/wrapper.mjs')));
const server = createPokerServer({ ...defaults, port: 0, host: '127.0.0.1',
  betweenHandsMs: 100, handStartDelayMs: 20, actionDelayMs: 5,
  actionTimeoutMs: 10_000,
  players: ['hero', 'bob', 'carol'].map(name => ({ id: name, name,
    token: `test-${name}`, balance: 10000 })) });
const port = await server.listen();
console.log(port);
const bots = [];
const poll = setInterval(() => {
  if (!server.service.accounts.get('hero').tableId) return;
  clearInterval(poll);
  for (const name of ['bob', 'carol']) {
    const ws = new WebSocket(`ws://127.0.0.1:${port}/ws`, {
      headers: { Authorization: `Bearer test-${name}` } });
    bots.push(ws);
    let raisedHand;
    ws.on('error', () => {});
    ws.on('message', raw => {
      const m = JSON.parse(raw.toString());
      if (m.type === 'connected') ws.send(JSON.stringify({ type: 'join_lobby', buy_in: 2000 }));
      if (m.type !== 'your_turn') return;
      const raise = m.valid_actions.find(a => a.action === 'raise');
      // Ensure real incremental raises (including the SB's posted blind) reach the client.
      const chosen = raisedHand !== m.hand_id && m.community_cards.length === 0 && raise
        ? raise : m.valid_actions.find(a => a.action === 'check')
          ?? m.valid_actions.find(a => a.action === 'call') ?? m.valid_actions[0];
      if (chosen.action === 'raise') raisedHand = m.hand_id;
      ws.send(JSON.stringify({ type: 'action', hand_id: m.hand_id, turn_token: m.turn_token,
        client_action_id: m.turn_token, action: chosen.action,
        ...(chosen.action === 'raise' ? { amount: chosen.min } : {}) }));
    });
  }
}, 10);

async function stop() {
  clearInterval(poll);
  for (const bot of bots) bot.close();
  await server.close();
}
process.once('SIGTERM', stop);
process.once('SIGINT', stop);
