// 前端纯逻辑单测（node --test，无第三方依赖）。
//
// 重点不是"跑通函数"，而是**守住跨语言的一致性**：
//   - core.js 的 set_options 白名单 == Rust OptionsPatch 的字段集合
//   - core.js 的渠道表 == Rust sdk::channel::CHANNELS（key 与 label 都要一致）
// 这两处历史上都出现过漂移（开关静默不落盘 / 渠道列表少一条），且都不会被编译期发现。
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const core = require('../src/core.js');
const root = path.resolve(__dirname, '..');

function walk(dir, ext, out) {
  for (const ent of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, ent.name);
    if (ent.isDirectory()) walk(p, ext, out);
    else if (ent.name.endsWith(ext)) out.push(p);
  }
  return out;
}

// 取结构体/常量末尾的配对花括号（够用：Rust 源码里字段值不含裸花括号）。
function blockAfter(src, marker) {
  const at = src.indexOf(marker);
  if (at < 0) return null;
  const open = src.indexOf('{', at);
  let depth = 0;
  for (let i = open; i < src.length; i += 1) {
    if (src[i] === '{') depth += 1;
    else if (src[i] === '}') {
      depth -= 1;
      if (depth === 0) return src.slice(open + 1, i);
    }
  }
  return null;
}

test('buildOptionsPatch 只保留白名单键，且保留 false/0/空串', () => {
  const out = core.buildOptionsPatch({
    auto_close: false,
    display_name: '',
    notify_persistent: 0,
    not_an_option: 'x',
    __proto__: null,
  });
  assert.deepEqual(Object.keys(out).sort(), ['auto_close', 'display_name', 'notify_persistent']);
  assert.equal(out.auto_close, false);
  assert.equal(out.display_name, '');
  assert.equal(out.notify_persistent, 0);
});

test('buildOptionsPatch 对空 patch 返回空对象', () => {
  assert.deepEqual(core.buildOptionsPatch(), {});
  assert.deepEqual(core.buildOptionsPatch(null), {});
  assert.deepEqual(core.buildOptionsPatch({}), {});
});

test('OPTION_KEYS 与 Rust OptionsPatch 字段集合完全一致', () => {
  const files = walk(path.join(root, 'src-tauri', 'src'), '.rs', []);
  let fields = null;
  for (const f of files) {
    const src = fs.readFileSync(f, 'utf8');
    if (!src.includes('struct OptionsPatch')) continue;
    const body = blockAfter(src, 'struct OptionsPatch');
    assert.ok(body, '无法解析 OptionsPatch 结构体：' + f);
    fields = [...body.matchAll(/^\s*(?:pub\s+)?([a-z_][a-z0-9_]*)\s*:/gm)].map((m) => m[1]);
    break;
  }
  assert.ok(fields, '在 src-tauri/src 下找不到 OptionsPatch 结构体');
  assert.deepEqual(
    [...core.OPTION_KEYS].sort(),
    [...new Set(fields)].sort(),
    '前端白名单与后端字段不一致：多出的开关会被后端忽略，少一个则前端永远发不出去',
  );
});

test('CHANNELS_UI 与 Rust CHANNELS 表一致（key + 展示名 + 条数）', () => {
  const src = fs.readFileSync(path.join(root, 'src-tauri', 'src', 'sdk', 'channel.rs'), 'utf8');
  const at = src.indexOf('pub const CHANNELS');
  assert.ok(at >= 0, 'sdk/channel.rs 里找不到 pub const CHANNELS');
  const end = src.indexOf('];', at);
  const body = src.slice(at, end);
  // 逐条 ChannelInfo 的 key/label 顺序固定（结构体字面量），直接按出现顺序配对。
  const rust = [...body.matchAll(/key:\s*"([^"]+)"[\s\S]*?label:\s*"([^"]+)"/g)]
    .map((m) => ({ key: m[1], label: m[2] }));
  assert.ok(rust.length > 0, 'CHANNELS 表里没解析到条目');
  assert.equal(core.CHANNELS_UI.length, rust.length, '前端渠道条数与后端不一致');
  const toMap = (arr) => Object.fromEntries(arr.map((c) => [c.key, c.label]));
  assert.deepEqual(toMap(core.CHANNELS_UI), toMap(rust));
});

test('channelLabel / channelUiLabel 的展示规则', () => {
  assert.equal(core.channelLabel('official'), '官服');
  assert.equal(core.channelLabel('bilibili'), 'B 服');
  assert.equal(core.channelLabel('hypergryph'), '鹰角');
  assert.equal(core.channelLabel('channel:uc'), 'UC（九游）');
  assert.equal(core.channelLabel('channel:nope'), 'nope');
  assert.equal(core.channelUiLabel('xiaomi'), '小米');
});

test('acctKey：鹰角用手机号，其余用 uid', () => {
  assert.equal(core.acctKey('hypergryph', { phone: '13800000000', uid: 'u1' }), '13800000000');
  assert.equal(core.acctKey('official', { phone: '13800000000', uid: 'u1' }), 'u1');
});

test('fmtDuration：固定时钟下的 H:MM:SS 与边界', () => {
  const now = Date.UTC(2026, 0, 1, 12, 0, 0);
  const start = Math.floor(now / 1000) - 3725;
  assert.equal(core.fmtDuration(start, now), '1:02:05');
  assert.equal(core.fmtDuration(Math.floor(now / 1000) - 65, now), '0:01:05');
  assert.equal(core.fmtDuration(Math.floor(now / 1000) + 60, now), '0:00:00', '未来时间应钳到 0');
  assert.equal(core.fmtDuration(0, now), '');
  assert.equal(core.fmtDuration(-1, now), '');
});

test('isRoomMissingError 只认终态错误', () => {
  assert.equal(core.isRoomMissingError('未找到该房间'), true);
  assert.equal(core.isRoomMissingError('房间不存在'), true);
  assert.equal(core.isRoomMissingError('get_info 失败：code=1（未找到该房间）'), true);
  assert.equal(core.isRoomMissingError('code=1(room missing)'), true);
  assert.equal(core.isRoomMissingError('网络请求超时'), false);
  assert.equal(core.isRoomMissingError(''), false);
  assert.equal(core.isRoomMissingError(undefined), false);
});

test('index.html 先加载 core.js 再加载 app.js', () => {
  const html = fs.readFileSync(path.join(root, 'src', 'index.html'), 'utf8');
  const coreAt = html.indexOf('src="core.js"');
  const appAt = html.indexOf('src="app.js"');
  assert.ok(coreAt >= 0, 'index.html 没有引用 core.js');
  assert.ok(coreAt < appAt, 'core.js 必须在 app.js 之前加载');
});
