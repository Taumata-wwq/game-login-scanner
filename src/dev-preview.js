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
    frameTick++;
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
          case "get_config": return Promise.resolve(JSON.parse(JSON.stringify(cfg)));
          case "get_app_version": return Promise.resolve("0.3.2-preview");
          case "save_config": return Promise.resolve();
          case "set_options":
            if (args && args.patch) { for (var k in args.patch) cfg[k] = args.patch[k]; }
            return Promise.resolve();
          case "switch_game": if (args) cfg.current_game = args.game; return Promise.resolve();
          case "refresh_official_unames": return Promise.resolve();
          case "frontend_log": return Promise.resolve();
          case "check_update": return Promise.resolve();
          case "start_scan": emit("scan-status", { type: "started", target: args && args.target, message: "开始扫码（预览）" }); return Promise.resolve();
          case "stop_scan": emit("scan-status", { type: "stopped", target: args && args.target, message: "已停止（预览）" }); return Promise.resolve();
          case "screen_capture_state": return Promise.resolve(false);
          case "request_screen_capture": return Promise.resolve();
          case "stop_screen_capture": return Promise.resolve();
          case "capture_preview": return Promise.resolve(previewFrame());
          case "login_qr_start": return Promise.resolve(["preview", '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 29 29" shape-rendering="crispEdges" style="width:100%;height:100%"><rect width="29" height="29" fill="#fff"/><rect x="0" y="0" width="7" height="7" fill="#000"/><rect x="22" y="0" width="7" height="7" fill="#000"/><rect x="0" y="22" width="7" height="7" fill="#000"/><rect x="2" y="2" width="3" height="3" fill="#fff"/><rect x="24" y="2" width="3" height="3" fill="#fff"/><rect x="2" y="24" width="3" height="3" fill="#fff"/><rect x="10" y="4" width="2" height="2" fill="#000"/><rect x="14" y="8" width="2" height="2" fill="#000"/><rect x="18" y="4" width="2" height="2" fill="#000"/><rect x="10" y="12" width="2" height="2" fill="#000"/><rect x="16" y="14" width="2" height="2" fill="#000"/><rect x="12" y="18" width="2" height="2" fill="#000"/><rect x="18" y="18" width="2" height="2" fill="#000"/><rect x="4" y="10" width="2" height="2" fill="#000"/><rect x="4" y="14" width="2" height="2" fill="#000"/><rect x="24" y="10" width="2" height="2" fill="#000"/><rect x="24" y="16" width="2" height="2" fill="#000"/><rect x="10" y="24" width="2" height="2" fill="#000"/><rect x="16" y="24" width="2" height="2" fill="#000"/></svg>']);
          case "login_qr_poll": return Promise.resolve(["waiting", "official", "", ""]);
          case "live_start": return Promise.resolve({ session_id: "preview", w: 1280, h: 720 });
          case "live_stop": return Promise.resolve();
          case "live_refresh_meta": return Promise.resolve(null);
          case "live_set_active": return Promise.resolve();
          case "live_history_push": case "live_history_remove": case "live_history_reorder": case "live_history_set_note":
          case "live_watch_sync": case "set_last_live": case "set_preview_active": case "set_scan_region":
          case "set_tile_mode": case "set_ui_widths": case "select_account": case "delete_account":
          case "set_account_meta": case "reorder_accounts": case "hypergryph_select": case "hypergryph_delete":
          case "hypergryph_set_meta": case "hypergryph_reorder": case "login_qr_cancel":
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