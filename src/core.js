// 纯逻辑层：不碰 DOM、不调用 Tauri，供浏览器（app.js）与 node:test 共用。
//
// 放这里的理由：这些规则**成对约束着别处的定义**，一旦漂移就会出现「界面有开关
// 但后端不认」「渠道列表少一条」这类静默失效。抽成无副作用的函数后，可以由
// tests-js/core.test.js 直接与 Rust 侧源码对照（白名单 vs OptionsPatch 字段、
// 渠道表 vs sdk::channel::CHANNELS），把漂移变成红灯而不是线上问题。
(function (root, factory) {
  'use strict';
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  root.GSCore = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  // set_options 的补丁字段白名单：必须与 Rust 侧 OptionsPatch 的字段集合完全一致。
  // 漏一个键 = 该开关静默不落盘（历史上 screen_scan_auto_bg / screen_scan_stop_on_resume /
  // notify_persistent / notify_live_status 四项就是这样失效的）。
  const OPTION_KEYS = [
    'auto_login_last',
    'always_on_top',
    'auto_close',
    'stop_others_on_scan_success',
    'auto_check_update',
    'display_name',
    'screen_scan_auto_bg',
    'screen_scan_stop_on_resume',
    'notify_persistent',
    'notify_live_status',
  ];

  // 只保留白名单内的键；显式传入的 false / 0 / 空字符串必须原样保留（按"键是否存在"判断）。
  function buildOptionsPatch(patch) {
    const args = {};
    if (!patch) return args;
    for (const k of OPTION_KEYS) if (k in patch) args[k] = patch[k];
    return args;
  }

  // 第三方渠道（Android）：与 Rust sdk::channel::CHANNELS 一一对应。
  const CHANNELS_UI = [
    { key: 'xiaomi', label: '小米' },
    { key: 'vivo', label: 'vivo' },
    { key: 'yyb', label: '应用宝' },
    { key: 'wegame', label: 'Wegame' },
    { key: 'huawei', label: '华为' },
    { key: 'oppo', label: 'OPPO' },
    { key: 'uc', label: 'UC（九游）' },
  ];

  function channelUiLabel(key) {
    const c = CHANNELS_UI.find((x) => x.key === key);
    return c ? c.label : key;
  }

  function channelLabel(ch) {
    if (ch === 'hypergryph') return '鹰角';
    if (ch === 'official') return '官服';
    if (ch.startsWith('channel:')) return channelUiLabel(ch.slice(8));
    return 'B 服';
  }

  // 账号稳定键：鹰角用手机号，其余用 uid。
  function acctKey(ch, t) {
    return ch === 'hypergryph' ? t.phone : t.uid;
  }

  // 开播时长文本（H:MM:SS）。nowMs 可注入，便于用固定时钟做断言。
  function fmtDuration(start, nowMs) {
    if (!start || start <= 0) return '';
    const now = (typeof nowMs === 'number' ? nowMs : Date.now()) / 1000;
    const secs = Math.max(0, Math.floor(now) - start);
    const h = Math.floor(secs / 3600), m = Math.floor((secs % 3600) / 60), s = secs % 60;
    const p = (n) => String(n).padStart(2, '0');
    return h + ':' + p(m) + ':' + p(s);
  }

  // 判定「房间不存在」的错误特征：后端 get_info 返回 code=1 时消息为「未找到该房间」，
  // 抖音侧为「房间不存在」。仅这些**终态**错误才落到 missing；网络/风控等瞬时失败
  // 不应误报成「房间不存在」。
  function isRoomMissingError(msg) {
    const s = String(msg || '');
    return /未找到该房间|房间不存在|code=1（|code=1\(/.test(s);
  }

  return {
    OPTION_KEYS, buildOptionsPatch, CHANNELS_UI, channelUiLabel, channelLabel,
    acctKey, fmtDuration, isRoomMissingError,
  };
});
