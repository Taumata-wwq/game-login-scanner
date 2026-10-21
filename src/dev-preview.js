// 浏览器预览用 mock：仅在非 Tauri 环境生效，注入 window.__TAURI__ 以驱动 UI。
// 生产（Tauri）环境不会加载到此脚本（见 index.html head 的条件注入）。
(function () {
  if (window.__TAURI__ || window.__TAURI_INTERNALS__) return;
  if (!/^(file:|https?:\/\/(localhost|127\.0\.0\.1))/.test(location.href) && !location.search.match(/preview/)) {
    // 非本地打开也允许（方便静态托管预览）。
  }
  console.log("[dev-preview] 未检测到 Tauri，启用 mock");

  var now = Math.floor(Date.now() / 1000);
  var cfg = {
    bili_accounts: [
      { uid: "10001", access_key: "x", uname: "B站用户一号", game: "bh3", alias: "", remark: "备用", last_login_succ: true, last_used_at: now },
      { uid: "10002", access_key: "x", uname: "B站用户二号", game: "bh3", alias: "小号", remark: "", last_login_succ: true, last_used_at: now - 100 }
    ],
    official_accounts: [
      { uid: "20001", stoken: "x", mid: "20001", uname: "米游社主号", game: "bh3", alias: "", remark: "主用", last_login_succ: true, last_used_at: now }
    ],
    hypergryph_accounts: [],
    channel_accounts: [],
    active_channel: {},
    current_game: "bh3",
    active_bili: { bh3: "10001" },
    active_official: { bh3: "20001" },
    active_hypergryph: {},
    game_channel: { bh3: "official" },
    display_name: "",
    auto_login_last: false,
    always_on_top: false,
    auto_close: false,
    display_fps: 60,
    win_x: null, win_y: null, win_w: null, win_h: null,
    last_live_id: "",
    sidebar_w: null, game_col_w: null,
    stop_others_on_scan_success: true,
    auto_check_update: false,
    device_id: "preview-device",
    device_fp: "", fp_seed_id: "", fp_seed_time: "",
    live_history: [
      { room: "21001", platform: "bilibili", note: "测试主播A", last_used: now, title: "崩坏3直播中", nickname: "主播A", cover: "", room_id: "21001", live_status: 1, live_start: now - 3600, last_checked: now },
      { room: "21002", platform: "bilibili", note: "", last_used: now - 500, title: "未开播房间", nickname: "主播B", cover: "", room_id: "21002", live_status: 0, live_start: 0, last_checked: now }
    ]
  };

  var listeners = {};
  function emit(name, payload) { (listeners[name] || []).forEach(function (fn) { try { fn({ payload: payload }); } catch (e) {} }); }
  function clone(o) { return JSON.parse(JSON.stringify(o)); }
  function nowSec() { return Math.floor(Date.now() / 1000); }
  function reject(msg) { return Promise.reject(msg); }

  // --- 有状态 mock：让「选账号 / 加直播间 / 登录加号」在预览里真生效 ---
  var captureOn = false;          // 屏幕捕获授权状态（request_screen_capture 后为 true）
  function acctList(ch) {
    if (ch === "bilibili") return cfg.bili_accounts;
    if (ch === "official") return cfg.official_accounts;
    return cfg.hypergryph_accounts;
  }
  // 与后端一致：「当前账号」全局唯一——选中官服/B 服时清掉渠道账号选择。
  function setActive(ch, game, key) {
    if (ch === "bilibili") cfg.active_bili[game] = key;
    else if (ch === "official") cfg.active_official[game] = key;
    else cfg.active_hypergryph[game] = key;
    cfg.game_channel[game] = ch;
    if (cfg.active_channel) delete cfg.active_channel[game];
  }
  function liveIndex(platform, room) {
    for (var i = 0; i < cfg.live_history.length; i++) {
      if (cfg.live_history[i].platform === platform && cfg.live_history[i].room === room) return i;
    }
    return -1;
  }
  function liveUpsert(platform, room, note) {
    var i = liveIndex(platform, room);
    if (i >= 0) {
      cfg.live_history[i].last_used = nowSec();
      if (note) cfg.live_history[i].note = note;
      return cfg.live_history[i];
    }
    var rec = {
      room: room, platform: platform, note: note || "", last_used: nowSec(),
      title: "", nickname: "", cover: "", room_id: room,
      live_status: 0, live_start: 0, last_checked: nowSec(),
    };
    cfg.live_history.unshift(rec);
    return rec;
  }
  // 登录成功：新增（或更新）一个账号并设为当前。
  function loginUpsert(ch, game, key, uname) {
    var list = acctList(ch);
    var idKey = ch === "hypergryph" ? "phone" : "uid";
    var found = null;
    for (var i = 0; i < list.length; i++) {
      if (list[i][idKey] === key && list[i].game === game) { found = list[i]; break; }
    }
    if (!found) {
      found = ch === "bilibili"
        ? { uid: key, access_key: "x", uname: uname, game: game, alias: "", remark: "" }
        : ch === "official"
        ? { uid: key, stoken: "x", mid: key, uname: uname, game: game, alias: "", remark: "" }
        : { phone: key, game: game, alias: "", remark: "" };
      list.push(found);
    }
    found.last_used_at = nowSec();
    found.last_login_succ = true;
    setActive(ch, game, key);
    return found;
  }

  // 模拟预览帧（32x… 320x180 棋盘格 + 缓慢移动亮块，供预览区渲染验证）。
  var frameTick = 0;
  function previewFrame() {
    var w = 320, h = 180;
    var buf = new ArrayBuffer(24 + w * h * 4);
    var dv = new DataView(buf);
    dv.setUint32(0, w, true);
    dv.setUint32(4, h, true);
    dv.setInt32(8, 0, true);
    dv.setInt32(12, 0, true);
    dv.setInt32(16, 1920, true);
    dv.setInt32(20, 1080, true);
    var px = new Uint8ClampedArray(buf, 24);
    // 测试用开关：冻结动画后每帧内容完全一致，可用画布哈希判定「平移/缩放是否真的生效」
    // （否则动画本身就会让哈希变化，验证不可信）。
    if (!window.__freezePreview) frameTick++;
    var blockX = (frameTick * 3) % (w + 24) - 12;
    for (var y = 0; y < h; y++) {
      for (var x = 0; x < w; x++) {
        var i = (y * w + x) * 4;
        var v = (((x >> 4) + (y >> 4)) & 1) ? 70 : 24;
        if (x >= blockX && x < blockX + 24 && y > 76 && y < 104) v = 180;
        px[i] = v; px[i + 1] = v; px[i + 2] = v + 10; px[i + 3] = 255;
      }
    }
    return buf;
  }

  window.__TAURI__ = {
    core: {
      invoke: function (cmd, args) {
        console.log("[mock invoke]", cmd, args || "");
        switch (cmd) {
          case "get_config": return Promise.resolve(clone(cfg));
          case "get_app_version": return Promise.resolve("0.3.2-preview");
          case "set_options":
            if (args && args.patch) { for (var k in args.patch) cfg[k] = args.patch[k]; }
            return Promise.resolve();
          case "switch_game": if (args) cfg.current_game = args.game; return Promise.resolve();
          case "refresh_official_unames": return Promise.resolve();
          case "frontend_log": return Promise.resolve();
          case "check_update": return Promise.resolve();
          case "start_scan": emit("scan-status", { type: "started", target: args && args.target, message: "开始扫码（预览）" }); return Promise.resolve();
          case "stop_scan": emit("scan-status", { type: "stopped", target: args && args.target, message: "已停止（预览）" }); return Promise.resolve();
          case "screen_capture_state": return Promise.resolve(captureOn);
          case "request_screen_capture": captureOn = true; return Promise.resolve();
          case "stop_screen_capture": captureOn = false; return Promise.resolve();
          case "capture_preview": return Promise.resolve(previewFrame());
          case "capture_live_frame": return Promise.resolve(previewFrame());
          case "login_qr_start": return Promise.resolve(["preview", '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 29 29" shape-rendering="crispEdges" style="width:100%;height:100%"><rect width="29" height="29" fill="#fff"/><rect x="0" y="0" width="7" height="7" fill="#000"/><rect x="22" y="0" width="7" height="7" fill="#000"/><rect x="0" y="22" width="7" height="7" fill="#000"/><rect x="2" y="2" width="3" height="3" fill="#fff"/><rect x="24" y="2" width="3" height="3" fill="#fff"/><rect x="2" y="24" width="3" height="3" fill="#fff"/><rect x="10" y="4" width="2" height="2" fill="#000"/><rect x="14" y="8" width="2" height="2" fill="#000"/><rect x="18" y="4" width="2" height="2" fill="#000"/><rect x="10" y="12" width="2" height="2" fill="#000"/><rect x="16" y="14" width="2" height="2" fill="#000"/><rect x="12" y="18" width="2" height="2" fill="#000"/><rect x="18" y="18" width="2" height="2" fill="#000"/><rect x="4" y="10" width="2" height="2" fill="#000"/><rect x="4" y="14" width="2" height="2" fill="#000"/><rect x="24" y="10" width="2" height="2" fill="#000"/><rect x="24" y="16" width="2" height="2" fill="#000"/><rect x="10" y="24" width="2" height="2" fill="#000"/><rect x="16" y="24" width="2" height="2" fill="#000"/></svg>']);
          case "login_qr_poll": return Promise.resolve(["waiting", "official", "", ""]);
          case "login_qr_cancel": return Promise.resolve();
          // --- 直播间：真实写入 live_history，会话带 id（与后端 SessionInfo 同构）---
          case "live_start": {
            var id = (args.platform || "bilibili") + ":" + args.room;
            var rec = liveUpsert(args.platform || "bilibili", args.room, "");
            rec.live_status = 1;
            rec.live_start = nowSec();
            rec.title = rec.title || "预览直播间 " + args.room;
            rec.nickname = rec.nickname || "预览主播";
            return Promise.resolve({
              id: id, room: args.room, platform: args.platform || "bilibili",
              w: 1280, h: 720, title: rec.title, nickname: rec.nickname,
              cover: "", room_id: args.room, duration: "0:00:00", status: "live",
            });
          }
          case "live_stop": return Promise.resolve();
          case "live_refresh_meta":
            return Promise.resolve({
              title: "预览直播间 " + (args && args.room), nickname: "预览主播",
              cover: "", room_id: (args && args.room) || "", live_status: 1,
            });
          case "live_set_active": return Promise.resolve();
          case "live_history_push":
            if (!args || !args.room) return reject("房间号为空");
            liveUpsert(args.platform || "bilibili", args.room, (args && args.note) || "");
            return Promise.resolve();
          case "live_history_remove": {
            var ri = liveIndex(args.platform || "bilibili", args.room);
            if (ri >= 0) cfg.live_history.splice(ri, 1);
            return Promise.resolve();
          }
          case "live_history_set_note": {
            var ni = liveIndex(args.platform || "bilibili", args.room);
            if (ni >= 0) cfg.live_history[ni].note = (args && args.note) || "";
            return Promise.resolve();
          }
          // 排序：前端传完整有序的 "platform:room" 列表。
          case "live_history_reorder": {
            var byId = {};
            cfg.live_history.forEach(function (r) { byId[r.platform + ":" + r.room] = r; });
            var out = [];
            ((args && args.order) || []).forEach(function (k) {
              if (byId[k]) { out.push(byId[k]); delete byId[k]; }
            });
            for (var kk in byId) out.push(byId[kk]);
            cfg.live_history = out;
            return Promise.resolve();
          }
          // --- 账号：选择 / 删除 / 备注 全部落盘到 mock cfg ---
          case "select_account": {
            var g = (args && args.game) || cfg.current_game;
            // 第三方渠道账号（channel 形如 "channel:xiaomi"）：选择存 active_channel。
            if (args && String(args.channel).indexOf("channel:") === 0) {
              var ckey = String(args.channel).slice(8);
              var hit2 = (cfg.channel_accounts || []).some(function (t) {
                return t.channel === ckey && t.game === g && t.uid === args.uid;
              });
              if (!hit2) return reject("账号不存在");
              cfg.active_channel = cfg.active_channel || {};
              cfg.active_channel[g] = { channel: ckey, uid: args.uid };
              // 与后端一致：选中渠道账号时清掉 B服/官服渠道选择（两种指向不能并存）。
              cfg.game_channel[g] = null;
              return Promise.resolve();
            }
            var list = acctList(args.channel);
            var key = args.uid;
            var hit = list.some(function (t) { return t.game === g && t.uid === key; });
            if (!hit) return reject("账号不存在");
            setActive(args.channel, g, key);
            return Promise.resolve();
          }
          case "hypergryph_select": {
            var gh = (args && args.game) || cfg.current_game;
            var hp = args.phone;
            if (!cfg.hypergryph_accounts.some(function (t) { return t.game === gh && t.phone === hp; })) return reject("账号不存在");
            setActive("hypergryph", gh, hp);
            return Promise.resolve();
          }
          case "delete_account": {
            var dg = (args && args.game) || cfg.current_game;
            var dl = acctList(args.channel);
            var di = dl.findIndex(function (t) { return t.game === dg && t.uid === args.uid; });
            if (di < 0) return reject("账号不存在");
            dl.splice(di, 1);
            var am = args.channel === "bilibili" ? cfg.active_bili : cfg.active_official;
            if (am[dg] === args.uid) delete am[dg];
            return Promise.resolve();
          }
          case "hypergryph_delete": {
            var hg = (args && args.game) || cfg.current_game;
            var hi = cfg.hypergryph_accounts.findIndex(function (t) { return t.game === hg && t.phone === args.phone; });
            if (hi < 0) return reject("账号不存在");
            cfg.hypergryph_accounts.splice(hi, 1);
            if (cfg.active_hypergryph[hg] === args.phone) delete cfg.active_hypergryph[hg];
            return Promise.resolve();
          }
          case "set_account_meta": {
            var mg = (args && args.game) || cfg.current_game;
            var mt = acctList(args.channel).find(function (t) { return t.game === mg && t.uid === args.uid; });
            if (!mt) return reject("账号不存在");
            if (args.alias !== undefined) mt.alias = args.alias;
            if (args.remark !== undefined) mt.remark = args.remark;
            return Promise.resolve();
          }
          case "hypergryph_set_meta": {
            var tg = (args && args.game) || cfg.current_game;
            var tt = cfg.hypergryph_accounts.find(function (t) { return t.game === tg && t.phone === args.phone; });
            if (!tt) return reject("账号不存在");
            if (args.alias !== undefined) tt.alias = args.alias;
            if (args.remark !== undefined) tt.remark = args.remark;
            return Promise.resolve();
          }
          // 排序：后端语义为「给出该游戏该渠道的完整有序 uid/phone 列表」。
          case "reorder_accounts": case "hypergryph_reorder": {
            var rc = args.channel === "bilibili" ? "bili_accounts" : args.channel === "official" ? "official_accounts" : "hypergryph_accounts";
            var rk = args.channel === "hypergryph" ? "phone" : "uid";
            var rg = (args && args.game) || cfg.current_game;
            var order = (args && args.order) || [];
            var mine = cfg[rc].filter(function (t) { return t.game === rg; });
            var rest = cfg[rc].filter(function (t) { return t.game !== rg; });
            var byKey = {};
            mine.forEach(function (t) { byKey[t[rk]] = t; });
            var sorted = [];
            order.forEach(function (k) { if (byKey[k]) { sorted.push(byKey[k]); delete byKey[k]; } });
            mine.forEach(function (t) { if (byKey[t[rk]]) { sorted.push(t); delete byKey[t[rk]]; } });
            cfg[rc] = sorted.concat(rest);
            return Promise.resolve();
          }
          // --- 登录：预览环境下直接"成功"，真实添加一个账号 ---
          case "cred_send_code": return Promise.resolve({ status: "ok" });
          case "hypergryph_send_code": return Promise.resolve({ status: "ok" });
          case "cred_login_sms": case "cred_login_pwd": {
            var lg = (args && args.game) || cfg.current_game;
            var lch = args.channel === "bilibili" ? "bilibili" : "official";
            var lkey = (args && (args.account || args.phone)) || String(90000 + Math.floor(Math.random() * 9999));
            var lu = loginUpsert(lch, lg, lkey, (lch === "bilibili" ? "B服预览号 " : "官服预览号 ") + lkey);
            console.log("[dev-preview] 已添加账号 " + lkey + "（" + lch + "）");
            return Promise.resolve({ status: "ok", uid: lu.uid });
          }
          case "hypergryph_login": {
            var hg2 = (args && args.game) || cfg.current_game;
            loginUpsert("hypergryph", hg2, args.phone, "鹰角预览号 " + args.phone);
            return Promise.resolve({ status: "ok" });
          }
          // --- 渠道登录（渠道扩展）：预览环境直接"成功"，添加一个渠道账号 ---
          case "channel_login": {
            var cch = (args && args.channel) || "xiaomi";
            var cg = (args && args.game) || cfg.current_game;
            var cuid = "mock-" + cch + "-" + String(90000 + Math.floor(Math.random() * 9999));
            cfg.channel_accounts.push({
              channel: cch, game: cg, uid: cuid, name: "预览渠道号 " + cuid,
              cred: "mock", alias: "", remark: "", last_login_succ: true, last_used_at: nowSec()
            });
            console.log("[dev-preview] 已添加渠道账号 " + cuid + "（" + cch + "）");
            // 与真实后端一致：登录即选中该账号，并清掉 B服/官服渠道指向。
            cfg.active_channel = cfg.active_channel || {};
            cfg.active_channel[cg] = { channel: cch, uid: cuid };
            cfg.game_channel[cg] = null;
            return Promise.resolve({ channel: cch, channel_label: "预览渠道", uid: cuid, name: "预览渠道号 " + cuid });
          }
          // --- B 站 SDK 授权登录（Android）：预览环境直接"成功" ---
          case "bili_sdk_login": {
            var bg = (args && args.game) || cfg.current_game;
            loginUpsert("bilibili", bg, "20002", "B站授权预览账号");
            console.log("[dev-preview] B 站授权登录已 mock");
            return Promise.resolve({ status: "ok" });
          }
          case "delete_channel_account": {
            var dch = args && args.channel, dg = (args && args.game) || cfg.current_game, du = args && args.uid;
            cfg.channel_accounts = cfg.channel_accounts.filter(function (a) {
              return !(a.channel === dch && a.game === dg && a.uid === du);
            });
            return Promise.resolve();
          }
          case "live_watch_sync": case "set_last_live": case "set_preview_active": case "set_scan_region":
          case "set_tile_mode": case "set_ui_widths": case "open_url":
            return Promise.resolve();
          default: return Promise.resolve(null);
        }
      }
    },
    event: {
      listen: function (name, cb) { (listeners[name] = listeners[name] || []).push(cb); return Promise.resolve(function () {}); }
    },
    window: {
      getCurrentWindow: function () { return { startDragging: function () { return Promise.resolve(); }, minimize: function () { return Promise.resolve(); }, close: function () { return Promise.resolve(); }, setAlwaysOnTop: function () { return Promise.resolve(); } }; }
    }
  };
  window.__TAURI_INTERNALS__ = {};
})();