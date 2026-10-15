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