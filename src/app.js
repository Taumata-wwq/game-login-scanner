(function () {
  'use strict';
  const T = window.__TAURI__;
  const invoke = T && T.core ? T.core.invoke : null;
  const listen = T && T.event ? T.event.listen : null;
  const appWindow = T && T.window ? T.window.getCurrentWindow() : null;

  let config = null;
  // 并行扫码：每路目标（"screen" / "live:{id}"）独立启停；最多 5 路直播 + 1 路桌面。
  let scanningTargets = new Set();
  let stoppingTargets = new Set();
  // 「扫码成功后自动停止其他直播间」（设置项；renderOptions 从配置同步）。
  let stopOthersOnScanSuccess = true;
  // 屏幕扫码：切后台 / 回前台自动停（设置项，默认开；renderOptions 从配置同步）。
  let screenScanAutoBgEnabled = true;
  let screenScanStopOnResume = true;
  // 通知开关（设置项，默认开）。
  let notifyPersistent = true;
  let notifyLiveStatus = true;
  // 移动端「摄像头扫码」取景模式：中间显示区扩展覆盖账号 / 游戏区服区。
  // 纯 UI 模式——进入/退出都不触碰任何扫描会话（直播 / 屏幕各路照常运行）。
  let cameraMode = false;
  let cameraSavedH = 0;   // 进入前 .acct-area 的高度（退出时按此值平滑还原）
  // 摄像头预览循环句柄（取景模式下持续拉取 capture_camera_frame 绘到 #mcCanvas）。
  let cameraPreviewTimer = null;
  let cameraPreviewBusy = false;
  let filter = 'official';            // 账号筛选：official | bilibili（默认官服）
  // 扫描区域（实时监视器）
  let preview = null;                 // {mon:{x,y,w,h}} 预览所在显示器信息
  let view = { scale: 1, fx: 0.5, fy: 0.5 };   // 实际显示的视口（平滑插值后的值）
  // 目标视口：鼠标/滚轮直接设置它，view 每帧快速追向它（把瞬时跳变变为平滑滑动，
  // 削弱起手/缩放时的卡顿感）。重置视图也只需设目标值。
  let viewT = { scale: 1, fx: 0.5, fy: 0.5 };
  // 只读调试探针：供浏览器预览下的自动化验证读取视口状态（拖拽/缩放是否真的生效）。
  // 画布内容本身不可靠——预览帧持续更新会让像素哈希自然变化。
  window.__viewState = () => ({
    target: { ...viewT }, shown: { ...view }, hasPreview: !!preview,
    mon: preview && preview.mon ? { ...preview.mon } : null,
  });

  const $ = (id) => document.getElementById(id);
  const logEl = $('log');

  // 顶层自检：脚本在初始化途中中断（元素缺失等模块级异常不触发 window.onerror，
  // 表现为「界面能渲染但全部 JS 失效」）时，3 秒后主动上报，避免静默失败。
  let moduleOk = false;
  setTimeout(() => {
    if (!moduleOk) {
      try { invoke('frontend_log', { level: 'error', msg: '[自检] 前端模块未完成初始化：可能有元素缺失或脚本中断' }); } catch (e) {}
    }
  }, 3000);

  // 全局错误捕获：仅记录脚本运行时错误（e.target 为 window）。
  // 资源加载失败（img/script 等，e.target 为元素、message 为空）不记录——
  // 否则每个封面加载失败都会刷一条「脚本错误: @」。
  window.addEventListener('error', (e) => {
    if (e.target && e.target !== window) return;
    try { log('脚本错误: ' + (e.message || '') + ' @ ' + (e.filename || ''), 'error'); } catch (x) {}
  }, true);
  window.addEventListener('unhandledrejection', (e) => {
    try { log('未处理 Promise 拒绝: ' + (e.reason || ''), 'error'); } catch (x) {}
  });

  // ---------- 全局交互 ----------
  // 禁用全局右键菜单（问题4）。
  document.addEventListener('contextmenu', (e) => e.preventDefault());

  // ---------- 无边框窗口缩放（四边/四角热区 → Tauri startResizeDragging） ----------
  // decorations:false 时系统不提供边缘缩放；8 个固定热区（6/12px）委托
  // startResizeDragging 按方向驱动。热区置于 body 直下（不在 header 内），
  // 因此不会与 data-tauri-drag-region 的标题栏拖拽互相触发。
  document.querySelectorAll('.rz').forEach((el) => {
    el.addEventListener('mousedown', (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      try {
        window.__TAURI__.window.getCurrentWindow().startResizeDragging(el.dataset.rz);
      } catch (x) { /* 忽略（如非 Tauri 环境） */ }
    });
  });

  // 窗口拖拽：仅在「背景区域」按下才触发（不含按钮/输入/画布/卡片等交互元素）。
  // 单一实现（纯 JS startDragging）——不再用 data-tauri-drag-region，避免两处竞争。
  function isInteractive(el) {
    if (!el || el === document.body) return false;
    const tag = el.tagName;
    if (tag === 'BUTTON' || tag === 'INPUT' || tag === 'CANVAS' || tag === 'A' || tag === 'SELECT' || tag === 'TEXTAREA') return true;
    if (el.isContentEditable) return true;
    if (el.closest('.rz, button, input, canvas, a, .room-card, .live-h-list, .acct-item, .gitem, .ibtn, .vsplit, .gsplit, .overlay, .fps, .region-reset, .startbar, .game-col, .acct-list, .room-list')) return true;
    return false;
  }
  document.addEventListener('mousedown', (e) => {
    // 移动端无桌面窗口：不接管拖窗（与文件内其余同类调用一致）。
    if (window.__MOBILE__) return;
    if (e.button !== 0) return;
    if (e.detail > 1) return;  // 双击不触发拖拽
    if (isInteractive(e.target)) return;
    if (!appWindow || !appWindow.startDragging) return;
    e.preventDefault();
    appWindow.startDragging().catch(() => {});
  });

  // ---------- 日志 ----------
  // 时间戳手工补零：`toLocaleTimeString` 在 0 点会给出「24:00:00」
  // （hour12:false 的 h24 行为，且 hourCycle 会被 hour12 覆盖），日志里读着
  // 像次日。这里直接按 24 小时制拼，跨浏览器/WebView 完全确定。
  function logTime(d) {
    const p = (n) => (n < 10 ? '0' + n : '' + n);
    return p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds());
  }
  function log(msg, type) {
    const line = document.createElement('div');
    line.className = type || 'info';
    const t = document.createElement('span');
    t.className = 'time';
    t.textContent = logTime(new Date());
    line.appendChild(t);
    line.appendChild(document.createTextNode(msg));
    logEl.appendChild(line);
    while (logEl.childNodes.length > 500) logEl.removeChild(logEl.firstChild);
    logEl.scrollTop = logEl.scrollHeight;
  }
  $('btnClearLog').addEventListener('click', (e) => { e.stopPropagation(); logEl.textContent = ''; });

  // ---------- 窗口控制 ----------
  function win(action) {
    invoke('frontend_log', { level: 'info', msg: '窗口操作: ' + action }).catch(() => {});
    if (!appWindow) { log('窗口 API 不可用（__TAURI__.window 缺失）', 'error'); return; }
    try {
      const p = appWindow[action]();
      if (p && p.catch) p.catch((e) => {
        log('窗口操作失败(' + action + '): ' + e, 'error');
        invoke('frontend_log', { level: 'error', msg: '窗口操作失败(' + action + '): ' + e }).catch(() => {});
      });
    } catch (e) {
      log('窗口操作异常(' + action + '): ' + e, 'error');
      invoke('frontend_log', { level: 'error', msg: '窗口操作异常(' + action + '): ' + e }).catch(() => {});
    }
  }
  // 标题栏按钮：pointerdown 为主路径（WebView2 最小化还原后，首次 click 可能被
  // 「激活点击」吞掉——表现为按钮一时全部失灵）；click 兜底，done 标志去重。
  function bindWinButton(id, fn) {
    const el = $(id);
    let fired = false;
    el.addEventListener('pointerdown', (e) => {
      if (e.button !== 0) return;
      fired = true;
      fn();
    });
    el.addEventListener('click', () => {
      if (fired) { fired = false; return; }
      fn();
    });
  }
  bindWinButton('btnTools', () => setToolsOpen($('toolsOverlay').hidden));
  bindWinButton('btnMin', () => win('minimize'));
  bindWinButton('btnClose', () => win('close'));

  // ---------- 弹窗 ----------
  function setToolsOpen(open) {
    $('toolsOverlay').hidden = !open;
    $('btnTools').classList.toggle('open', open);
    // 打开设置面板是请求通知权限的合适时机：用户此刻正看着通知相关的开关，
    // 语境明确（Android 13+ 不请求就发不出任何通知）。
    if (open) requestNotifyPermissionOnce();
  }
  let notifyPermissionAsked = false;
  function requestNotifyPermissionOnce() {
    if (notifyPermissionAsked) return;
    notifyPermissionAsked = true;
    invoke('request_notify_permission').catch(() => {});
  }
  document.querySelectorAll('[data-close]').forEach((b) => {
    b.addEventListener('click', () => {
      // 移动端工具面板为滑动抽屉（常驻显示）：交由抽屉模型收回，勿置 hidden。
      if (window.__MOBILE__ && b.dataset.close === 'toolsOverlay') {
        if (window.closeRight) window.closeRight();
        return;
      }
      $(b.dataset.close).hidden = true;
      if (b.dataset.close === 'loginOverlay') cancelQr();
    });
  });
  document.querySelectorAll('.overlay').forEach((o) => {
    o.addEventListener('mousedown', (e) => {
      if (e.target === o) {
        if (window.__MOBILE__ && o.id === 'toolsOverlay') return; // 抽屉模型：勿置 hidden
        o.hidden = true;
        if (o.id === 'loginOverlay') cancelQr();
      }
    });
  });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      document.querySelectorAll('.overlay').forEach((o) => {
        if (window.__MOBILE__ && o.id === 'toolsOverlay') {
          if (window.closeRight) window.closeRight();
          return;
        }
        o.hidden = true;
        if (o.id === 'loginOverlay') cancelQr();
      });
    }
  });

  // 长按手势（移动端）：静止按住 500ms 触发；移动 >10px 或抬起取消；触发后吞掉随后的 click。
  // 用 touch 事件实现（移动端环境；避免 pointer 合成在部分 WebView/模拟环境的差异）。
  function bindLongPress(el, fn) {
    let timer = null, sx = 0, sy = 0, target = null, fired = false;
    el.addEventListener("touchstart", (e) => {
      const t = e.touches[0];
      if (!t) return;
      sx = t.clientX; sy = t.clientY; target = e.target; fired = false;
      timer = setTimeout(() => { timer = null; fired = true; fn(target); }, 500);
    }, { passive: true });
    el.addEventListener("touchmove", (e) => {
      if (!timer) return;
      const t = e.touches[0];
      if (t && (Math.abs(t.clientX - sx) > 10 || Math.abs(t.clientY - sy) > 10)) {
        clearTimeout(timer); timer = null;
      }
    }, { passive: true });
    const cancel = () => { if (timer) { clearTimeout(timer); timer = null; } };
    el.addEventListener("touchend", cancel);
    el.addEventListener("touchcancel", cancel);
    el.addEventListener("click", (e) => {
      if (fired) { fired = false; e.stopPropagation(); e.preventDefault(); }
    }, true);
  }

  // ---------- 账号渲染 ----------
  // 当前游戏 id（账号按游戏独立存储；旧数据 game 为空时归入崩坏3）。
  function curGameId() { return config ? (config.current_game || 'bh3') : 'bh3'; }
  function acctGame(t) { return t.game; }
  function accountsAll() {
    if (!config) return [];
    const g = curGameId();
    const out = [];
    // 保留配置中的存储顺序（用户可拖拽调整），不再按使用时间排序。
    (config.bili_accounts || []).forEach((t) => { if (acctGame(t) === g) out.push({ ch: 'bilibili', t }); });
    (config.official_accounts || []).forEach((t) => { if (acctGame(t) === g) out.push({ ch: 'official', t }); });
    (config.hypergryph_accounts || []).forEach((t) => { if (acctGame(t) === g) out.push({ ch: 'hypergryph', t }); });
    // 第三方渠道账号（渠道扩展；**仅崩坏3** 显示——其它游戏无渠道服）。
    if (g === 'bh3') {
      (config.channel_accounts || []).forEach((t) => { if (acctGame(t) === g) out.push({ ch: 'channel:' + t.channel, t }); });
    }
    return out;
  }
  // 当前游戏下某渠道的激活账号标识（active_* 现为 { game: id } 映射）。
  function activeUidOf(ch) {
    if (!config) return null;
    const g = curGameId();
    if (ch === 'bilibili') return (config.active_bili || {})[g];
    if (ch === 'hypergryph') return (config.active_hypergryph || {})[g];
    return (config.active_official || {})[g];
  }
  // 鹰角账号以 phone 作标识；其余以 uid。
  function acctKey(ch, t) { return ch === 'hypergryph' ? t.phone : t.uid; }
  // 第三方渠道账号（ch 形如 'channel:xiaomi'）。
  function isChannelAcct(ch) { return ch.startsWith('channel:'); }
  // 是否「当前账号」：该游戏该渠道的激活标识匹配。
  //
  // 「当前账号」全局唯一：第三方渠道账号与官服/B 服互斥。后端在选中任一方时都会
  // 清掉另一方的指向（config.rs 的 set_current_channel / upsert_bili / upsert_official
  // 与 lib.rs 的 select_account），这里再做一次互斥，保证旧配置里两个字段同时有值时
  // 也只高亮一个（升级后不必等用户重新点一次）。
  function isCurrentAccount(ch, key) {
    if (!config) return false;
    const g = curGameId();
    if (ch === 'hypergryph') return (config.active_hypergryph || {})[g] === key;
    if (isChannelAcct(ch)) {
      const sel = (config.active_channel || {})[g];
      return !!sel && sel.channel === ch.slice(8) && sel.uid === key;
    }
    // 官服 / B 服：该游戏已选中第三方渠道账号时，两者都不再是「当前账号」。
    if ((config.active_channel || {})[g]) return false;
    const curCh = (config.game_channel || {})[g];
    return curCh === ch && activeUidOf(ch) === key;
  }
  function accountName(ch, t) {
    if (ch === 'hypergryph') {
      const base = t.phone || '未知';
      return t.alias ? (t.alias + '（' + base + '）') : base;
    }
    if (isChannelAcct(ch)) {
      const base = t.name || ('UID ' + t.uid);
      return t.alias ? (t.alias + '（' + base + '）') : base;
    }
    const base = ch === 'bilibili' ? (t.uname || 'UID ' + t.uid) : (t.uname || 'uid ' + t.uid);
    // 有备注名时显示「备注名（原名）」。
    return t.alias ? (t.alias + '（' + base + '）') : base;
  }
  function channelLabel(ch) {
    if (ch === 'hypergryph') return '鹰角';
    if (ch === 'official') return '官服';
    if (ch.startsWith('channel:')) return channelUiLabel(ch.slice(8));
    return 'B 服';
  }

  // ---------- 第三方渠道（渠道登录，Android）----------
  // 与后端 sdk::channel::CHANNELS 对应——仅列已实现的渠道。
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

  // ---------- 游戏（多游戏支持） ----------
  // 各游戏共享同一批账号（同一米游社/B站账号可登录米哈游全系游戏）；
  // 切换游戏只改变扫描确认时使用的 biz_key 与协议端点。
  const GAMES = [
    { id: 'bh3', name: '崩坏3', vendor: 'mihoyo', desc: '官服 / B服', official: true, bili: true, hyper: false },
    { id: 'hsr', name: '崩坏：星穹铁道', vendor: 'mihoyo', desc: '官服 / B服', official: true, bili: true, hyper: false },
    { id: 'genshin', name: '原神', vendor: 'mihoyo', desc: '官服 / B服', official: true, bili: true, hyper: false },
    { id: 'zzz', name: '绝区零', vendor: 'mihoyo', desc: '官服 / B服', official: true, bili: true, hyper: false },
    { id: 'endfield', name: '明日方舟：终末地', vendor: 'hypergryph', desc: '鹰角账号', official: false, bili: false, hyper: true },
    { id: 'arknights', name: '明日方舟', vendor: 'hypergryph', desc: '鹰角账号', official: false, bili: false, hyper: true },
  ];
  function gameMeta(id) { return GAMES.find((g) => g.id === id) || GAMES[0]; }
  function gameLabel(id) { return gameMeta(id).name; }
  // 游戏选择列（左列）：直接列全部游戏，点击切换。
  function renderGame() {
    if (!config) return;
    const col = $('gameCol');
    if (!col) return;
    col.textContent = '';
    const cur = config.current_game || 'bh3';
    for (const g of GAMES) {
      const it = document.createElement('div');
      it.className = 'gitem' + (g.id === cur ? ' on' : '');
      const nm = document.createElement('span');
      nm.textContent = g.name;
      it.appendChild(nm);
      it.addEventListener('click', () => switchGame(g.id));
      col.appendChild(it);
    }
    const mgn = $('mGameName');
    if (mgn) mgn.textContent = gameMeta(cur).name;
  }
  async function switchGame(id) {
    if (!config || config.current_game === id) { if (window.__MOBILE__) closeMGameOverlay(); return; }
    try {
      await invoke('switch_game', { game: id });
      config = await invoke('get_config');
    } catch (e) {
      log('切换游戏失败: ' + e, 'error');
      return;
    }
    // 恢复目标游戏上次的渠道；不支持时由 renderFilter 回退。
    filter = initialFilterForGame(id);
    renderGame();
    renderAll();
    if (window.__MOBILE__) closeMGameOverlay();
    log('已切换到 ' + gameLabel(id), 'info');
  }

  // 打开某游戏时初始展示哪个渠道的账号列表：优先「当前选中的第三方渠道账号」，
  // 其次上次使用的渠道（game_channel），都没有则回退到官服/B 服。
  // 必须在两处共用（启动恢复、切换游戏），否则渠道账号被选中后 game_channel 为
  // 空，列表会停在官服/B 服上，看不到那个已被选中的渠道账号。
  function initialFilterForGame(gid) {
    const g = gameMeta(gid);
    if (g.hyper) return 'hypergryph';
    const sel = (config && (config.active_channel || {})[gid]) || null;
    if (sel && g.id === 'bh3') return 'channel:' + sel.channel;
    const saved = (config && (config.game_channel || {})[gid]) || null;
    return saved || filter;
  }

  function renderFilter() {
    const g = gameMeta(config ? (config.current_game || 'bh3') : 'bh3');
    // 先回退 filter 到该游戏支持的渠道，再渲染高亮——否则切换游戏时高亮会先投给
    // 即将被隐藏的旧渠道，导致当前无任何按钮高亮（直到下次渲染）。
    // 渠道服（channel:*）仅崩坏3有；切到其它游戏时一律回退到官服。
    if (isChannelAcct(filter) && g.id !== 'bh3') filter = 'official';
    if (g.hyper) {
      if (filter !== 'hypergryph') filter = 'hypergryph';
    } else {
      if (filter === 'hypergryph') filter = 'official';
      if (filter === 'bilibili' && !g.bili) filter = 'official';
      if (filter === 'official' && !g.official) filter = 'bilibili';
    }
    document.querySelectorAll('#filterSeg button').forEach((b) => {
      const f = b.dataset.f;
      const supported = f === 'bilibili' ? g.bili : (f === 'official' ? g.official : g.hyper);
      b.hidden = !supported;
      b.classList.toggle('on', f === filter && supported);
    });
    // 移动端区服名同步。
    const mc = $('mChanName');
    if (mc) mc.textContent = channelLabel(filter);
  }

  function renderAccounts() {
    const list = $('acctList');
    list.textContent = '';
    const rows = accountsAll().filter((a) => a.ch === filter);
    rows.forEach(({ ch, t }, idx) => {
      const key = acctKey(ch, t);
      const row = document.createElement('div');
      const isCurrent = isCurrentAccount(ch, key);
      row.className = 'acct-item' + (isCurrent ? ' on' : '');
      row.draggable = !isChannelAcct(ch); // 渠道账号暂不参与拖拽排序
      row.dataset.uid = key;
      row.dataset.ch = ch;
      row.dataset.idx = idx;
      // 选中按钮：仅点击它才切换当前账号。
      const radio = document.createElement('span');
      radio.className = 'radio';
      radio.title = '设为当前账号';
      // 键盘可达：Tab 聚焦，Enter/空格与点击同效（只补语义，不改视觉）。
      radio.tabIndex = 0;
      radio.setAttribute('role', 'button');
      radio.addEventListener('click', (e) => { e.stopPropagation(); selectAccount(ch, key); });
      radio.addEventListener('keydown', (e) => {
        if (e.key !== 'Enter' && e.key !== ' ') return;
        e.preventDefault();
        e.stopPropagation();
        selectAccount(ch, key);
      });
      const chip = document.createElement('span');
      chip.className = 'chip';
      chip.textContent = channelLabel(ch);
      const nm = document.createElement('span');
      nm.className = 'nm';
      nm.textContent = accountName(ch, t);
      nm.title = '双击修改备注';
      // 右侧备注（无则为空）。
      const remark = document.createElement('span');
      remark.className = 'remark';
      remark.textContent = t.remark || '';
      remark.title = t.remark || '';
      const del = document.createElement('button');
      del.className = 'ibtn';
      del.title = '删除该账号';
      del.innerHTML = '<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"><path d="M6 6l12 12M18 6L6 18"/></svg>';
      del.addEventListener('click', (e) => { e.stopPropagation(); deleteAccount(ch, key); });
      row.append(radio, chip, nm, remark, del);
      // 双击行（非按钮/选中点）→ 编辑备注（内部弹窗）；移动端触摸无双击：长按触发。
      row.addEventListener('dblclick', (e) => {
        if (e.target.closest('.radio') || e.target.closest('.ibtn')) return;
        if (isChannelAcct(ch)) return; // 渠道账号备注编辑后续版本接入
        openRemarkDialog(ch, key);
      });
      if (window.__MOBILE__) {
        bindLongPress(row, (tgt) => {
          if (tgt && (tgt.closest('.radio') || tgt.closest('.ibtn'))) return;
          if (isChannelAcct(ch)) return; // 渠道账号备注编辑后续版本接入
          openRemarkDialog(ch, key);
        });
      }
      // 拖拽排序。
      row.addEventListener('dragstart', (e) => {
        dragUid = key;
        row.classList.add('dragging');
        e.dataTransfer.effectAllowed = 'move';
      });
      row.addEventListener('dragend', () => { dragUid = null; row.classList.remove('dragging'); renderAccounts(); });
      row.addEventListener('dragover', (e) => { e.preventDefault(); e.dataTransfer.dropEffect = 'move'; });
      row.addEventListener('drop', (e) => {
        e.preventDefault();
        if (!dragUid || dragUid === key) return;
        reorderAccounts(ch, dragUid, key);
      });
      list.appendChild(row);
    });
    // 「添加账号」作为列表末项（与账号同级、随列表滚动；不参与拖拽排序）。
    //
    // Android + B 服：**不提供**通用「＋ 添加账号」。手机端 B 服统一走 B 站 SDK
    // 授权（下方那个按钮），扫码/账密登录在手机上体验远不如一次授权，且授权链路
    // 已真机跑通——保留两条入口只会让用户困惑该点哪个。
    const biliSdkOnly = (filter === 'bilibili' && window.__ANDROID__);
    if (!biliSdkOnly) {
      const add = document.createElement('button');
      add.type = 'button';
      add.className = 'acct-add';
      add.textContent = '＋ 添加账号';
      add.title = '添加账号';
      add.addEventListener('click', () => {
        if (filter === 'hypergryph') {
          $('hyperPhone').value = '';
          $('hyperCode').value = '';
          $('hyperStatus').textContent = '';
          $('hyperOverlay').hidden = false;
          $('hyperPhone').focus();
          return;
        }
        if (isChannelAcct(filter)) {
          channelLogin(filter.slice(8));
          return;
        }
        openLogin();
      });
      list.appendChild(add);
    }
    // B 服（Android）：B 站授权登录入口——`bili_sdk_login` 调起 B 站客户端授权，
    // 产出 uid + access_key 后由后端校验并入库（B 服账号）。
    if (biliSdkOnly) {
      const addSdk = document.createElement('button');
      addSdk.type = 'button';
      addSdk.className = 'acct-add';
      addSdk.textContent = '＋ B 站授权登录';
      addSdk.title = '使用 B 站 SDK 授权登录（Android）';
      addSdk.addEventListener('click', () => biliSdkLogin());
      list.appendChild(addSdk);
    }
  }

  // 拖拽排序：把 dragUid 移到 targetUid 位置并保存。
  let dragUid = null;
  async function reorderAccounts(ch, dragUid, targetUid) {
    if (ch === 'hypergryph') {
      // 仅重排当前游戏的鹰角账号：交给后端（按 game 过滤），避免前端在混合全量
      // 数组上重排而打乱其它游戏排序。
      const game = curGameId();
      const mine = (config.hypergryph_accounts || []).filter((a) => a.game === game);
      const order = mine.map((a) => a.phone);
      const from = order.indexOf(dragUid);
      const to = order.indexOf(targetUid);
      if (from < 0 || to < 0) return;
      order.splice(from, 1);
      order.splice(to, 0, dragUid);
      await invoke('hypergryph_reorder', { game, order });
      config = await invoke('get_config');
      renderAccounts();
      return;
    }
    const order = (ch === 'bilibili' ? config.bili_accounts : config.official_accounts).map((a) => a.uid);
    const from = order.indexOf(dragUid);
    const to = order.indexOf(targetUid);
    if (from < 0 || to < 0) return;
    order.splice(from, 1);
    order.splice(to, 0, dragUid);
    try {
      await invoke('reorder_accounts', { game: curGameId(), channel: ch, order });
      config = await invoke('get_config');
      renderAccounts();
    } catch (e) { log('调整顺序失败: ' + e, 'error'); }
  }

  // 双击行编辑备注名 + 备注信息（内部弹窗）。
  let remarkTarget = null;   // { ch, uid }
  function openRemarkDialog(ch, uid) {
    const arr = ch === 'bilibili' ? config.bili_accounts
              : (ch === 'hypergryph' ? config.hypergryph_accounts : config.official_accounts);
    const g = curGameId();
    const t = (arr || []).find((a) =>
      (ch === 'hypergryph' ? a.phone : a.uid) === uid && acctGame(a) === g);
    if (!t) return;
    remarkTarget = { ch, uid };
    $('remarkAlias').value = t.alias || '';
    $('remarkText').value = t.remark || '';
    $('remarkOverlay').hidden = false;
    $('remarkAlias').focus();
  }
  function closeRemarkDialog() { $('remarkOverlay').hidden = true; remarkTarget = null; }
  async function saveRemarkDialog() {
    if (!remarkTarget) return;
    const { ch, uid } = remarkTarget;
    const alias = $('remarkAlias').value.trim();
    const remark = $('remarkText').value.trim();
    try {
      const game = curGameId();
      if (ch === 'hypergryph') {
        await invoke('hypergryph_set_meta', { game, phone: uid, alias, remark });
      } else {
        await invoke('set_account_meta', { game, channel: ch, uid, alias, remark });
      }
      config = await invoke('get_config');
      closeRemarkDialog();
      renderAccounts();
    } catch (e) { log('保存备注失败: ' + e, 'error'); }
  }

  // 设置项更新：走字段级命令（不整份回写配置），避免与账号类命令并发互相覆盖。
  // patch 的键名与 set_options 的补丁结构体字段一致（蛇形）。
  // 白名单必须与后端 OptionsPatch 的字段集合完全一致：漏一个键 = 该开关静默不落盘
  // （历史上 screen_scan_auto_bg / screen_scan_stop_on_resume / notify_persistent /
  // notify_live_status 四项就是这样失效的）。失败时回滚镜像并提示，避免 UI 说谎。
  async function saveCfg(patch) {
    const prev = config;
    config = Object.assign({}, config, patch);
    const args = {};
    if ('auto_login_last' in patch) args.auto_login_last = patch.auto_login_last;
    if ('always_on_top' in patch) args.always_on_top = patch.always_on_top;
    if ('auto_close' in patch) args.auto_close = patch.auto_close;
    if ('stop_others_on_scan_success' in patch) args.stop_others_on_scan_success = patch.stop_others_on_scan_success;
    if ('auto_check_update' in patch) args.auto_check_update = patch.auto_check_update;
    if ('display_fps' in patch) args.display_fps = patch.display_fps;
    if ('display_delay_ms' in patch) args.display_delay_ms = patch.display_delay_ms;
    if ('display_name' in patch) args.display_name = patch.display_name;
    if ('screen_scan_auto_bg' in patch) args.screen_scan_auto_bg = patch.screen_scan_auto_bg;
    if ('screen_scan_stop_on_resume' in patch) args.screen_scan_stop_on_resume = patch.screen_scan_stop_on_resume;
    if ('notify_persistent' in patch) args.notify_persistent = patch.notify_persistent;
    if ('notify_live_status' in patch) args.notify_live_status = patch.notify_live_status;
    try {
      await invoke('set_options', { patch: args });
    } catch (e) {
      config = prev;
      renderOptions();
      log('保存设置失败: ' + e, 'error');
    }
  }

  async function selectAccount(ch, uid) {
    try {
      const game = curGameId();
      if (ch === 'hypergryph') {
        await invoke('hypergryph_select', { game, phone: uid });
      } else {
        await invoke('select_account', { game, channel: ch, uid });
      }
      config = await invoke('get_config');
      log('已切换到 ' + channelLabel(ch) + ' 账号 ' + uid, 'info');
      renderAll();
    } catch (e) {
      log('切换账号失败: ' + e, 'error');
    }
  }

  async function deleteAccount(ch, uid) {
    try {
      const game = curGameId();
      if (isChannelAcct(ch)) {
        await invoke('delete_channel_account', { channel: ch.slice(8), game, uid });
      } else if (ch === 'hypergryph') {
        await invoke('hypergryph_delete', { game, phone: uid });
      } else {
        await invoke('delete_account', { game, channel: ch, uid });
      }
      config = await invoke('get_config');
      log('已删除 ' + channelLabel(ch) + ' 账号 ' + uid, 'info');
      renderAll();
    } catch (e) {
      log('删除账号失败: ' + e, 'error');
    }
  }

  // B 站 SDK 授权登录（Android）：调起 B 站客户端授权 → uid+access_key → 后端校验并入库（B 服账号）。
  async function biliSdkLogin() {
    try {
      const game = curGameId();
      log("正在调起 B 站授权登录…", "info");
      await invoke("bili_sdk_login", { game });
      config = await invoke("get_config");
      log("B 站授权登录成功", "ok");
      renderAll();
    } catch (e) {
      log("B 站授权登录失败: " + e, "error");
    }
  }

  // ---------- 统一登录弹窗（二维码 / 验证码 / 密码） ----------
  const $qrImg = $('addQrImg');
  const $qrLabel = $('addQrLabel');
  let qrActive = false;
  let pollTimer = null;
  let pollFails = 0;
  let qrStartSeq = 0;
  let qrAutoRetry = 0;
  // 扫码登录轮询间隔：等待扫码时 1.5s（保守，与官方前端同量级，控制风控面）；
  // 检测到「已扫码」后提速到 450ms——手机随时可能点确认，固定 1.5s 会让
  // 「确认 → 桌面显示成功」多等一个整间隔。自适应后该延迟最高省 ~1s。
  let qrPollDelay = 1500;

  function qrChannelLabel(ch) {
    return ch === 'official' ? '官服 · 米游社 App 扫码' : 'B 服 · B 站 App 扫码';
  }
  function setQrStatus(text, cls) {
    const el = $('credStatus');
    el.textContent = text;
    el.className = 'qr-status' + (cls ? ' ' + cls : '');
  }
  function stopPolling() { if (pollTimer) { clearTimeout(pollTimer); pollTimer = null; } }

  async function startQrLogin() {
    if (qrActive) return;
    const ch = filter;
    const seq = ++qrStartSeq;
    qrActive = true;
    pollFails = 0;
    qrPollDelay = 1500;
    $qrLabel.textContent = qrChannelLabel(ch);
    $qrImg.innerHTML = '';
    setQrStatus('生成中…', '');
    $('btnQrRefresh').style.display = 'none';
    try {
      const [, svg] = await invoke('login_qr_start', { channel: ch });
      if (seq !== qrStartSeq) { invoke('login_qr_cancel').catch(() => {}); return; }
      $qrImg.innerHTML = svg;
      setQrStatus('等待扫码…', '');
      log('已生成' + (ch === 'official' ? '官服' : 'B 服') + '登录二维码（180 秒内有效）', 'info');
      pollLoop();
    } catch (e) {
      if (seq !== qrStartSeq) return;
      qrActive = false;
      setQrStatus('生成失败', 'expired');
      $('btnQrRefresh').style.display = '';
      log('生成二维码失败: ' + e, 'error');
    }
  }

  function pollLoop() {
    stopPolling();
    pollTimer = setTimeout(async () => {
      if (!qrActive) return;
      let r;
      try {
        r = await invoke('login_qr_poll');
        pollFails = 0;
      } catch (e) {
        if (String(e).includes('没有进行中的扫码登录')) { qrActive = false; return; }
        pollFails++;
        if (pollFails >= 8) {
          qrActive = false;
          setQrStatus('网络异常，请点「重新生成」', 'expired');
          $('btnQrRefresh').style.display = '';
          log('扫码登录轮询连续失败: ' + e, 'error');
          return;
        }
        pollLoop();
        return;
      }
      const [status, channel, uid, name] = r;
      if (status === 'waiting') {
        setQrStatus($qrImg.innerHTML ? '等待扫码…' : '生成中…', '');
        qrPollDelay = 1500;
        pollLoop();
      } else if (status === 'scanned') {
        setQrStatus('已扫码，请在手机上确认', 'scanned');
        qrPollDelay = 450; // 已扫码：确认随时到达，提速轮询（见 qrPollDelay 注释）
        pollLoop();
      } else if (status === 'expired') {
        qrAutoRetry++;
        if (qrAutoRetry <= 3) {
          setQrStatus('已过期，重新生成…', 'expired');
          stopPolling();
          qrActive = false;
          startQrLogin();
        } else {
          setQrStatus('多次过期，请手动重新生成', 'expired');
          $('btnQrRefresh').style.display = '';
        }
      } else if (status === 'confirmed') {
        setQrStatus('登录成功', 'confirmed');
        try { config = await invoke('get_config'); } catch (e) {}
        qrActive = false;
        invoke('login_qr_cancel').catch(() => {});
        renderAll();
        log(channelLabel(channel) + '扫码登录成功' + (name ? '：' + name : ''), 'success');
        setTimeout(() => { $('loginOverlay').hidden = true; }, 800);
      }
    }, qrPollDelay);
  }

  function cancelQr() {
    stopPolling();
    if (qrActive) {
      qrActive = false;
      invoke('login_qr_cancel').catch(() => {});
      log('已取消扫码登录', 'info');
    }
  }

  // ---------- 统一登录弹窗：验证码 / 密码 ----------
  let loginMode = 'qr';   // qr | sms | pwd
  let credCodeTimer = null;
  function setCredStatus(t) { $('credStatus').textContent = t; }

  // 切换登录方式页签。
  function switchLoginMode(mode) {
    loginMode = mode;
    document.querySelectorAll('#loginModeSeg button').forEach((b) => {
      b.classList.toggle('on', b.dataset.mode === mode);
    });
    $('loginQrPage').hidden = mode !== 'qr';
    $('loginSmsPage').hidden = mode !== 'sms';
    $('loginPwdPage').hidden = mode !== 'pwd';
    $('btnQrRefresh').style.display = mode === 'qr' ? '' : 'none';
    $('btnCredLogin').style.display = mode === 'qr' ? 'none' : '';
    setCredStatus('');
    if (mode === 'qr') {
      qrAutoRetry = 0;
      pollFails = 0;
      startQrLogin();
    } else {
      cancelQr();
    }
  }

  // 第三方渠道登录（渠道扩展，Android）：调起渠道 SDK 登录 UI → 后端换取
  // combo/granter 凭证并保存渠道账号（channel_login 命令）。
  async function channelLogin(key) {
    const label = channelUiLabel(key);
    log('正在打开' + label + '登录…', 'info');
    try {
      const r = await invoke('channel_login', { channel: key, game: curGameId() });
      log(label + '账号已添加：' + (r.name || r.uid), 'info');
      config = await invoke('get_config');
      renderAll();
    } catch (e) {
      log(label + '登录失败：' + String(e), 'error');
    }
  }

  // 打开统一登录弹窗（桌面默认二维码；移动端不提供二维码登录——手机即本机，直接输账号）。
  function openLogin() {
    $('loginTitle').textContent = (filter === 'official' ? '官服' : 'B 服') + '账号登录';
    $('credPhone').value = ''; $('credCode').value = '';
    $('credAccount').value = ''; $('credPassword').value = '';
    $('loginOverlay').hidden = false;
    // B站不支持短信登录（风控限制）；移动端移除二维码页签。
    const isBili = filter === 'bilibili';
    const mobile = !!window.__MOBILE__;
    const segBtns = Array.from(document.querySelectorAll('#loginModeSeg button'));
    segBtns.forEach((b) => {
      b.hidden = (b.dataset.mode === 'sms' && isBili) || (b.dataset.mode === 'qr' && mobile);
    });
    // 可见页签数决定等分与显隐（仅剩一种方式时整段隐藏，避免孤立按钮）。
    const visibleN = segBtns.filter((b) => !b.hidden).length;
    $('loginModeSeg').hidden = visibleN <= 1;
    $('loginModeSeg').classList.toggle('two', visibleN === 2);
    switchLoginMode(mobile ? 'pwd' : 'qr');
  }

  document.querySelectorAll('#loginModeSeg button').forEach((b) => {
    b.addEventListener('click', () => switchLoginMode(b.dataset.mode));
  });
  function credCountdown() {
    const btn = $('btnCredSend');
    let left = 60;
    btn.disabled = true; btn.style.opacity = '0.55'; btn.textContent = left + 's';
    credCodeTimer = setInterval(() => {
      left--;
      if (left <= 0) {
        clearInterval(credCodeTimer); credCodeTimer = null;
        btn.disabled = false; btn.style.opacity = ''; btn.textContent = '发送验证码';
      } else { btn.textContent = left + 's'; }
    }, 1000);
  }
  async function doSendCode(aigis) {
    return invoke('cred_send_code', { channel: filter, phone: $('credPhone').value.trim(), aigis: aigis || '' });
  }
  $('btnCredSend').addEventListener('click', async () => {
    if (credCodeTimer) return;
    const phone = $('credPhone').value.trim();
    if (!phone) { setCredStatus('请输入手机号'); return; }
    setCredStatus('发送中…');
    try {
      const r = await doSendCode('');
      if (r && r.status === 'captcha') {
        setCredStatus('需要人机验证');
        showCaptcha(r.aigis, (aigis) => {
          setCredStatus('发送中…');
          doSendCode(aigis).then((r2) => {
            if (r2 && r2.status === 'captcha') { setCredStatus('需再次验证，请关闭后重试'); return; }
            setCredStatus('验证码已发送');
            credCountdown();
          }).catch((e) => setCredStatus('发送失败: ' + e));
        });
        return;
      }
      setCredStatus('验证码已发送');
      credCountdown();
    } catch (e) { setCredStatus('发送失败: ' + e); }
  });
  async function doCredLogin(aigis) {
    const game = curGameId();
    if (loginMode === 'sms') {
      const phone = $('credPhone').value.trim();
      const code = $('credCode').value.trim();
      return invoke('cred_login_sms', { channel: filter, game, phone, code, aigis: aigis || '' });
    }
    const account = $('credAccount').value.trim();
    const password = $('credPassword').value;
    return invoke('cred_login_pwd', { channel: filter, game, account, password, aigis: aigis || '' });
  }

  $('btnCredLogin').addEventListener('click', async () => {
    setCredStatus('登录中…');
    try {
      if (loginMode === 'sms') {
        if (!$('credPhone').value.trim() || !$('credCode').value.trim()) { setCredStatus('请填写手机号和验证码'); return; }
      } else {
        if (!$('credAccount').value.trim() || !$('credPassword').value) { setCredStatus('请填写账号和密码'); return; }
      }
      const r = await doCredLogin('');
      if (r && r.status === 'captcha') {
        setCredStatus('需要人机验证');
        showCaptcha(r.aigis, (aigis) => {
          setCredStatus('验证通过，正在登录…');
          doCredLogin(aigis).then((r2) => {
            if (r2 && r2.status === 'captcha') { setCredStatus('需再次验证，请关闭后重试'); return; }
            onCredSuccess();
          }).catch((e) => setCredStatus('登录失败: ' + e));
        });
        return;
      }
      onCredSuccess();
    } catch (e) { setCredStatus('登录失败: ' + e); }
  });

  function onCredSuccess() {
    invoke('get_config').then((c) => {
      config = c;
      renderAll();
      $('loginOverlay').hidden = true;
      log((filter === 'official' ? '官服' : 'B 服') + '账号登录成功', 'success');
    });
  }

  // ---------- 人机验证（极验 GT3 / GT4，应用内弹窗） ----------
  // 米哈游极验：aigis = { session_id, data: { gt, risk_type, ... } }。
  // B站游戏 SDK 极验：{gt, challenge, userid}（无 session_id）。
  //
  // 早期实现开独立 WebviewWindow 加载 verify.html —— 在 Android 上 wry 只有单个
  // Activity/WebView，第二窗口既渲染不出内容也关不掉（用户看到的就是「空白且
  // 无法返回」）。现改为与其它弹窗同构的 #captchaOverlay，在主 webview 内直接
  // 挂载极验 SDK；关闭走通用的 data-close / 点背景 / Esc / 返回键四条路径。
  let captchaRetry = null;    // 验证完成后重试的回调
  const captchaBox = () => $('captchaBox');
  function setCaptchaHint(msg) {
    const h = $('captchaHint');
    if (h) h.textContent = msg;
  }
  /// 极验诊断日志：**只写 logcat**（`frontend_log`），不进用户可见的应用内日志面板。
  ///
  /// 极验这条链路的失败形态是「静默」的（JSONP 被拦、回调不来 → 一片空白），
  /// 没有设备日志就只能靠猜；但这些过程信息对用户毫无价值，故与用户可见的
  /// `log()` 分开——只有真正的错误（`captchaFail`）才同时写面板。
  function captchaDiag(msg) {
    try { invoke('frontend_log', { level: 'info', msg: '[极验] ' + msg }); } catch (e) {}
  }
  /// 极验失败：写 logcat **并**告诉用户（这些是真需要用户知道/照做的）。
  function captchaFail(msg) {
    log(msg, 'error');
    try { invoke('frontend_log', { level: 'error', msg: '[极验] ' + msg }); } catch (e) {}
  }
  /// 页面来源诊断：GT3/GT4 SDK 的 `protocol` 默认取 `window.location.protocol + '//'`。
  /// Tauri 的 Android 页面来源是 **http://tauri.localhost**（真机实测），于是 SDK 会拼出
  /// `http://api.geetest.com/gettype.php` 这类**明文**请求；release 包
  /// `usesCleartextTraffic=false` 且 WebView 亦拦混合内容 → JSONP 永不回来、
  /// `initGeetest` 回调永不触发 → 弹窗一片空白（用户反馈的「无内容」）。
  /// 解法是在初始化参数里显式给 `https: true`（SDK 内该分支优先于 location.protocol）。
  /// 本函数留在日志里，便于日后来源变化时一眼看出。
  function captchaOriginInfo() {
    return 'origin=' + location.origin + ' protocol=' + location.protocol +
      ' secureContext=' + (window.isSecureContext === true);
  }
  // 极验 SDK 动态加载（GT3 与 GT4 是两套互不相同的入口，按需加载其一）。
  function loadGeetestScript(src, ready, cb) {
    if (ready()) { captchaDiag('SDK 已在页面中：' + src.split('/').pop()); cb(); return; }
    const s = document.createElement('script');
    s.src = src;
    s.onerror = () => {
      captchaFail('极验 SDK 加载失败：' + src);
      setCaptchaHint('极验 SDK 加载失败，请检查网络后重试');
    };
    s.onload = () => { captchaDiag('SDK 加载完成：' + src.split('/').pop()); cb(); };
    document.head.appendChild(s);
  }
  async function showCaptcha(aigis, retryFn) {
    let data = aigis && aigis.data !== undefined ? aigis.data : aigis;
    if (typeof data === 'string') {
      try { data = JSON.parse(data); } catch (e) { data = {}; }
    }
    if (!data || typeof data !== 'object') data = {};
    const sid = aigis && aigis.session_id ? aigis.session_id : '';
    const gt = data.gt || '';
    const challenge = data.challenge || '';
    const isGt3 = !!(gt && challenge && !sid);
    // B站把 userid 放在 session_id 通道；GT3 完成后回传 JSON
    // {challenge, userid, validate}。
    const biliUserId = data.userid || data.gt_user_id || '';
    const captchaId = data.captcha_id || data.captchaId || gt;
    const riskType = data.risk_type || data.riskType || '';
    if (!captchaId) {
      captchaFail('验证参数缺失：aigis=' + JSON.stringify(aigis).slice(0, 200));
      setCredStatus('验证参数缺失');
      if (retryFn) retryFn('');
      return;
    }
    captchaRetry = retryFn;
    // 上一轮的残影（同一次登录可能连续触发两次验证）——先清空舞台。
    const box = captchaBox();
    box.innerHTML = '';
    setCaptchaHint(isGt3 ? '请完成下方验证…' : '正在加载验证组件…');
    $('captchaOverlay').hidden = false;
    captchaDiag('弹窗已打开，代际=' + (isGt3 ? 'GT3' : 'GT4') + ' box=' + (box ? 'ok' : 'MISSING'));
    // 本次验证是否已有结论（成功交付 or 被用户关闭）。极验 SDK 的事件迟于弹窗
    // 关闭才到达是常态，故统一用 settled 闸门挡住「关窗后仍继续登录」。
    let settled = false;
    // 完成后统一出口：交给调用方的重试回调，并收起弹窗。
    function finish(result) {
      if (settled) return;
      settled = true;
      const fn = captchaRetry;
      captchaRetry = null;
      $('captchaOverlay').hidden = true;
      box.innerHTML = '';
      if (fn) fn(result);
    }
    // 用户主动关窗（关闭按钮 / 点背景 / Esc / 返回键都会置 hidden）：清掉回调，
    // 让迟到的 SDK 事件变成空操作。
    const closeWatch = new MutationObserver(() => {
      if (!$('captchaOverlay').hidden) return;
      closeWatch.disconnect();
      if (settled) return;
      settled = true;
      captchaRetry = null;
      box.innerHTML = '';
    });
    closeWatch.observe($('captchaOverlay'), { attributes: true, attributeFilter: ['hidden'] });
    if (isGt3) {
      // ---------- 极验第三代（B站）：gt + challenge ----------
      captchaDiag('GT3 初始化 gt=' + captchaId + ' challenge=' + challenge.slice(0, 16) + '… | ' + captchaOriginInfo());
      loadGeetestScript(
        'https://static.geetest.com/static/js/gt.0.4.9.js',
        () => !!window.initGeetest,
        () => {
          if ($('captchaOverlay').hidden) return;
          let done = false;
          try {
            window.initGeetest(
              {
                gt: captchaId,
                challenge,
                offline: false,
                new_captcha: true,
                // `https: true` 必须显式给：SDK 的 protocol 默认取
                // `window.location.protocol + '//'`，而 Tauri 的 Android 页面来源是
                // **http://tauri.localhost**（真机实测），于是它会去请求
                // `http://api.geetest.com/gettype.php` —— 明文请求被 WebView 与
                // usesCleartextTraffic=false 一并拦掉，JSONP 永不回来，initGeetest
                // 的回调永不触发，弹窗里就是一片空白（用户反馈的「无内容」）。
                // 该选项在 SDK 里优先于 location.protocol（见其 `if (userConfig.https)` 分支）。
                https: true,
                product: 'popup',
                lang: 'zh-cn',
                // api_server 不带协议前缀（SDK 自己拼 protocol），下方 protocol 已由
                // https:true 固定为 https://。
                api_server: 'api.geetest.com',
              },
              (captcha) => {
                captchaDiag('GT3 实例已回调，appendTo 挂载');
                captcha.appendTo(box);
                captcha.onReady(() => {
                  captchaDiag('GT3 onReady（控件已渲染）');
                  setCaptchaHint('请完成验证');
                  // onReady 后主动唤起验证面板（参考 BBH3ScanLaunch）。
                  try { captcha.verify(); } catch (e) { captchaFail('极验 verify 异常: ' + e); }
                });
                captcha.onSuccess(() => {
                  if (done) return;
                  done = true;
                  const v = captcha.getValidate();
                  if (!v) { setCaptchaHint('验证结果为空，请关闭后重试'); return; }
                  captchaDiag('GT3 onSuccess');
                  setCaptchaHint('验证通过，正在继续…');
                  finish(JSON.stringify({
                    challenge: v.geetest_challenge,
                    userid: biliUserId,
                    validate: v.geetest_validate,
                  }));
                });
                captcha.onError((e) => {
                  captchaFail('GT3 onError: ' + JSON.stringify(e));
                  if (!done) setCaptchaHint('验证出错，请关闭后重试');
                });
                // 某些 GT3 环境完成后只触发 onClose，故这里再读一次结果。
                captcha.onClose(() => {
                  if (done) return;
                  const v = captcha.getValidate();
                  captchaDiag('GT3 onClose，getValidate=' + JSON.stringify(v));
                  if (v && v.geetest_validate) {
                    done = true;
                    finish(JSON.stringify({
                      challenge: v.geetest_challenge,
                      userid: biliUserId,
                      validate: v.geetest_validate,
                    }));
                  } else {
                    setCaptchaHint('验证已取消');
                  }
                });
              }
            );
          } catch (e) {
            captchaFail('GT3 initGeetest 抛异常: ' + e);
            setCaptchaHint('极验初始化失败，请关闭后重试');
          }
        }
      );
      return;
    }
    // ---------- 极验第四代（米哈游）：captchaId ----------
    captchaDiag('GT4 初始化 captchaId=' + captchaId + ' riskType=' + (riskType || '-') + ' | ' + captchaOriginInfo());
    loadGeetestScript(
      'https://static.geetest.com/v4/gt4.js',
      () => !!window.initGeetest4,
      () => {
        if ($('captchaOverlay').hidden) return;
        // 参数对齐米哈游通行证登录页真实初始化：product bind + hideSuccess + userInfo。
        // `https: true` 与 GT3 同理：页面来源是 http://tauri.localhost 时，若不显式指定，
        // SDK 会拼出 http://gcaptcha4.geetest.com 的明文请求并被拦掉（真机实测）。
        const initParams = {
          captchaId, product: 'bind', language: 'zho', hideSuccess: true, https: true,
        };
        if (sid) initParams.userInfo = JSON.stringify({ session_id: sid });
        if (riskType) initParams.riskType = riskType;
        let done4 = false;
        try {
          window.initGeetest4(initParams, (captcha) => {
            captchaDiag('GT4 实例已回调，appendTo 挂载');
            captcha.appendTo(box);
            captcha.onReady(() => {
              captchaDiag('GT4 onReady（控件已渲染）');
              setCaptchaHint('请完成验证');
              captcha.showCaptcha();
            });
            captcha.onSuccess(() => {
              if (done4) return;
              done4 = true;
              const v = captcha.getValidate();
              if (!v) { setCaptchaHint('验证结果为空，请关闭后重试'); return; }
              captchaDiag('GT4 onSuccess');
              // 只保留 GT4 的 5 个标准字段（userInfo 会污染 aigis）。
              const clean = {
                captcha_id: v.captcha_id,
                lot_number: v.lot_number,
                pass_token: v.pass_token,
                gen_time: v.gen_time,
                captcha_output: v.captcha_output,
              };
              const payload = btoa(unescape(encodeURIComponent(JSON.stringify(clean))));
              setCaptchaHint('验证通过，正在继续…');
              finish(sid + ';' + payload);
            });
            captcha.onError((e) => {
              captchaFail('GT4 onError: ' + JSON.stringify(e));
              if (!done4) setCaptchaHint('验证出错，请关闭后重试');
            });
            captcha.onClose(() => {
              captchaDiag('GT4 onClose');
              if (!done4) setCaptchaHint('验证已取消');
            });
          });
        } catch (e) {
          captchaFail('GT4 initGeetest4 抛异常: ' + e);
          setCaptchaHint('极验初始化失败，请关闭后重试');
        }
      }
    );
  }

  // ---------- 鹰角账号登录 ----------
  let hyperCodeTimer = null;
  function startHyperCountdown() {
    const btn = $('btnHyperSend');
    let left = 60;
    btn.disabled = true;
    btn.style.opacity = '0.55';
    btn.textContent = left + 's';
    hyperCodeTimer = setInterval(() => {
      left--;
      if (left <= 0) {
        clearInterval(hyperCodeTimer);
        hyperCodeTimer = null;
        btn.disabled = false;
        btn.style.opacity = '';
        btn.textContent = '发送验证码';
      } else {
        btn.textContent = left + 's';
      }
    }, 1000);
  }
  $('btnHyperSend').addEventListener('click', async () => {
    if (hyperCodeTimer) return;
    const phone = $('hyperPhone').value.trim();
    if (!phone) { $('hyperStatus').textContent = '请输入手机号'; return; }
    $('hyperStatus').textContent = '发送中…';
    try {
      await invoke('hypergryph_send_code', { phone });
      $('hyperStatus').textContent = '验证码已发送';
      startHyperCountdown();
    } catch (e) { $('hyperStatus').textContent = '发送失败: ' + e; }
  });
  $('btnHyperLogin').addEventListener('click', async () => {
    const phone = $('hyperPhone').value.trim();
    const code = $('hyperCode').value.trim();
    if (!phone || !code) { $('hyperStatus').textContent = '请填写手机号和验证码'; return; }
    $('hyperStatus').textContent = '登录中…';
    try {
      await invoke('hypergryph_login', { phone, code });
      config = await invoke('get_config');
      renderAll();
      $('hyperOverlay').hidden = true;
      log('鹰角账号登录成功：' + phone, 'success');
    } catch (e) { $('hyperStatus').textContent = '登录失败: ' + e; }
  });

  // ---------- 按钮绑定 ----------
  $('btnQrRefresh').addEventListener('click', () => {
    qrAutoRetry = 0;
    pollFails = 0;
    cancelQr();
    startQrLogin();
  });
  $('btnRemarkSave').addEventListener('click', saveRemarkDialog);
  $('remarkAlias').addEventListener('keydown', (e) => { if (e.key === 'Enter') $('remarkText').focus(); });
  $('remarkText').addEventListener('keydown', (e) => { if (e.key === 'Enter') saveRemarkDialog(); });
  // 渠道筛选切换（官服 / B 服）。
  document.querySelectorAll('#filterSeg button').forEach((b) => {
    b.addEventListener('click', () => {
      filter = b.dataset.f;
      renderFilter();
      renderAccounts();
    });
  });

  // ---------- 侧栏宽度拖拽 ----------
  (function initVSplit() {
    const sp = $('vsplit');
    const side = document.querySelector('.sidebar');
    if (!sp || !side) return;
    let dragging = false;
    sp.addEventListener('mousedown', (e) => {
      e.preventDefault();
      dragging = true;
      sp.classList.add('dragging');
      document.body.style.cursor = 'col-resize';
    });
    window.addEventListener('mousemove', (e) => {
      if (!dragging) return;
      const appRect = document.querySelector('.app').getBoundingClientRect();
      let w = e.clientX - appRect.left;
      w = Math.min(Math.max(w, 140), Math.min(240, appRect.width - 320));
      side.style.width = w + 'px';
      sizeCanvasToDisplay();
    });
    window.addEventListener('mouseup', () => {
      if (!dragging) return;
      dragging = false;
      sp.classList.remove('dragging');
      document.body.style.cursor = '';
      // 拖动结束：列宽落盘（下次启动恢复）。
      invoke('set_ui_widths', { sidebar: Math.round(side.getBoundingClientRect().width) }).catch(() => {});
    });
  })();

  // ---------- 账号列 / 游戏列 分界拖拽 ----------
  // 拖动调整右列（渠道切换 + 游戏列表）宽度，限幅 70~140px（默认 110px，见 CSS）。
  // 宽度持久化：拖动结束落盘（`set_ui_widths`），下次启动恢复。
  const GAME_COL_MIN = 70, GAME_COL_MAX = 140;
  // 左列宽度低于该值时：渠道按钮文案降级为「官 / B」（两个中文字在窄列里放不下）。
  const CHAN_NARROW_AT = 76;
  function updateChanLabels() {
    const seg = $('filterSeg'), wrap = document.querySelector('.game-wrap');
    if (!seg || !wrap) return;
    seg.classList.toggle('narrow', wrap.clientWidth < CHAN_NARROW_AT);
  }
  (function initGSplit() {
    const sp = $('gSplit'), wrap = document.querySelector('.game-wrap');
    if (!sp || !wrap) return;
    let dragging = false;
    sp.addEventListener('mousedown', (e) => {
      e.preventDefault();
      dragging = true;
      sp.classList.add('dragging');
      document.body.style.cursor = 'col-resize';
    });
    window.addEventListener('mousemove', (e) => {
      if (!dragging) return;
      // 游戏列在账号列右侧：宽度 = 列右缘 − 光标（向左拖变宽）。
      const right = wrap.getBoundingClientRect().right;
      const w = Math.min(Math.max(Math.round(right - e.clientX), GAME_COL_MIN), GAME_COL_MAX);
      wrap.style.width = w + 'px';
      updateChanLabels();
    });
    window.addEventListener('mouseup', () => {
      if (!dragging) return;
      dragging = false;
      sp.classList.remove('dragging');
      document.body.style.cursor = '';
      // 拖动结束：列宽落盘（下次启动恢复）。
      invoke('set_ui_widths', { gameCol: Math.round(wrap.getBoundingClientRect().width) }).catch(() => {});
    });
    updateChanLabels();
  })();

  // ---------- 扫描 ----------
  // Android：确保屏幕捕获已授权（已捕获时后端直接返回，不重复弹框；
  // 命令不可用的环境——桌面/浏览器预览——直接放行，不拦截原有流程）。
  async function ensureScreenCapture() {
    if (!window.__MOBILE__) return true;
    let capturing = false;
    try {
      capturing = await invoke('screen_capture_state');
    } catch (_) {
      return true;
    }
    if (capturing) return true;
    try {
      await invoke('request_screen_capture');
      log('屏幕捕获已就绪', 'info');
      return true;
    } catch (e) {
      log('屏幕捕获授权失败或已取消：' + String(e), 'error');
      return false;
    }
  }
  // 当前视图对应的扫码目标："桌面" → "screen"；直播会话 → "live:{id}"；
  // 选中的未开播房间（无会话，无可扫画面）→ null。
  function activeScanTarget() {
    if (!activeLiveId) return 'screen';
    return activeSession() ? ('live:' + activeLiveId) : null;
  }
  // 请求停止一路扫码（按钮与自动停止共用；幂等）。
  function stopScanTarget(t) {
    if (!scanningTargets.has(t) || stoppingTargets.has(t)) return;
    stoppingTargets.add(t);
    renderScan();
    invoke('stop_scan', { target: t }).catch(() => {});
    // 兜底：后端若未回 stopped 事件（目标已不存在 / 事件丢失），3 秒后强制清理，
    // 否则按钮永久卡在「停止中…」，无法回到「开始扫码」。
    setTimeout(() => {
      if (stoppingTargets.has(t)) {
        stoppingTargets.delete(t);
        scanningTargets.delete(t);
        renderScan();
      }
    }, 3000);
  }
  $('btnStart').addEventListener('click', () => {
    const t = activeScanTarget();
    if (!t) {
      log('该直播间未开播：开播后会自动连接，随后即可扫码', 'warn');
      return;
    }
    if (scanningTargets.has(t)) {
      if (stoppingTargets.has(t)) return;
      stopScanTarget(t);
      log('已请求停止…', 'info');
    } else {
      invoke('start_scan', { target: t }).catch((e) => log(String(e), 'error'));
    }
  });

  // ---------- 扫描区域（实时监视器） ----------
  let srcCanvas = null;
  let prevRegionStr = null;
  let previewTimer = null;
  let previewErrShown = false;
  // 画布内容是否已被清空（尺寸重建 / 强制重栅格化）：供 drawView 的冻结守卫
  // 放行一次兜底绘制，避免「画布已清空 + 冻结」组合下整屏保持黑色。
  let canvasBlank = false;
  const canvas = $('previewCanvas');
  // 缓存 2D 上下文（getContext 每帧调用有查找开销）。
  const canvasCtx = canvas.getContext('2d');

  // 视窗布局：按 16:9 在「主列宽度」与「扣除信息栏+账号区最小高度后的可用高度」中取小，
  // 等比适配（letterbox），绝不拉伸变形。账号区保留 min 120px。
  const ACCT_MIN_H = 120;
  const VP_INFO_H = 34;
  function layoutViewport() {
    // 移动端：宽高由 CSS（aspect-ratio: 3/2）控制，不走桌面 16:9 计算。
    if (window.__MOBILE__) return;
    const main = document.querySelector('.main-col');
    const fit = $('vpFit'), stage = $('vpStage');
    if (!main || !fit || !stage) return;
    const mainW = main.clientWidth;
    const mainH = main.clientHeight;
    if (mainW <= 0 || mainH <= 0) return;
    const availH = Math.max(60, mainH - VP_INFO_H - ACCT_MIN_H);
    const ar = 16 / 9;
    let w = mainW, h = mainW / ar;
    if (h > availH) { h = availH; w = availH * ar; }
    w = Math.round(w); h = Math.round(h);
    if (stage.style.width !== w + 'px') stage.style.width = w + 'px';
    if (stage.style.height !== h + 'px') stage.style.height = h + 'px';
    if (fit.style.height !== h + 'px') fit.style.height = h + 'px';
  }

  function sizeCanvasToDisplay() {
    layoutViewport();
    const rect = canvas.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    let w = Math.round(Math.min(Math.max((rect.width || 480) * dpr, 320), 4096));
    w -= w % 4;
    // 移动端按实际显示区比例算分辨率（3:2）；桌面固定 16:9。
    const ar = (window.__MOBILE__ && rect.height > 0) ? (rect.width / rect.height) : (16 / 9);
    const h = Math.round(w / ar);
    if (w !== canvas.width || h !== canvas.height) {
      canvas.width = w;
      canvas.height = h;
      // 改 width/height 会清空画布内容。若此时恰逢底图冻结守卫（见 drawView），
      // 不重绘就会整屏保持黑——标记「画布为空」让守卫放行一次兜底绘制。
      canvasBlank = true;
    }
  }
  sizeCanvasToDisplay();
  window.addEventListener('resize', () => sizeCanvasToDisplay());
  const canvasWrap = canvas.parentElement;
  let settleTimer = null;
  function forceFullRaster() {
    const w = canvas.width, h = canvas.height;
    if (w === 0 || h === 0) return;
    canvas.width = 0; canvas.height = 0;
    canvas.width = w; canvas.height = h;
    canvasBlank = true;
    drawView();
  }
  const canvasRO = new ResizeObserver(() => {
    sizeCanvasToDisplay();
    drawView();
    if (settleTimer) clearTimeout(settleTimer);
    settleTimer = setTimeout(forceFullRaster, 120);
  });
  canvasRO.observe(document.querySelector('.main-col') || canvasWrap);
  let dprQuery = null;
  function onDprChange() {
    const oldW = canvas.width;
    sizeCanvasToDisplay();
    if (canvas.width !== oldW) drawView();
    watchDpr();
  }
  function watchDpr() {
    if (dprQuery) dprQuery.removeEventListener('change', onDprChange);
    dprQuery = window.matchMedia('(resolution: ' + (window.devicePixelRatio || 1) + 'dppx)');
    dprQuery.addEventListener('change', onDprChange);
  }
  watchDpr();

  function viewportFrac(vv) {
    const v = vv || view;
    const z = Math.max(v.scale, 1);
    // scale=1 显示完整流（letterbox 由 contentRect 负责居中），缩放时裁剪。
    const fw = 1 / z;
    const fh = 1 / z;
    let x0 = v.fx - fw / 2;
    let y0 = v.fy - fh / 2;
    x0 = Math.min(Math.max(x0, 0), 1 - fw);
    y0 = Math.min(Math.max(y0, 0), 1 - fh);
    return { x0, y0, fw, fh };
  }

  function viewportMonRect() {
    if (!preview) return null;
    const v = viewportFrac();
    const m = preview.mon;
    const X0 = m.x + Math.round(v.x0 * m.w);
    const Y0 = m.y + Math.round(v.y0 * m.h);
    const X1 = m.x + Math.round((v.x0 + v.fw) * m.w);
    const Y1 = m.y + Math.round((v.y0 + v.fh) * m.h);
    return { x: X0, y: Y0, w: X1 - X0, h: Y1 - Y0 };
  }

  // ---------- 直播源（B站 / 抖音低延迟画面，多路监控） ----------
  // 会话列表：[{ id, room, platform, w, h }]；activeLiveId 为当前主画面。
  let liveSessions = [];
  let activeLiveId = null;
  // 「当前是否在观看直播会话」（有前台会话），而非「是否有任何会话」。
  // 后者会导致：切到桌面但仍有会话时，预览既不拉直播帧也不拉屏幕帧。
  function liveOn() { return activeLiveId !== null; }
  function activeSession() { return liveSessions.find((s) => s.id === activeLiveId) || null; }
  function livePlatformLabel(p) { return (p === 'douyin') ? '抖音' : 'B站'; }

  // ---------- 通知栏（Android；桌面为空实现） ----------
  //
  // 手机切到游戏里扫码时应用界面不可见——通知栏是唯一的状态出口：
  //   常驻：后台是否在扫码 / 正在扫哪些直播间（回前台立刻撤下）；
  //   即时：扫码成功、直播间开播下播等一次性提醒（可划掉）。
  //
  // 均受设置开关控制（notify_persistent / notify_live_status），且整条链路静默降级：
  // 通知失败不该影响扫码本身，故所有 invoke 都吞掉异常。
  let persistentNotifyShown = false;

  /// 后台扫码状态的文案：正在扫哪些目标（屏幕 / 各直播间）与各自游戏。
  function scanStatusText() {
    const parts = [];
    if (scanningTargets.has('screen')) parts.push('屏幕 · ' + gameLabel(curGameId()));
    for (const t of scanningTargets) {
      if (!t.startsWith('live:')) continue;
      const id = t.slice(5);
      const s = liveSessions.find((x) => x.id === id);
      const room = liveRooms.find((r) => (r.platform + ':' + r.room) === id);
      const name = (room && (room.note || room.title)) || (s && s.room) || id;
      parts.push(name);
    }
    // 摄像头取景也算一路（虽不占 scanningTargets 的语义，但对用户是「在扫码」）。
    if (cameraMode) parts.push('摄像头');
    return parts;
  }

  /// 同步常驻通知：后台且有扫码任务时显示/更新，否则撤下。
  function syncPersistentNotification() {
    if (!window.__ANDROID__) return;
    if (!notifyPersistent || !document.hidden) {
      if (persistentNotifyShown) {
        invoke('notify_persistent_hide').catch(() => {});
        persistentNotifyShown = false;
      }
      return;
    }
    const parts = scanStatusText();
    const scanning = parts.length > 0;
    const title = scanning ? '正在扫码' : '扫码器在后台';
    const text = scanning ? parts.join(' · ') : '当前没有进行中的扫码任务';
    // 点通知回到应用时尽量定位到"当前在看/在扫的那个直播间"：
    // 优先正在扫的直播目标，其次当前观看的，都没有则只回主界面。
    let openTarget = '';
    for (const t of scanningTargets) {
      if (String(t).startsWith('live:')) { openTarget = String(t); break; }
    }
    if (!openTarget && activeLiveId) openTarget = 'live:' + activeLiveId;
    invoke('notify_persistent_show', { title, text, target: openTarget }).catch(() => {});
    persistentNotifyShown = true;
  }

  /// 点通知 → 定位到对应直播间（由 Android 侧 MainActivity 经
  /// `window.__gsOpenTarget` 调用；参数形如 `live:bilibili:6`）。
  window.__gsOpenTarget = function (target) {
    try {
      const s = String(target || '');
      if (!s.startsWith('live:')) return true;
      const id = s.slice(5);   // `bilibili:6`
      const known = liveRooms.some((r) => (r.platform + ':' + r.room) === id);
      if (known) {
        switchLive(id).catch(() => {});
        log('已从通知打开直播间 ' + id, 'info');
        return true;
      }
      // 房间可能已被删除：仍切到它（后端会按 id 建会话），失败则忽略。
      switchLive(id).catch(() => {});
      return true;
    } catch (e) { return false; }
  };

  /// 点通知上的「停止扫码」→ 停掉所有扫码路（含屏幕共享），
  /// 由 Android 侧 MainActivity 经 `window.__gsStopScanFromNotification` 调用。
  window.__gsStopScanFromNotification = function () {
    try {
      const targets = [...scanningTargets];
      for (const t of targets) stopScanTarget(t);
      if (targets.length) log('已从通知停止扫码（' + targets.length + ' 路）', 'info');
      // 屏幕路一并释放捕获，否则前台服务与常驻通知会一直挂着。
      if (targets.includes('screen')) stopScreenShare({ quiet: true });
      syncPersistentNotification();
      return true;
    } catch (e) { return false; }
  };

  /// 发一条即时通知。`kind` ∈ scan_success | live_status | scan_error。
  ///
  /// `target` 为扫码目标键（`screen` / `camera` / `live:<platform>:<room>`）——除了
  /// 决定文案，也作为**点击跳转目标**传给系统通知：用户点"扫码成功"通知时应直接
  /// 落到那个直播间，而不是自己再翻侧栏找。
  ///
  /// **成功通知做去抖合并**：多路并行扫码时，几个直播间可能在同一瞬间各命中一个码
  /// （不同直播间各自的二维码）。逐条发会在一秒内刷出多条通知，既吵又难看清。
  /// 这里把短窗口内的多次成功合成一条汇总（"3 个直播间已确认"），更符合"扫完一轮"
  /// 的语义；错误/下播等事件不合并（各自是独立信息）。
  function notifyEvent(kind, target, message) {
    if (!window.__ANDROID__) return;
    if (!notifyLiveStatus) return;
    if (kind === 'success') {
      queueSuccessNotification(target);
      return;
    }
    let title = '扫码器';
    let text = message || '';
    const openTarget = String(target || '').startsWith('live:') ? String(target) : '';
    if (kind === 'stopped') {
      title = '扫码已停止';
      text = message || '已停止扫描';
    } else if (kind === 'error' || kind === 'failed') {
      title = '扫码失败';
      text = message || '扫描过程中出现问题';
    } else {
      return; // started 等过程事件不发即时通知，避免刷屏
    }
    invoke('notify_event', {
      kind: kind === 'success' ? 'scan_success' : 'live_status',
      title, text, target: openTarget,
    }).catch(() => {});
  }

  // 成功通知合并窗口（毫秒）：并行多路在此时段内的成功合并为一条。
  const SCAN_SUCCESS_MERGE_MS = 1200;
  let successMergeTimer = null;
  let successMergeTargets = [];

  // 通知观测计数（供排障与自动化验证；`window.__gsNotifyStats` 可读）。
  // 现场排查「到底发没发通知 / 合并了几条」时，比翻 logcat 更直接。
  window.__gsNotifyStats = {
    queued: 0,      // 进入合并队列的成功次数
    flushes: 0,     // 实际发出的成功通知条数
    merged: 0,      // 被合并（即一条通知覆盖多个目标）的次数
    lastTargets: [],// 最近一次 flush 的目标列表
  };

  /// 把一次扫码成功并入待发队列；窗口结束后统一发一条（单条则直述该目标）。
  function queueSuccessNotification(target) {
    if (!successMergeTargets.includes(target)) successMergeTargets.push(target);
    window.__gsNotifyStats.queued++;
    if (successMergeTimer !== null) return;
    successMergeTimer = setTimeout(() => {
      const targets = successMergeTargets;
      successMergeTargets = [];
      successMergeTimer = null;
      flushSuccessNotification(targets);
    }, SCAN_SUCCESS_MERGE_MS);
  }

  /// 发出合并后的成功通知。
  function flushSuccessNotification(targets) {
    if (!targets.length) return;
    // 统计：本次发出的条数与是否发生了合并（自动化验证与现场排障都用它）。
    const st = window.__gsNotifyStats;
    st.flushes++;
    if (targets.length > 1) st.merged++;
    st.lastTargets = targets.slice();
    let title = '扫码成功';
    let text = '';
    let openTarget = '';
    if (targets.length === 1) {
      const t = targets[0];
      if (t === 'screen') text = '屏幕扫码已识别并确认登录';
      else if (t === 'camera') text = '摄像头扫码已识别并确认登录';
      else {
        const id = String(t).startsWith('live:') ? String(t).slice(5) : '';
        const room = liveRooms.find((r) => (r.platform + ':' + r.room) === id);
        const name = (room && (room.note || room.title)) || id;
        text = (name ? name + '：' : '') + '已识别并确认登录';
      }
      openTarget = String(t).startsWith('live:') ? String(t) : '';
    } else {
      // 多路同时成功：列出名字（过多时收敛为计数），点击落到第一个直播间。
      const names = targets.map((t) => {
        if (t === 'screen') return '屏幕';
        if (t === 'camera') return '摄像头';
        const id = String(t).slice(5);
        const room = liveRooms.find((r) => (r.platform + ':' + r.room) === id);
        return (room && (room.note || room.title)) || id;
      });
      title = '扫码成功（' + targets.length + ' 个）';
      text = names.length <= 4 ? names.join('、') + ' 均已确认登录'
        : names.slice(0, 3).join('、') + ' 等 ' + targets.length + ' 个已确认登录';
      for (const t of targets) {
        if (String(t).startsWith('live:')) { openTarget = String(t); break; }
      }
    }
    invoke('notify_event', {
      kind: 'scan_success', title, text, target: openTarget,
    }).catch(() => {});
  }

  // 更新视窗信息栏（标题/副标题）。数据源优先前台房间（含未开播）。
  function updateVpInfo() {
    const ttl = $('vpTitle'), sub = $('vpSub');
    if (!ttl) return;
    // 摄像头取景模式：标题栏改为「摄像头扫码」（顶栏标题同步），直播会话状态不变。
    if (cameraMode) {
      ttl.textContent = '摄像头扫码';
      sub.textContent = '取景中';
      if (window.updateMobileTitle) window.updateMobileTitle('摄像头扫码', '取景中');
      return;
    }
    if (!activeLiveId) {
      ttl.textContent = '屏幕预览';
      sub.textContent = '未连接直播';
      if (window.updateMobileTitle) window.updateMobileTitle('屏幕预览', '未连接直播');
      return;
    }
    const room = liveRooms.find((r) => (r.platform + ':' + r.room) === activeLiveId);
    const sess = activeSession();
    const platform = room ? room.platform : (sess ? sess.platform : '');
    const roomNo = room ? room.room : (sess ? sess.room : '');
    const title = (room && room.title) || (sess && sess.title) || roomNo;
    // 与左列同规则：有备注时显示「备注（原标题）」。
    const note = (room && room.note) || '';
    ttl.textContent = note ? (note + '（' + title + '）') : title;
    // 移动端标题栏：主标题（含备注）+ 副标题（平台 房间号）。
    if (window.updateMobileTitle) window.updateMobileTitle(ttl.textContent, livePlatformLabel(platform) + ' ' + roomNo);
    // 副标题：平台 房间号 · 时长（主播名只在左列与悬浮提示出现）。
    const parts = [];
    parts.push(livePlatformLabel(platform) + ' ' + roomNo);
    const duration = (room && room.live_start) ? fmtDuration(room.live_start) : (sess && sess.duration ? sess.duration : '');
    if (duration) parts.push('时长 ' + duration);
    sub.textContent = parts.join(' · ');
  }

  // 开播时长文本（H:MM:SS）。
  function fmtDuration(start) {
    if (!start || start <= 0) return '';
    const secs = Math.max(0, Math.floor(Date.now() / 1000) - start);
    const h = Math.floor(secs / 3600), m = Math.floor((secs % 3600) / 60), s = secs % 60;
    const p = (n) => String(n).padStart(2, '0');
    return h + ':' + p(m) + ':' + p(s);
  }

  // 已保存直播间列表（来自 config.live_history，含元数据缓存；含未开播）。
  let liveRooms = [];

  // 直播间元数据获取状态（id -> 'pending' | 'missing'）。
  //   pending：刚添加/正在拉取——列表与预览先显示「正在获取直播间…」占位；
  //   missing：服务端明确回「房间不存在」——显示「房间不存在，请检查」。
  // 只为**确实在拉取**或**确认不存在**的房间留痕；取到数据即删除条目（恢复正常渲染）。
  const roomFetchState = new Map();
  // 判定「房间不存在」的错误特征：后端 get_info 返回 code=1 时消息为「未找到该房间」，
  // 抖音侧为「房间不存在」。仅这些**终态**错误才落到 missing；网络/风控等瞬时失败
  // 不应误报成「房间不存在」，故保持正常渲染（沿用缓存/占位）。
  function isRoomMissingError(msg) {
    const s = String(msg || '');
    return /未找到该房间|房间不存在|code=1（|code=1\(/.test(s);
  }

  // 拉取已保存直播间列表（含元数据缓存），保持本地顺序（避免点击后跳序）。
  async function reloadLiveRooms() {
    try {
      const cfg = await invoke('get_config');
      const incoming = cfg.live_history || [];
      const byId = new Map(incoming.map((r) => [r.platform + ':' + r.room, r]));
      // 保持本地已有顺序；合并元数据（保留本地非空值，避免瞬时清空）；追加新房间。
      const merged = [];
      for (const r of liveRooms) {
        const id = r.platform + ':' + r.room;
        const inc = byId.get(id);
        if (inc) {
          // 后端返回的字段可能瞬时为空（刷新中）——保留本地非空值。
          r.title = inc.title || r.title;
          r.nickname = inc.nickname || r.nickname;
          r.cover = inc.cover || r.cover;
          r.room_id = inc.room_id || r.room_id;
          r.live_status = inc.live_status;
          r.note = inc.note || r.note;
          merged.push(r);
          byId.delete(id);
        }
      }
      for (const r of byId.values()) merged.push(r);
      liveRooms = merged;
    } catch (e) {
      // 拉取失败：保留现有列表，但必须让用户知道界面可能已与后端不一致。
      log('加载直播间列表失败: ' + e, 'error');
    }
    renderLiveTabs();
    syncWatchers();
  }

  // 刷新单个直播间的权威元数据（标题 / 主播 / 封面 / 开播状态）。
  //
  // 调用方通常不 await：失败多为瞬时网络问题，静默即可（30s 兜底轮询会补上），
  // 唯一要显式处理的是「房间不存在」这个终态——落到 missing 占位并提示用户。
  // （此前 switchLive / 开播推送 / 画面刷新三处各写一份，本函数即其合并。）
  function refreshRoomMeta(id) {
    const sep = id.indexOf(':');
    const platform = id.slice(0, sep);
    const room = id.slice(sep + 1);
    return invoke('live_refresh_meta', { platform, room }).then((m) => {
      const local = liveRooms.find((x) => (x.platform + ':' + x.room) === id);
      if (local && m) {
        local.title = m.title || local.title;
        local.nickname = m.nickname || local.nickname;
        local.cover = m.cover || local.cover;
        local.room_id = m.room_id || local.room_id;
        local.live_status = m.live_status;
      }
      // 拿到数据即撤掉占位（顺带让「不存在」的房间在改名后恢复正常）。
      if (roomFetchState.delete(id)) renderLiveTabs();
      if (activeLiveId === id) {
        updateVpInfo();
        if (coverShown) showCover(id);   // 仍在占位态：同步刷新封面/状态文案
      }
      return m;
    }).catch((e) => {
      // 点开一个已存的坏房间（房间号被删/填错）：同样落到「房间不存在，请检查」。
      if (isRoomMissingError(e)) {
        roomFetchState.set(id, 'missing');
        renderLiveTabs();
        if (activeLiveId === id) { updateVpInfo(); if (coverShown) showCover(id); }
      }
      return null;
    });
  }

  // 渲染侧栏直播间列表（"桌面"卡片 + 已保存直播间卡片 + 添加按钮）。
  function renderLiveTabs() {
    const box = $('roomList');
    box.innerHTML = '';
    // 1) "桌面"卡片：切回屏幕预览。
    const desk = document.createElement('div');
    desk.className = 'room-card' + (activeLiveId ? '' : ' on');
    const dthumb = document.createElement('div');
    dthumb.className = 'thumb';
    dthumb.innerHTML = '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" class="ph"><rect x="3" y="4" width="18" height="12" rx="1.5"/><path d="M8 20h8M12 16v4"/></svg>';
    const dmeta = document.createElement('div');
    dmeta.className = 'meta';
    const dn = document.createElement('div');
    dn.className = 'rname';
    dn.textContent = '桌面';
    const ds = document.createElement('div');
    ds.className = 'rsub';
    ds.textContent = '屏幕预览';
    dmeta.append(dn, ds);
    desk.append(dthumb, dmeta);
    desk.addEventListener('mousedown', () => exitLiveMode());
    // 移动端不显示"桌面"卡片（手机无桌面屏幕预览场景）。
    if (!window.__MOBILE__) box.appendChild(desk);

    // 2) 已保存直播间卡片（封面/标题/主播/状态）。含未开播。
    for (const s of liveRooms) {
      const id = s.platform + ':' + s.room;
      const sess = liveSessions.find((x) => x.id === id);
      const active = activeLiveId === id;
      const live = (sess ? sess.status : (s.live_status === 1 ? 'live' : 'off'));
      // 元数据尚未就绪（刚添加）或已确认不存在：标题与状态徽标改用对应占位文案，
      // 且不显示封面/缩略图（此时并无真实封面可用）。
      const fstate = roomFetchState.get(id);
      const card = document.createElement('div');
      card.className = 'room-card' + (active ? ' on' : '');
      card.draggable = true;
      card.dataset.id = id;
      const thumb = document.createElement('div');
      thumb.className = 'thumb';
      // 封面优先级：API 封面 > 关键帧缩略图 > 占位。
      const coverSrc = fstate ? '' : (s.cover || frameThumbs.get(id) || '');
      if (coverSrc) {
        const img = document.createElement('img');
        img.src = coverSrc;
        img.alt = '';
        img.referrerPolicy = 'no-referrer';
        img.addEventListener('error', (ev) => {
          ev.target.remove();
          thumb.insertAdjacentHTML('afterbegin', '<span class="ph">●</span>');
        });
        thumb.appendChild(img);
      } else {
        thumb.innerHTML = '<span class="ph">●</span>';
      }
      const meta = document.createElement('div');
      meta.className = 'meta';
      const rn = document.createElement('div');
      rn.className = 'rname';
      // 第一行：有备注时显示「备注（原标题）」（与账号名同规则）；无备注显示标题。
      // 正在获取 / 房间不存在时，标题行改为对应的占位文案（覆盖备注拼接）。
      const origTitle = s.title || s.room;
      const dispName = s.note ? (s.note + '（' + origTitle + '）') : origTitle;
      rn.textContent = fstate === 'pending' ? '正在获取直播间…'
        : fstate === 'missing' ? '房间不存在，请检查'
        : dispName;
      const rs = document.createElement('div');
      rs.className = 'rsub';
      // 第二行：主播名（无主播名时退化为「平台 + 房间号」）。
      rs.textContent = s.nickname || (livePlatformLabel(s.platform) + ' ' + s.room);
      // 悬浮提示：备注（直播间名）-主播名（无主播名时省略后缀）。
      card.title = s.nickname ? (dispName + '-' + s.nickname) : dispName;
      meta.append(rn, rs);
      const st = document.createElement('span');
      const stCls = fstate === 'missing' ? 'off'
        : fstate === 'pending' ? 'off'
        : live === 'scan' ? 'scan' : live === 'done' ? 'done' : live === 'live' ? 'live' : 'off';
      st.className = 'st ' + stCls;
      st.textContent = fstate === 'missing' ? '不存在'
        : fstate === 'pending' ? '获取中'
        : live === 'scan' ? '识别中' : live === 'done' ? '已确认' : live === 'live' ? '直播中' : '未开播';
      // 状态徽标叠在封面正中；「移除直播间」自右侧滑出 —— 两者都不再占用文字宽度。
      thumb.append(st);
      const x = document.createElement('span');
      x.className = 'x';
      x.textContent = '✕';
      x.title = '移除该直播间';
      x.addEventListener('mousedown', (ev) => { ev.stopPropagation(); removeRoom(id); });
      card.append(thumb, meta, x);
      // 点击切换：不用 mousedown——拖动排序按下时也会触发 mousedown，会在拖动途中把画面切走。
      card.addEventListener('click', (ev) => {
        if (ev.target === x) return;
        // 立即切换：有会话直接切画面；未连接/未开播先显示封面占位（首帧到达自动揭除）。
        switchLive(id);
      });
      // 双击编辑备注；移动端触摸无双击：长按触发。
      card.addEventListener('dblclick', (ev) => {
        if (ev.target === x) return;
        openLiveNoteDialog(s);
      });
      if (window.__MOBILE__) {
        bindLongPress(card, (tgt) => {
          if (tgt && tgt === x) return;
          openLiveNoteDialog(s);
        });
      }
      // 拖拽排序（持久化到 config 顺序）。与账号列表同构：dragstart 只记录源，
      // dragover 只放行，drop 里才重排并保存（dragover 里重建列表会让被拖动的卡片
      // 中途被替换 → 列表抖动、拖拽事件丢失）。
      card.addEventListener('dragstart', (ev) => {
        dragSrcId = id;
        card.classList.add('dragging');
        ev.dataTransfer.effectAllowed = 'move';
      });
      card.addEventListener('dragend', () => { card.classList.remove('dragging'); dragSrcId = null; });
      card.addEventListener('dragover', (ev) => { ev.preventDefault(); ev.dataTransfer.dropEffect = 'move'; });
      card.addEventListener('drop', (ev) => {
        ev.preventDefault();
        if (!dragSrcId || dragSrcId === id) return;
        const from = liveRooms.findIndex((x) => (x.platform + ':' + x.room) === dragSrcId);
        const to = liveRooms.findIndex((x) => (x.platform + ':' + x.room) === id);
        if (from < 0 || to < 0 || from === to) return;
        const [moved] = liveRooms.splice(from, 1);
        liveRooms.splice(to, 0, moved);
        renderLiveTabs();
        // 失败不吞：提示并回滚为后端真实顺序（否则本地顺序只在本次会话内成立）。
        invoke('live_history_reorder', { order: liveRooms.map((x) => x.platform + ':' + x.room) })
          .catch((e) => { log('保存房间顺序失败: ' + e, 'error'); reloadLiveRooms(); });
      });
      box.appendChild(card);
    }

    // 3) 添加按钮：打开添加直播间弹窗。
    const add = document.createElement('button');
    add.className = 'room-add';
    add.textContent = '+ 添加直播间';
    add.addEventListener('mousedown', () => showAddLiveDialog());
    box.appendChild(add);
  }

  // 每会话记忆的视图（缩放/位置）：切换时保存/恢复，方便来回切换。
  const liveViews = new Map(); // id -> { scale, fx, fy }
  // 无封面房间的关键帧缩略图（dataURL）：进入时取首帧，退出时更新为末帧。
  const frameThumbs = new Map(); // id -> dataURL

  // 从 srcCanvas 截取缩略图（等比缩放到 96px 宽），失败返回 null。
  function captureThumb() {
    if (!srcCanvas || !srcCanvas.width || !srcCanvas.height) return null;
    try {
      const tw = 96, th = Math.round((srcCanvas.height / srcCanvas.width) * tw);
      const c = document.createElement('canvas');
      c.width = tw; c.height = th;
      c.getContext('2d').drawImage(srcCanvas, 0, 0, tw, th);
      return c.toDataURL('image/jpeg', 0.6);
    } catch (e) { return null; }
  }
  // 拖拽排序源会话 id。
  let dragSrcId = null;

  // ---- 直播画面缓冲（0–2000ms，只作用于直播间画面）----------------------
  // 语义是**抖动缓冲**（jitter buffer），不是「延迟显示器」：
  //   帧到达 → 入队并记到达时刻 → 按「到达时刻 + 缓冲」**逐帧等间隔**播出。
  // 每个收到的帧都恰好上屏一次（不抽稀、不并帧），所以播出节奏 ≈ 到达节奏，
  // 网络/解码的瞬时抖动被缓冲吸收 → 观感连续；代价是画面整体晚「缓冲」毫秒。
  //
  // 旧实现（2026-10-22 之前）只呈现「到期帧里最新的那一帧」——在 30fps 流 +
  // 300ms 缓冲下等于每秒只上屏几次，画面反而更卡；这正是「调了延迟也不流畅」的根因。
  //
  // 有界性：延迟上限 = 缓冲 + LIVE_BUF_DRIFT_MAX_MS，超过就从**队首**丢（追帧），
  // 保证延迟不会随时间单调增长；帧数/字节预算只是内存兜底。
  //
  // 作用范围：只有直播间画面走这里。桌面/屏幕预览直接呈现（保持实时），扫码取帧
  // 在后端独立任务里读最新帧——两者都不受本缓冲影响。
  // 诊断计数（先声明：下面的缓冲函数与观测钩子都会读写它们）。
  let diagPolls = 0, diagNew = 0, diagDrawn = 0, diagDropped = 0, diagAt = 0;
  let liveBufMs = 0;                        // 由 config.display_delay_ms 注入
  const LIVE_BUF_MAX_FRAMES = 240;          // 内存兜底（240 帧 ≈ 8s@30fps）
  const LIVE_BUF_DRIFT_MAX_MS = 250;        // 允许比设定缓冲再旧这么多（超出即追帧丢帧）
  const LIVE_BUF_BUDGET_BYTES = (window.__ANDROID__ ? 24 : 64) * 1024 * 1024;
  let bufQ = [];                            // [{ at, apply, bytes }]，按到达时刻升序
  let bufBytes = 0;
  function liveBufReset() {
    bufQ = [];
    bufBytes = 0;
  }
  /** 直播帧入队；缓冲为 0 时直通（零额外开销）。 */
  function liveBufPush(apply, bytes) {
    if (!liveBufMs) { diagDrawn++; apply(); return; }
    const now = performance.now();
    bufQ.push({ at: now, apply, bytes: bytes || 0 });
    bufBytes += bytes || 0;
    while (bufQ.length > 1
      && (bufQ.length > LIVE_BUF_MAX_FRAMES || bufBytes > LIVE_BUF_BUDGET_BYTES)) {
      bufBytes -= bufQ.shift().bytes;
      diagDropped++;
    }
    liveBufDrain(now);
  }
  /** 播出节拍：一次至多放行一帧（等间隔），并做延迟上界的追帧丢帧。 */
  function liveBufDrain(nowIn) {
    if (!bufQ.length) return;
    const now = nowIn === undefined ? performance.now() : nowIn;
    const maxAge = liveBufMs + LIVE_BUF_DRIFT_MAX_MS;
    while (bufQ.length > 1 && now - bufQ[0].at > maxAge) {
      bufBytes -= bufQ.shift().bytes;
      diagDropped++;
    }
    if (bufQ.length && now - bufQ[0].at >= liveBufMs) {
      const f = bufQ.shift();
      bufBytes -= f.bytes;
      diagDrawn++;
      f.apply();
    }
  }

  // 观测用：缓冲队列状态（与 window.__gsNotifyStats 同类，供脚本/真机核对）。
  window.__gsDelayStats = () => ({
    ms: liveBufMs, queued: bufQ.length, bytes: bufBytes,
    drawn: diagDrawn, dropped: diagDropped, polls: diagPolls, newFrames: diagNew,
  });

  // ---- 播放诊断（现场排障）--------------------------------------------
  // 每 10 秒把「取帧 / 新帧 / 上屏 / 丢帧」的真实速率转发到后端日志
  // （frontend_log 只进文件日志 / logcat，不占用应用内日志面板）。
  // 直播帧率上不去时，这一行就能区分「后端没出帧 / 前端没取到 / 播出丢帧」。
  function playDiagTick() {
    const now = performance.now();
    if (!diagAt) { diagAt = now; return; }
    if (now - diagAt < 10000) return;
    const secs = (now - diagAt) / 1000;
    diagAt = now;
    const wasLive = liveOn();
    const line = '[播放] 取帧 ' + (diagPolls / secs).toFixed(1) + '/s 新帧 '
      + (diagNew / secs).toFixed(1) + '/s 上屏 ' + (diagDrawn / secs).toFixed(1)
      + '/s 丢帧 ' + (diagDropped / secs).toFixed(1) + '/s 缓冲 ' + liveBufMs
      + 'ms 队列 ' + bufQ.length + ' 帧率设置 ' + fpsSetting
      + (wasLive ? '' : '（非直播态）');
    diagPolls = diagNew = diagDrawn = diagDropped = 0;
    try { invoke('frontend_log', { level: 'info', msg: line }); } catch (e) {}
  }

  // ---------- 封面占位（切换直播间/未开播时先显示封面，首帧到达后揭除） ----------
  let coverShown = false;
  function showCover(roomId) {
    const el = $('vpCover');
    if (!el) return;
    const img = $('vpCoverImg'), tip = $('vpCoverTip');
    const room = liveRooms.find((r) => (r.platform + ':' + r.room) === roomId);
    const src = room ? (room.cover || frameThumbs.get(roomId) || '') : '';
    if (src) {
      img.src = src;
      img.style.display = '';
    } else {
      img.removeAttribute('src');
      img.style.display = 'none';
    }
    if (room) {
      const name = room.note ? (room.note + '（' + (room.title || room.room) + '）') : (room.title || room.room);
      // 元数据未就绪 / 确认不存在时，占位文案同步反映真实状态（而非显示尚未拿到的标题）。
      const fstate = roomFetchState.get(roomId);
      const stat = fstate === 'pending' ? '正在获取直播间…'
        : fstate === 'missing' ? '房间不存在，请检查'
        : room.live_status === 1 ? '直播中，等待画面…' : '未开播（开播后自动连接）';
      tip.textContent = fstate ? stat : (name + ' · ' + stat);
    } else {
      tip.textContent = '';
    }
    el.hidden = false;
    coverShown = true;
  }
  function hideCover() {
    const el = $('vpCover');
    if (el) el.hidden = true;
    coverShown = false;
  }

  // 切换直播间：**立即**选中（未连接的先上封面占位），连线/取帧在后台完成。
  // 未开播房间同样可切换（显示封面；开播时自动连接并揭除封面）。
  async function switchLive(id) {
    if (activeLiveId === id) return;
    const sep = id.indexOf(':');
    const platform = id.slice(0, sep);
    const room = id.slice(sep + 1);
    // 保存当前会话视图 + 末帧缩略图（切换前）。
    if (activeLiveId) {
      liveViews.set(activeLiveId, { ...view });
      const oldRoom = liveRooms.find((r) => (r.platform + ':' + r.room) === activeLiveId);
      if (oldRoom && !oldRoom.cover) {
        const t = captureThumb();
        if (t) frameThumbs.set(activeLiveId, t);
      }
    }
    // ① 立即切换 UI（高亮 / 信息栏 / 预览目标）。
    const hasSession = liveSessions.some((s) => s.id === id);
    switchLiveNow(id);
    // ② 无会话：先上封面占位（连线成功、首帧到达后自动揭除）。
    if (hasSession) hideCover(); else showCover(id);
    // ③ 后台刷新一次元数据（标题/封面按需更新）。
    refreshRoomMeta(id);
    // ④ 未连接：后台连接（成功 → 首帧到达揭除封面；失败 → 保持封面）。
    if (!hasSession) {
      await connectLive(platform, room, '');
      if (activeLiveId === id && coverShown) showCover(id);
    }
  }

  // 元数据轮询（兜底对账）：每 30s 一轮——开播/下播已由弹幕 WS 即时推送
  // （见 bindLiveStatusEvents），轮询只负责兜底（WS 失败/漏事件）与抖音房间；
  // 逐个刷新已保存直播间（每个间隔 2s）。
  let metaPollTimer = null;
  let metaPollRunning = false;
  async function pollMetaRound() {
    if (metaPollRunning) return;
    metaPollRunning = true;
    try {
      const rooms = liveRooms.slice();
      for (const r of rooms) {
        try {
          const m = await invoke('live_refresh_meta', { platform: r.platform, room: r.room });
          // 更新本地缓存 + 仅在**状态变化**时重渲染（此前每轮每间都整表重建 DOM，
          // 封面 img 反复重建 → 无谓的解析/布局/图片解码开销）。
          const local = liveRooms.find((x) => x.platform === r.platform && x.room === r.room);
          if (local && m) {
            // 轮询只更新「开播状态」；标题/封面/主播名仅在点击或开播变化时更新
            // （避免抖音签名封面 URL 每次刷新导致图片反复重载）。
            const went_live = local.live_status !== 1 && m.live_status === 1;
            const status_changed = local.live_status !== m.live_status;
            local.live_status = m.live_status;
            if (went_live) {
              local.title = m.title || local.title;
              local.nickname = m.nickname || local.nickname;
              local.cover = m.cover || local.cover;
              local.room_id = m.room_id || local.room_id;
            }
            // 元数据到达即视为获取成功：撤掉「正在获取…」占位（此前不重渲染也撤掉）。
            const rid = r.platform + ':' + r.room;
            const hadPending = roomFetchState.delete(rid);
            if (status_changed || went_live || hadPending) {
              renderLiveTabs();
              if (rid === activeLiveId) updateVpInfo();
            }
          }
        } catch (e) {
          // 轮询这一路的失败：只有「房间不存在」才是终态（保持/落到 missing，
          // 不再反复重试刷屏）；其余瞬时失败维持原状，交给下一轮。
          const rid = r.platform + ':' + r.room;
          if (isRoomMissingError(e) && roomFetchState.get(rid) !== 'missing') {
            roomFetchState.set(rid, 'missing');
            renderLiveTabs();
          }
        }
        await new Promise((res) => setTimeout(res, 2000));
      }
    } finally {
      metaPollRunning = false;
    }
  }
  function startMetaPoll() {
    if (metaPollTimer) return;
    metaPollTimer = setInterval(pollMetaRound, 30000);
  }

  // ---------- 开播/下播即时监测（后端弹幕 WS，见 src-tauri/src/watch.rs）----------
  // 同步监测列表（幂等）：房间列表变化时调用；非 B站（抖音）后端会自行过滤。
  function syncWatchers() {
    const rooms = liveRooms
      .filter((r) => r.platform === 'bilibili')
      .map((r) => ({ platform: r.platform, room: r.room }));
    invoke('live_watch_sync', { rooms }).catch(() => {});
  }
  // 弹幕 WS 的实时状态推送：即时更新状态徽标与封面（轮询降为 30s 兜底）。
  function bindLiveStatusEvents() {
    if (!listen) return;
    listen('live-status', (ev) => {
      const p = (ev && ev.payload) || {};
      const local = liveRooms.find((x) => x.platform === p.platform && x.room === p.room);
      if (!local || local.live_status === p.live_status) return;
      const went_live = local.live_status !== 1 && p.live_status === 1;
      local.live_status = p.live_status;
      log((local.nickname || local.room) + (went_live ? ' 已开播' : ' 已下播'),
        went_live ? 'success' : 'info');
      // 直播间开播/下播：常驻通知里的直播间清单要跟着变（用户可能在后台扫码），
      // 并按设置发一条可划掉的即时通知。
      syncPersistentNotification();
      if (notifyLiveStatus) {
        const rid = p.platform + ':' + p.room;
        notifyEvent('live_status', rid, (local.nickname || local.title || local.room) +
          (went_live ? ' 已开播' : ' 已下播'));
      }
      renderLiveTabs();
      if ((p.platform + ':' + p.room) === activeLiveId) updateVpInfo();
      if (went_live) {
        // 正选中该房间（处于未开播封面态）：自动连接，首帧到达后自动揭除封面。
        const rid = p.platform + ':' + p.room;
        if (rid === activeLiveId && !liveSessions.some((s) => s.id === rid)) {
          connectLive(p.platform, p.room, '');
        }
        // 开播后拉一次权威元数据（标题/封面/主播名随开播更新）。
        invoke('live_refresh_meta', { platform: p.platform, room: p.room }).then((m) => {
          if (!m) return;
          local.title = m.title || local.title;
          local.nickname = m.nickname || local.nickname;
          local.cover = m.cover || local.cover;
          local.room_id = m.room_id || local.room_id;
          renderLiveTabs();
          if (rid === activeLiveId) {
            updateVpInfo();
            if (coverShown) showCover(rid);   // 封面/状态文案同步刷新
          }
        }).catch(() => {});
      }
    });
  }

  // 双击直播间编辑备注（自定义弹窗，非浏览器 prompt）。
  let liveNoteRoom = null;
  function openLiveNoteDialog(room) {
    liveNoteRoom = room;
    $('liveNoteInput').value = room.note || '';
    $('liveNoteOverlay').hidden = false;
    setTimeout(() => $('liveNoteInput').focus(), 50);
  }
  function saveLiveNote() {
    if (!liveNoteRoom) return;
    const note = $('liveNoteInput').value.trim();
    const room = liveNoteRoom;
    $('liveNoteOverlay').hidden = true;
    liveNoteRoom = null;
    // 不做乐观更新：后端写成功后本地才生效；失败时回滚为后端真实备注。
    invoke('live_history_set_note', {
      room: room.room, platform: room.platform, note,
    }).then(() => {
      room.note = note;
      renderLiveTabs();
      updateVpInfo();
    }).catch((e) => {
      log('保存备注失败: ' + e, 'error');
      reloadLiveRooms();
    });
  }
  $('btnLiveNoteSave').addEventListener('click', saveLiveNote);
  $('liveNoteInput').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') saveLiveNote();
  });

  // 移除已保存直播间（先删后端，成功后才停会话 + 从本地删除 + 刷新）。
  async function removeRoom(id) {
    const sep = id.indexOf(':');
    const platform = id.slice(0, sep);
    const room = id.slice(sep + 1);
    // 不做乐观更新：后端删除失败时本地不能先消失（否则下次刷新条目又「复活」）。
    try {
      await invoke('live_history_remove', { room, platform });
    } catch (e) {
      log('删除房间失败: ' + e, 'error');
      await reloadLiveRooms();   // 回滚为后端真实列表
      return;
    }
    if (liveSessions.some((s) => s.id === id)) closeLive(id);
    liveRooms = liveRooms.filter((r) => !(r.platform === platform && r.room === room));
    renderLiveTabs();
    await reloadLiveRooms();
  }

  // 关闭指定会话（前端调后端停止，再刷新列表）。
  function closeLive(id) {
    const s = liveSessions.find((x) => x.id === id);
    if (s) invoke('live_stop', { platform: s.platform, room: s.room }).catch(() => {});
    // 会话结束：其扫码路一并停止（避免无帧空转的任务残留）。
    stopScanTarget('live:' + id);
    liveSessions = liveSessions.filter((x) => x.id !== id);
    liveViews.delete(id);
    if (activeLiveId === id) {
      activeLiveId = null;
      if (liveSessions.length) switchLive(liveSessions[0].id);
    }
    if (!liveSessions.length) exitLiveMode();
    renderLiveTabs();
  }

  // 切回桌面（屏幕预览）：暂停所有会话（省资源），保留会话与视图记忆。
  function exitLiveMode() {
    // 保存当前会话视图，供切回时恢复；并更新末帧缩略图。
    if (activeLiveId) {
      liveViews.set(activeLiveId, { ...view });
      const room = liveRooms.find((r) => (r.platform + ':' + r.room) === activeLiveId);
      if (room && !room.cover) {
        const t = captureThumb();
        if (t) frameThumbs.set(activeLiveId, t);
      }
    }
    invoke('live_set_active', { id: '' }).catch(() => {});
    invoke('set_preview_active', { active: true }).catch(() => {});
    activeLiveId = null;
    liveBufReset();   // 回到桌面：丢弃直播间残留的延迟帧
    preview = null;
    view = { scale: 1, fx: 0.5, fy: 0.5 };
    viewT = { scale: 1, fx: 0.5, fy: 0.5 };
    srcCanvas = null;
    liveAnchor = null;
    liveAnchorPrev = null;
    baseMap = null; baseMapReady = false; baseMapActive = false;
    if (baseMapTimer) { clearTimeout(baseMapTimer); baseMapTimer = null; }
    closeTileMode(false);
    hideCover();
    invoke('set_last_live', { id: '' }).catch(() => {});   // 回到桌面：清空「最后观看」
    renderLiveTabs();
    updateVpInfo();
    renderScan();   // 桌面视图：按钮反映"桌面扫码"路的状态
  }

  // 添加直播间弹窗：平台 + 房间号 + 备注。
  let addLivePlatform = 'bilibili';
  let addLiveNote = '';
  function showAddLiveDialog() {
    $('addLiveRoom').value = '';
    $('addLiveNote').value = '';
    addLiveNote = '';
    $('addLiveOverlay').hidden = false;
    setTimeout(() => $('addLiveRoom').focus(), 50);
  }
  // 平台选择段。
  $('addLivePlat').addEventListener('click', (e) => {
    const btn = e.target.closest('button');
    if (!btn) return;
    addLivePlatform = btn.dataset.p;
    for (const b of $('addLivePlat').querySelectorAll('button')) b.classList.toggle('on', b === btn);
  });
  $('btnAddLiveSave').addEventListener('click', async () => {
    const room = $('addLiveRoom').value.trim();
    if (!room) { log('请输入直播间号', 'error'); return; }
    const note = $('addLiveNote').value.trim();
    const platform = addLivePlatform;
    const nid = platform + ':' + room;
    $('addLiveOverlay').hidden = true;
    // ① 立即入列 + 置「正在获取直播间…」占位：不必等网络往返，用户马上看到反馈。
    liveRooms.push({
      room, platform, note: note || '', last_used: Math.floor(Date.now() / 1000),
      title: '', nickname: '', cover: '', room_id: '', live_status: 0,
      live_start: 0, last_checked: 0,
    });
    roomFetchState.set(nid, 'pending');
    renderLiveTabs();
    await connectLive(platform, room, note);
    // 新增成功（已建立会话）：切到新房间（含封面占位语义）；未开播则不切、仅进列表。
    if (liveSessions.some((s) => s.id === nid)) switchLive(nid);
  });
  $('addLiveRoom').addEventListener('keydown', (e) => { if (e.key === 'Enter') $('btnAddLiveSave').click(); });

  // 连接直播间（platform/room/note 明确传入）。未开播不抛异常。
  async function connectLive(platform, room, note) {
    const id = platform + ':' + room;
    // 先持久化该房间（无论开播与否），保证左栏始终可见。
    // 失败：后端并没有这个房间——不建立会话（否则会连出一个列表里不存在的直播间），
    // 并把界面回滚为后端真实列表。
    try {
      await invoke('live_history_push', { room, platform, note: note || '' });
    } catch (e) {
      log('保存直播间失败: ' + e, 'error');
      await reloadLiveRooms();
      return;
    }
    // 立即拉取一次元数据（标题/封面/主播名），不等轮询。
    try {
      const m = await invoke('live_refresh_meta', { platform, room });
      const local = liveRooms.find((x) => (x.platform + ':' + x.room) === id);
      if (local && m) {
        local.title = m.title || local.title;
        local.nickname = m.nickname || local.nickname;
        local.cover = m.cover || local.cover;
        local.room_id = m.room_id || local.room_id;
        local.live_status = m.live_status;
      }
      // 取到元数据：撤掉占位，落到真实数据。
      roomFetchState.delete(id);
      renderLiveTabs();
    } catch (e) {
      // 拉取失败：仅当服务端**明确**说房间不存在才落到「房间不存在，请检查」；
      // 网络/风控等瞬时失败保留 pending 语义由后续轮询补上，避免误报。
      const local = liveRooms.find((x) => (x.platform + ':' + x.room) === id);
      if (isRoomMissingError(e)) {
        roomFetchState.set(id, 'missing');
        log('直播间 ' + room + '：房间不存在，请检查房间号是否正确', 'error');
      } else if (!(local && (local.title || local.nickname))) {
        roomFetchState.set(id, 'pending');
      }
      renderLiveTabs();
    }
    if (liveSessions.some((s) => s.id === id)) {
      await reloadLiveRooms();
      return;
    }
    try {
      const info = await invoke('live_start', { platform, room });
      if (note) info.note = note;
      info.status = 'live';   // 已连接 → 直播中
      liveSessions.push(info);
      baseMap = null; baseMapReady = false; baseMapActive = false;
      if (baseMapTimer) { clearTimeout(baseMapTimer); baseMapTimer = null; }
      closeTileMode(false);
      invoke('set_preview_active', { active: false }).catch(() => {});
      log('已连接' + livePlatformLabel(platform) + '直播间 ' + room + '（' + info.w + '×' + info.h + '），预览与扫码已切换', 'success');
    } catch (e) {
      // 房间不存在时 live_start 也会失败：此时不重复报错（上面已给明确结论），
      // 避免同一问题刷两条矛盾日志；其余失败照常提示原因。
      if (!roomFetchState.has(id) || roomFetchState.get(id) !== 'missing') {
        log('直播间 ' + room + '：' + e, 'info');
      }
    }
    await reloadLiveRooms();
    // 不在此强制切换视图：点击流程已在 switchLive 中**立即**切换（此后用户可能已
    // 看向别处，连接完成再把画面拽回来会打断操作）；「新增直播间」的成功切换由
    // 保存按钮的调用方负责。
  }

  // 仅切换主画面（不触发连接）——供 connectLive 成功后调用，避免递归。
  function switchLiveNow(id) {
    if (activeLiveId === id) return;
    liveBufReset();   // 换源：上一路排队的帧不能漏到新画面上
    if (activeLiveId) liveViews.set(activeLiveId, { ...view });
    activeLiveId = id;
    invoke('live_set_active', { id }).catch(() => {});
    const s = activeSession();
    if (s && s.w > 0 && s.h > 0) {
      preview = { mon: { x: 0, y: 0, w: s.w, h: s.h } };
      const saved = liveViews.get(id);
      view = saved ? { ...saved } : { scale: 1, fx: 0.5, fy: 0.5 };
      viewT = { ...view };
      srcCanvas = null;
      liveAnchor = null;   // 换会话：旧锚定帧作废
      liveAnchorPrev = null;
    }
    renderLiveTabs();
    updateVpInfo();
    renderScan();   // 切换目标后按钮状态随目标重算
    invoke('set_last_live', { id }).catch(() => {});   // 记住最后观看（下次启动恢复）
    refreshPreview().catch(() => {});
  }

  // 直播帧拉取：协议与屏幕预览一致（24 字节头 + RGBA），mon = 流原始尺寸，
  // 因此视口/拖拽/缩放交互完全复用；无需底图（每帧即当前视口裁剪）。
  // 响应长度语义：0 字节 = 会话已关闭（下播/断流）→ 自动切回屏幕；
  // 24 字节 = 缓冲欠载或帧未变 → 沿用上一帧（流帧率低于 30fps 属正常现象）。
  async function refreshLiveFrame() {
    const s = activeSession();
    // 无会话（选中的未开播房间 / 连线中）：保持封面占位，不在此退出
    // （退出由 closeLive、桌面卡片等显式路径处理）。
    if (!s) return false;
    const region = viewportMonRect() || { x: 0, y: 0, w: 0, h: 0 };
    // 预览转换上限（避免按显示分辨率逐像素转 YUV→RGBA 的高开销；
    // 画布放大由 GPU drawImage 完成，质量足够）。
    const buf = await invoke('capture_live_frame', {
      id: s.id,
      x: region.x, y: region.y, w: region.w, h: region.h,
      maxWidth: Math.min(canvas.width, 480),
    });
    if (!(buf instanceof ArrayBuffer)) throw new Error('直播数据异常');
    if (buf.byteLength === 0) {
      // 该会话已结束（下播/断流）：从列表移除，回退到剩余会话或屏幕。
      closeLive(s.id);
      log('直播间 ' + s.room + ' 已结束', 'info');
      return false;
    }
    if (buf.byteLength <= 24) return false;   // 欠载/帧未变：沿用上一帧
    const dv = new DataView(buf);
    const pw = dv.getUint32(0, true), ph = dv.getUint32(4, true);
    const mon = { x: 0, y: 0, w: dv.getInt32(16, true), h: dv.getInt32(20, true) };
    if (!pw || !ph || 24 + pw * ph * 4 > buf.byteLength) throw new Error('直播数据异常');
    // 呈现段整体进入直播缓冲：下面读写的都是「画布侧」状态
    // （preview / view / srcCanvas / 锚定帧 / 封面 / 缩略图），延后执行不会让
    // 取帧逻辑看到陈旧值。返回值仍表示「本轮到新帧」，AIMD 节拍不受影响。
    liveBufPush(() => {
      // 排队期间可能已切走直播间：旧会话的帧不能画到新会话的画布上。
      const cur = activeSession();
      if (!cur || cur.id !== s.id) return;
      if (!preview || preview.mon.w !== mon.w || preview.mon.h !== mon.h) {
        preview = { mon };
        view = { scale: 1, fx: 0.5, fy: 0.5 };
        viewT = { scale: 1, fx: 0.5, fy: 0.5 };
        liveAnchor = null;   // 流规格变化（换流）：旧锚定帧作废
        liveAnchorPrev = null;
      }
      if (!srcCanvas || srcCanvas.width !== pw || srcCanvas.height !== ph) {
        srcCanvas = document.createElement('canvas');
        srcCanvas.width = pw;
        srcCanvas.height = ph;
      }
      const img = new ImageData(new Uint8ClampedArray(buf, 24, pw * ph * 4), pw, ph);
      srcCanvas.getContext('2d').putImageData(img, 0, 0);
      // 记录本帧覆盖的 mon 矩形（= 请求时的视口）：拖拽/缩放期按此锚定绘制；
      // 上一帧保留作垫底（运动补白）。
      if (liveAnchor) liveAnchorPrev = liveAnchor;
      liveAnchor = { rect: region };
      clearPendingRealtime();
      viewDirty = false;
      drawView();
      if (coverShown) hideCover();   // 首帧到达：揭除封面占位
      previewErrShown = false;
      // 无封面房间：首次进入记录首帧缩略图。
      const room = liveRooms.find((r) => (r.platform + ':' + r.room) === s.id);
      if (room && !room.cover && !frameThumbs.has(s.id)) {
        const t = captureThumb();
        if (t) frameThumbs.set(s.id, t);
      }
    }, buf.byteLength);
    return true;
  }

  let previewBusy = false;
  let liveBusy = false;
  async function refreshPreview() {
    if (liveOn()) {
      if (liveBusy) return false;
      liveBusy = true;
      try { return await refreshLiveFrame(); }
      finally { liveBusy = false; }
    }
    if (previewBusy) return false;
    previewBusy = true;
    try {
      const region = viewportMonRect() || { x: 0, y: 0, w: 0, h: 0 };
      const buf = await invoke('capture_preview', {
        x: region.x, y: region.y, w: region.w, h: region.h,
        maxWidth: canvas.width,
      });
      if (!(buf instanceof ArrayBuffer)) throw new Error('预览数据异常');
      if (buf.byteLength === 24) {
        clearPendingRealtime();
        viewDirty = false;
        return false;
      }
      const dv = new DataView(buf);
      const pw = dv.getUint32(0, true), ph = dv.getUint32(4, true);
      const mon = {
        x: dv.getInt32(8, true), y: dv.getInt32(12, true),
        w: dv.getInt32(16, true), h: dv.getInt32(20, true),
      };
      if (!pw || !ph || 24 + pw * ph * 4 > buf.byteLength) throw new Error('预览数据异常');
      // 桌面/屏幕预览**不缓冲**：缓冲只作用于直播间画面，此处保持实时呈现。
      (() => {
        // 切换期间可能已进直播间：屏幕帧不能盖到直播画布上。
        if (liveOn()) return;
        const monChanged = !!preview && (preview.mon.x !== mon.x || preview.mon.y !== mon.y
          || preview.mon.w !== mon.w || preview.mon.h !== mon.h);
        if (monChanged) {
          view = { scale: 1, fx: 0.5, fy: 0.5 };
          viewT = { scale: 1, fx: 0.5, fy: 0.5 };
        }
        if (!preview || monChanged) {
          preview = { mon };
        }
        if (!srcCanvas || srcCanvas.width !== pw || srcCanvas.height !== ph) {
          srcCanvas = document.createElement('canvas');
          srcCanvas.width = pw;
          srcCanvas.height = ph;
        }
        const img = new ImageData(new Uint8ClampedArray(buf, 24, pw * ph * 4), pw, ph);
        srcCanvas.getContext('2d').putImageData(img, 0, 0);
        clearPendingRealtime();
        viewDirty = false;
        drawView();
        updateRegion();
        previewErrShown = false;
      })();
      return true;
    } catch (e) {
      if (!previewErrShown) {
        previewErrShown = true;
        log('预览失败: ' + e, 'error');
      }
      return false;
    } finally {
      previewBusy = false;
    }
  }

  // 显示区域帧率（设置项 30/60/120，默认 60）：预览轮询间隔上限。
  let fpsSetting = 60;
  function previewIntervalMs() {
    return 1000 / (fpsSetting || 60);
  }
  // 直播预览节拍：**增减式自适应**跟随流帧率。
  //
  // 历史坑：早先固定 40ms（25fps）/闲置 50ms（20fps）；上一版改为「按每秒新帧数
  // 估算流帧率」——但该估算自指：一旦轮询间隔长于流帧间隔（如 50ms > 33ms），
  // 每轮都能拿到新帧，估出的「帧率」恒等于轮询率 → 间隔永远收不回来，卡死在
  // 20fps（实测流 29.2fps、显示 17fps）。
  //
  // 现改为 AIMD（加减式）控制；**下限由 16ms 改为 0**：
  // - 本轮拿到新帧 → 立即再取（间隔 0），由 IPC 往返自然限速；
  // - 连续三轮无新帧 → 指数退避（4→8→16→24ms 上限），避免空转烧 CPU。
  //
  // 为什么下限必须是 0（2026-10-13 实测，739 个埋点采样）：
  // 旧下限 16ms 使单帧 = 间隔(16~50) + 取帧 IPC(19.9ms) = 周期 67ms → 只有 15fps，
  // 而流本身约 25~33fps（解码心跳），等于把消费速度压到流之下 —— 后端帧队列常年
  // 满 4 帧，crop_frame 每轮只弹一帧，积压帧被丢弃（日志「队列 4」）。
  // **间隔并不省 CPU**：await IPC 期间主线程是空闲的，它只是让画面变卡。
  // 退避上限 24ms 保证低帧率流（~40ms/帧）仍能跟上。
  let liveEstInterval = 0;    // 有帧即 0（尽快取下一帧）
  let liveMiss = 0;           // 连续「无新帧」计数
  let liveWasOn = false;
  function liveAdaptiveInterval(changed) {
    if (!liveWasOn) { liveWasOn = true; liveEstInterval = 0; liveMiss = 0; }
    if (changed) {
      liveMiss = 0;
      liveEstInterval = 0;
    } else if (++liveMiss >= 3) {
      liveMiss = 0;
      liveEstInterval = Math.min(24, liveEstInterval ? liveEstInterval * 2 : 4);
    }
    return Math.max(liveEstInterval, previewIntervalMs());
  }
  const PREVIEW_IDLE_INTERVAL = 140;
  let idleFrames = 0;
  let previewActive = false;
  let fpsFrames = 0, fpsLast = performance.now();
  // 帧率徽标：直播模式统计「真实渲染的新帧数」（有意义的显示帧率；轮询快于流时
  // 不应把重复帧算进去）；屏幕预览沿用轮询节拍（静止画面下也反映预览在跑）。
  function tickFps(changed) {
    if (changed || !liveOn()) fpsFrames++;
    const now = performance.now();
    if (now - fpsLast >= 1000) {
      const el = $('fpsMeter');
      if (el) el.textContent = fpsFrames;
      fpsFrames = 0;
      fpsLast = now;
    }
  }
  // 代际令牌：stop/start 快速交替（如缩放到极限时每次滚轮都触发）会残留
  // 仍在 await 的旧循环实例，恢复后与新循环并行 → 帧率远超上限。代际不匹配
  // 的实例在任何续行点直接退出。
  let previewGen = 0;
  async function previewLoop(gen) {
    if (!previewActive || gen !== previewGen) return;
    // 先按播出节拍放行一条已到期的直播缓冲帧：本轮到期的帧可能在上一轮就入队了，
    // 而下一轮要等 refreshPreview 返回后才调度，不在这里补一次会多积一周期。
    // （本循环同时充当播出时钟：它的节拍由设置里的显示帧率决定。）
    liveBufDrain();
    const t0 = performance.now();
    diagPolls++;
    const changed = await refreshPreview();
    if (changed) diagNew++;
    playDiagTick();
    tickFps(changed);
    if (!previewActive || gen !== previewGen) return;
    if (changed) idleFrames = 0; else idleFrames++;
    // 直播：AIMD 自适应节拍（见 liveAdaptiveInterval）；连续无新帧时指数退避省 CPU。
    const interval = liveOn()
      ? liveAdaptiveInterval(changed)
      : idleFrames > 2
        ? PREVIEW_IDLE_INTERVAL
        : previewIntervalMs();
    if (!liveOn()) liveWasOn = false;   // 退出直播：下次进入时重置统计窗口
    const elapsed = performance.now() - t0;
    previewTimer = setTimeout(() => previewLoop(gen), Math.max(0, interval - elapsed));
  }
  /// 设置里切换显示帧率后调用：重启预览循环让新节拍立即生效。
  function restartPreviewTiming() {
    if (!previewActive) return;
    stopPreview();
    startPreview();
  }
  function startPreview() {
    if (previewActive || paused || dragging || baseMapActive) return;
    previewActive = true;
    const gen = ++previewGen;
    previewLoop(gen);
  }
  function stopPreview() {
    previewActive = false;
    previewGen++;
    if (previewTimer) { clearTimeout(previewTimer); previewTimer = null; }
  }
  // 屏幕扫码「自动切后台」标志：回到前台时据此自动停止屏幕扫码（用完即止）。
  let screenScanAutoBg = false;
  // 屏幕扫码前正在观看的直播间（回到应用后恢复它）。
  //
  // 屏幕扫码必须把中间区从「直播画面」切到「桌面预览」才能扫到游戏内的二维码，
  // 而 exitLiveMode() 会清空 activeLiveId / 视图 / last_live。此前回到应用后
  // 中间就停在退出前的桌面预览上，用户得手动点一次直播间才能回去——这与
  // 「回到应用应看到之前的直播间」相悖，故在此记住并在回前台时恢复。
  let screenScanPrevLive = null;
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
      // 隐藏时若正处于拖拽中，window 的 mouseup 可能不再触发 → dragging 永久为 true
      // → 之后回前台 startPreview 被守卫挡住，预览永久死掉。此处显式复位拖拽态。
      panStart = null;
      if (dragging) { dragging = false; canvas.classList.remove('dragging'); }
      stopPreview();
      // 切到后台：显示常驻通知（后台是否在扫码 / 正在扫哪些直播间）。
      syncPersistentNotification();
    } else {
      startPreview();
      // 回前台：立刻撤下常驻通知（用户已能看到界面，通知栏没有必要再占位）。
      syncPersistentNotification();
      // 屏幕扫码切后台后用户切回：按设置决定是否自动停止屏幕扫码并释放共享。
      //
      // 只停扫码循环不够：MediaProjection 仍在取帧，通知栏持续显示「正在共享屏幕」，
      // 却没有消费者——既费电也违背用户的停止预期（真机实测：日志已写「自动停止
      // 屏幕扫码」，设置面板的「屏幕共享」仍显示「共享中」）。
      //
      // 该行为现由设置项 `screen_scan_stop_on_resume` 控制（默认开）：关掉后保持
      // 捕获与扫码，适合反复切出/切回连续扫多个码的场景。
      if (screenScanAutoBg && screenScanStopOnResume) {
        screenScanAutoBg = false;
        const wasScanning = scanningTargets.has("screen") || stoppingTargets.has("screen");
        if (wasScanning) stopScanTarget("screen");
        if (wasScanning) log("已回到应用，自动停止屏幕扫码", "info");
        stopScreenShare({ quiet: true });
      }
      // 恢复屏幕扫码前正在看的直播间（房间可能已被删除，故需再确认一次）。
      if (screenScanPrevLive) {
        const prev = screenScanPrevLive;
        screenScanPrevLive = null;
        const known = liveRooms.some((r) => (r.platform + ':' + r.room) === prev);
        if (known && !liveOn()) {
          switchLive(prev).catch(() => {});
          log("已回到应用，恢复上次观看的直播间", "info");
        }
      }
    }
  });

  // 内容绘制矩形：默认按流比例 letterbox 居中（留黑边，不裁剪）；
  // coverFill=true（移动端双击切换）时改为填满画布并裁切多余部分。
  let coverFill = false;
  function contentRect() {
    const cw0 = canvas.width, ch0 = canvas.height;
    if (!preview) return { cw: cw0, ch: ch0, ox: 0, oy: 0 };
    const ma = preview.mon.w / preview.mon.h;
    const ca = cw0 / ch0;
    // 放大后（scale>1）按 cover 铺满显示区，消除 letterbox 黑边；未放大时 contain。
    const fill = coverFill || viewT.scale > 1.02;
    let cw, ch;
    if (ma >= ca) {
      // 内容更宽：contain 横向填满+上下黑边；cover 纵向填满+横向裁切。
      if (fill) { ch = ch0; cw = Math.round(ch0 * ma); }
      else { cw = cw0; ch = Math.round(cw0 / ma); }
    } else {
      if (fill) { cw = cw0; ch = Math.round(cw0 / ma); }
      else { ch = ch0; cw = Math.round(ch0 * ma); }
    }
    return { cw, ch, ox: Math.round((cw0 - cw) / 2), oy: Math.round((ch0 - ch) / 2) };
  }

  // ---------- 视图平滑 ----------
  const VIEW_SMOOTH = 0.5;
  let pendingRealtime = false;
  function clearPendingRealtime() { pendingRealtime = false; }
  function stepView() {
    const scaleChanged = view.scale !== viewT.scale;
    view.scale = viewT.scale;
    const dx = viewT.fx - view.fx;
    const dy = viewT.fy - view.fy;
    if (Math.abs(dx) < 0.0003 && Math.abs(dy) < 0.0003) {
      view.fx = viewT.fx; view.fy = viewT.fy;
      return scaleChanged;
    }
    view.fx += dx * VIEW_SMOOTH;
    view.fy += dy * VIEW_SMOOTH;
    return true;
  }
  function clampViewT() {
    if (!preview) return;
    const z = Math.max(viewT.scale, 1);
    const fw = 1 / z, fh = 1 / z;
    viewT.fx = Math.min(Math.max(viewT.fx, fw / 2), 1 - fw / 2);
    viewT.fy = Math.min(Math.max(viewT.fy, fh / 2), 1 - fh / 2);
  }
  let viewRaf = null;
  // 视图运动窗口（拖拽 mousemove / 滚轮缩放后短暂延长）：窗口内 RAF 重绘循环续跑，
  // 让「本地变换」逐帧生效（直播模式也走此路径，见 startLiveMotionRedraw）。
  let liveMotionUntil = 0;
  // 直播拖拽/缩放的「mon 锚定帧」：最近一帧实际覆盖的 mon 矩形（= 请求时的视口）。
  // 绘制时按此矩形把帧映射回 mon 空间：已加载区域随视图**平滑滑动**（本地变换，
  // 零延迟跟手），新帧到达只更新它覆盖的区域——不再整屏替换，消除快拖时的
  // 离散跳变/落后。拖出未加载区域短暂露暗底，由预览循环按流节拍（~19-25Hz）
  // 在 1~2 拍内补齐（= 地图类应用的瓦片手感）。
  let liveAnchor = null;
  // 上一锚定帧（垫底层：运动补白，见 drawLiveAnchored）。
  let liveAnchorPrev = null;
  function startLiveMotionRedraw() {
    if (viewRaf) return;
    const loop = () => {
      drawView();
      if (dragging || performance.now() < liveMotionUntil) {
        viewRaf = requestAnimationFrame(loop);
      } else {
        viewRaf = null;
      }
    };
    viewRaf = requestAnimationFrame(loop);
  }
  function ensureViewAnim() {
    // 直播模式（**必须最先处理**）：每次视图变化都要同步 view=viewT（跟手）。
    // 教训（2026-10-14 实机）：此前把该同步放在 `if (viewRaf) return` 之后 →
    // 拖拽开始后 RAF 已在跑，后续 mousemove 全被早退挡掉，view 永远停在拖动
    // 第一步的状态（画面"不动"、取帧也用旧视口 → 缩放露黑区）。
    // ① 视图即时同步（跟手）；
    // ② RAF 按最近一帧的 mon 锚定本地重绘（内容平滑滑动，零延迟）；
    // ③ 取帧交由预览主循环按流节拍（~19-25Hz）持续补齐当前视口——
    //    新帧只更新其覆盖区域，无整屏跳变。
    if (liveOn()) {
      view = { ...viewT };
      viewDirty = true;
      liveMotionUntil = performance.now() + 240;
      startLiveMotionRedraw();
      return;
    }
    if (viewRaf) return;
    // 屏幕模式：停止实时预览，启用瓦片底图 + RAF 平滑重绘（底图可客户端平移）。
    stopPreview();
    if (!paused && !baseMapActive) { startBaseMap(); }
    const loop = () => {
      const moving = stepView();
      drawView();
      if (moving || dragging) {
        viewRaf = requestAnimationFrame(loop);
      } else {
        viewRaf = null;
        stopBaseMap();
        viewDirty = false;
        // 恢复实时预览；显示源仍用底图，直到新实时帧到达（refreshPreview 清 pendingRealtime）。
        // 不设超时强制切回——那会在实时帧未到时切到旧 srcCanvas，造成闪烁。
        if (!paused) { pendingRealtime = true; startPreview(); }
        updateRegion();
        prevRegionStr = null;
      }
    };
    viewRaf = requestAnimationFrame(loop);
  }

  // ---------- 瓦片底图 ----------
  let baseMap = null;
  let baseMapReady = false;
  let pauseShot = null;
  let dragging = false;
  let paused = false;

  async function capturePauseShot(maxWidth) {
    for (let attempt = 0; attempt < 12; attempt++) {
      try {
        const buf = await invoke('capture_preview', { x: 0, y: 0, w: 0, h: 0, maxWidth });
        if (buf instanceof ArrayBuffer && buf.byteLength > 24) {
          const dv = new DataView(buf);
          const pw = dv.getUint32(0, true), ph = dv.getUint32(4, true);
          const mon = {
            x: dv.getInt32(8, true), y: dv.getInt32(12, true),
            w: dv.getInt32(16, true), h: dv.getInt32(20, true),
          };
          if (pw && ph && 24 + pw * ph * 4 <= buf.byteLength) {
            const c = document.createElement('canvas');
            c.width = pw; c.height = ph;
            c.getContext('2d').putImageData(
              new ImageData(new Uint8ClampedArray(buf, 24, pw * ph * 4), pw, ph), 0, 0);
            pauseShot = { canvas: c };
            if (!preview) preview = { mon };
            if (paused) drawView();
            return true;
          }
        }
      } catch (e) { /* 截图失败，重试 */ }
      if (!paused) return false;
      await new Promise((r) => setTimeout(r, 30));
    }
    return false;
  }

  const BASE_MAP_TARGET = 1280;
  const TILE_PX = 64;
  let baseMapTimer = null;
  let baseMapActive = false;
  let tileMode = false;
  // 开启瓦片模式时所用的显示器矩形（关闭时按同一矩形回传，见 closeTileMode）。
  let tileMon = null;

  function patchTiles(buf) {
    if (!(buf instanceof ArrayBuffer) || buf.byteLength < 8) return false;
    const dv = new DataView(buf);
    const tw = dv.getUint32(0, true), th = dv.getUint32(4, true);
    if (!tw || !th) return false;
    if (!baseMap || baseMap.canvas.width !== tw || baseMap.canvas.height !== th) {
      const c = document.createElement('canvas');
      c.width = tw; c.height = th;
      baseMap = { canvas: c };
      baseMapReady = false;
    }
    const ctx = baseMap.canvas.getContext('2d');
    let off = 8, dirty = false;
    while (off + 16 <= buf.byteLength) {
      const col = dv.getUint32(off, true), row = dv.getUint32(off + 4, true);
      const w = dv.getUint32(off + 8, true), h = dv.getUint32(off + 12, true);
      off += 16;
      const n = w * h * 4;
      if (off + n > buf.byteLength) break;
      const img = new ImageData(new Uint8ClampedArray(buf, off, n), w, h);
      ctx.putImageData(img, col * TILE_PX, row * TILE_PX);
      off += n;
      dirty = true;
    }
    return dirty;
  }

  const BASE_MAP_INTERVAL = 33;
  let baseMapFails = 0;
  // 关闭瓦片模式（幂等）。必须用「开启时同一显示器矩形」回传：早先用 (0,0,0,0) 会命中
  // (0,0) 所在显示器，在副屏上会漏关。force=true 用于补偿竞态（见 tileLoop）。
  function closeTileMode(force) {
    if (!force && !tileMode) return;
    tileMode = false;
    const m = tileMon || (preview ? preview.mon : null);
    if (!m) return;
    invoke('set_tile_mode', { x: m.x, y: m.y, w: m.w, h: m.h, on: false }).catch(() => {});
  }
  async function tileLoop() {
    if (!baseMapActive) return;
    const t0 = performance.now();
    try {
      const buf = await invoke('capture_tiles', {
        x: 0, y: 0, w: 0, h: 0, maxWidth: BASE_MAP_TARGET,
      });
      baseMapFails = 0;
      if (patchTiles(buf)) { baseMapReady = true; }
    } catch (e) {
      // 桌面复制不可用（HDR / 安全桌面等）：连续失败后放弃底图模式，
      // 回退实时帧拉伸绘制（解除冻结）。
      if (++baseMapFails >= 6) { stopBaseMap(); return; }
    }
    // 会话在本次请求期间结束（拖拽/缩放收尾）：后端 tile_frame 见到 tile_mode 为关会
    // 自行把它重新开启（兜底分支），仅靠 stopBaseMap 的一次「关闭」覆盖不到 —— 必须在
    // 本次请求副作用之后补发一次。否则瓦片模式残留 → 帧总线只拷「脏瓦片包围盒」→
    // 预览请求区域永远覆盖不到 → 前端收到「未变化」→ 画面冻结，直到下次拖拽/缩放才恢复。
    if (!baseMapActive) { closeTileMode(true); return; }
    const elapsed = performance.now() - t0;
    baseMapTimer = setTimeout(tileLoop, Math.max(0, BASE_MAP_INTERVAL - elapsed));
  }
  function startBaseMap() {
    if (baseMapActive) return;
    // preview 未就绪（如刚从直播切回、屏幕首帧未到）：不启用底图，
    // 否则 set_tile_mode 用错区域 → 瓦片永不脏 → 拖拽出现黑块。
    if (!preview) return;
    baseMapActive = true;
    // 旧底图来自上一轮拖拽/缩放会话，内容可能已过期：先作废，
    // 等后端 tile_mode 全量脏瓦片帧（首个 tileLoop 周期）刷新后再启用。
    baseMapReady = false;
    baseMap = null;
    tileMon = preview.mon;
    invoke('set_tile_mode', {
      x: tileMon.x, y: tileMon.y, w: tileMon.w, h: tileMon.h, on: true,
    }).catch(() => {});
    tileMode = true;
    tileLoop();
  }
  function stopBaseMap() {
    baseMapActive = false;
    if (baseMapTimer) { clearTimeout(baseMapTimer); baseMapTimer = null; }
    closeTileMode(false);
  }

  function drawView() {
    if (!preview) return;
    // 移动端：一旦有画面实际绘制即置位 __hadFrame（供封面/标题逻辑判断是否已出画面）。
    if (window.__MOBILE__ && !window.__hadFrame) {
      window.__hadFrame = true;
    }
    // 拖拽/缩放刚启动、新底图刷新中：冻结当前画面，不重绘。
    // 旧底图已作废（可能还停留在上一轮会话的内容），此时若强行取帧会闪现
    // 上一轮拖拽/缩放的最后一帧；冻结 1~2 帧后全量瓦片帧到达即恢复。
    // 例外：画布刚被重建/清空（canvasBlank）时冻结已无内容可保留，必须绘制
    // 兜底（退到 srcCanvas 实时帧）——否则画布全黑。
    if (viewDirty && baseMapActive && !baseMapReady && !canvasBlank) return;
    const ctx = canvasCtx;
    ctx.fillStyle = '#101010';
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    const v = viewportFrac();
    const r = contentRect();
    const useBase = !liveOn() && (viewDirty || pendingRealtime) && baseMapReady;
    const shot = paused ? pauseShot : (useBase ? baseMap : null);
    if (shot) {
      const sc = shot.canvas;
      const sx = v.x0 * sc.width, sy = v.y0 * sc.height;
      const sw = Math.max(1, v.fw * sc.width), sh = Math.max(1, v.fh * sc.height);
      ctx.imageSmoothingEnabled = true;
      ctx.imageSmoothingQuality = 'high';
      ctx.drawImage(sc, sx, sy, sw, sh, r.ox, r.oy, r.cw, r.ch);
    } else if (liveOn()) {
      // 直播：按「帧覆盖的 mon 矩形」锚定绘制（见 liveAnchor 注释）；
      // 稳态（视图未动、帧即当前视口）下与整屏绘制等价。
      drawLiveAnchored();
    } else if (srcCanvas) {
      ctx.imageSmoothingEnabled = r.cw <= srcCanvas.width + 0.5;
      // 拖拽动画中降质量：GPU 缩放成本更低，帧率更稳；屏幕预览静止时仍用 high 保清晰度。
      ctx.imageSmoothingQuality = dragging ? 'low' : 'high';
      ctx.drawImage(srcCanvas, r.ox, r.oy, r.cw, r.ch);
    }
    canvasBlank = false;
  }

  // 直播帧的 mon 锚定绘制：把帧内容映射回它实际覆盖的 mon 区域，绘制「当前视图
  // ∩ 帧覆盖区」，其余留暗底（等下一拍取帧补齐）。**两层**：当前锚定帧在上，
  // 上一锚定帧垫底——拖动/缩放时新露出的边缘先由上一帧（略旧但连续）补白，
  // 避免黑区闪烁；新帧到达后自然覆盖。
  function drawLiveAnchored() {
    const ctx = canvasCtx;
    const r = contentRect();
    const v = viewportFrac();
    const m = preview.mon;
    const sc = srcCanvas;
    if (!sc) return;
    if (!liveAnchor || liveAnchor.rect.w <= 0 || m.w <= 0 || m.h <= 0) {
      // 无锚信息（异常兜底）：退化为整屏绘制。
      ctx.imageSmoothingEnabled = r.cw <= sc.width + 0.5;
      ctx.imageSmoothingQuality = 'low';
      ctx.drawImage(sc, r.ox, r.oy, r.cw, r.ch);
      return;
    }
    // 垫底：上一锚定帧（与当前帧区域不同时才画）。
    const pa = liveAnchorPrev;
    if (pa && pa.rect.w > 0 && (pa.rect.x !== liveAnchor.rect.x || pa.rect.y !== liveAnchor.rect.y
        || pa.rect.w !== liveAnchor.rect.w || pa.rect.h !== liveAnchor.rect.h)) {
      drawLiveAnchorLayer(pa.rect, r, v, m);
    }
    drawLiveAnchorLayer(liveAnchor.rect, r, v, m);
  }

  // 按 mon 锚定绘制单层帧：帧内容(srcCanvas) ↔ 覆盖矩形(fr) ↔ 当前视图(v)。
  function drawLiveAnchorLayer(fr, r, v, m) {
    const ctx = canvasCtx;
    const sc = srcCanvas;
    const vx0 = m.x + v.x0 * m.w, vy0 = m.y + v.y0 * m.h;
    const vw = v.fw * m.w, vh = v.fh * m.h;
    const ix0 = Math.max(vx0, fr.x), iy0 = Math.max(vy0, fr.y);
    const ix1 = Math.min(vx0 + vw, fr.x + fr.w), iy1 = Math.min(vy0 + vh, fr.y + fr.h);
    if (ix1 - ix0 < 0.5 || iy1 - iy0 < 0.5) return;   // 无交集
    const sx = ((ix0 - fr.x) / fr.w) * sc.width;
    const sy = ((iy0 - fr.y) / fr.h) * sc.height;
    const sw = ((ix1 - ix0) / fr.w) * sc.width;
    const sh = ((iy1 - iy0) / fr.h) * sc.height;
    const dx = r.ox + ((ix0 - vx0) / vw) * r.cw;
    const dy = r.oy + ((iy0 - vy0) / vh) * r.ch;
    const dw = ((ix1 - ix0) / vw) * r.cw;
    const dh = ((iy1 - iy0) / vh) * r.ch;
    ctx.imageSmoothingEnabled = dw <= sw + 1;
    ctx.imageSmoothingQuality = 'low';
    ctx.drawImage(sc, sx, sy, sw, sh, dx, dy, dw, dh);
  }

  function updateRegion() {
    if (!preview || liveOn()) return;   // 直播模式下无屏幕扫描区域语义
    const r = viewportMonRect();
    const str = r.x + ',' + r.y + ',' + r.w + ',' + r.h;
    if (str !== prevRegionStr) {
      prevRegionStr = str;
      invoke('set_scan_region', { x: r.x, y: r.y, w: r.w, h: r.h }).catch(() => {});
    }
  }

  // ---------- 拖拽 / 缩放 ----------
  let panStart = null;
  let viewDirty = false;
  // 预览区即时提示（解释「拖拽/滚轮没反应」）：无画面 / 已是完整视图两种情形。
  let vpToastTimer = null;
  function vpToast(msg) {
    const el = $('vpToast');
    if (!el) return;
    el.textContent = msg;
    el.classList.add('on');
    clearTimeout(vpToastTimer);
    vpToastTimer = setTimeout(() => el.classList.remove('on'), 1700);
  }
  // 画面已完整可见（scale≤1 且非 cover 裁切）时，平移没有可移动余量。
  function panIsNoop() { return viewT.scale <= 1.02 && !coverFill; }
  function enterDrag() {
    if (dragging) return;
    dragging = true;
    viewDirty = false;
    ensureViewAnim();
  }
  canvas.addEventListener('mousedown', (e) => {
    if (e.button !== 0) return;
    if (!preview) { vpToast('先点「屏幕扫码」授权捕获，再拖动画面'); return; }
    if (panIsNoop()) vpToast('画面已完整显示，滚轮放大后可拖动');
    panStart = { x: e.clientX, y: e.clientY };
    canvas.classList.add('dragging');
    enterDrag();
  });
  window.addEventListener('mousemove', (e) => {
    if (!panStart) return;
    const dxCss = e.clientX - panStart.x;
    const dyCss = e.clientY - panStart.y;
    panStart = { x: e.clientX, y: e.clientY };
    if (!preview || !dragging) return;
    const rect = canvas.getBoundingClientRect();
    const k = canvas.width / rect.width;
    const r = contentRect();
    const v = viewportFrac();
    viewT.fx -= (dxCss * k * v.fw) / r.cw;
    viewT.fy -= (dyCss * k * v.fh) / r.ch;
    viewDirty = true;
    clampViewT();
    ensureViewAnim();
  });
  window.addEventListener('mouseup', () => {
    if (!panStart) return;
    panStart = null;
    canvas.classList.remove('dragging');
    dragging = false;
    ensureViewAnim();
  });

  canvas.addEventListener('wheel', (e) => {
    e.preventDefault();
    if (!preview) { vpToast('先点「屏幕扫码」授权捕获，再缩放画面'); return; }
    const rect = canvas.getBoundingClientRect();
    const sx = ((e.clientX - rect.left) * canvas.width) / rect.width;
    const sy = ((e.clientY - rect.top) * canvas.height) / rect.height;
    const r = contentRect();
    const ufx = (sx - r.ox) / r.cw;
    const ufy = (sy - r.oy) / r.ch;
    const v = viewportFrac();
    const mfx = v.x0 + ufx * v.fw;
    const mfy = v.y0 + ufy * v.fh;
    const factor = e.deltaY < 0 ? 1.15 : 1 / 1.15;
    viewT.scale = Math.min(Math.max(viewT.scale * factor, 1), 8);
    const nv = viewportFrac(viewT);
    viewT.fx = mfx + nv.fw * (0.5 - ufx);
    viewT.fy = mfy + nv.fh * (0.5 - ufy);
    viewDirty = true;
    clampViewT();
    ensureViewAnim();
    refreshPreview().catch(() => {});   // 缩放：立即补一帧追赶（快于预览循环节拍一拍）
  }, { passive: false });

  $('btnRegionReset').addEventListener('click', () => {
    if (!preview) return;
    viewT = { scale: 1, fx: 0.5, fy: 0.5 };
    ensureViewAnim();
  });

  // ---------- 画面刷新（右下角按钮）：重置当前画面源的全部状态 ----------
  //
  // 一次点击 = 三层重置：
  //   视图层：缩放/平移回中、锚定帧作废、延迟队列清空、错误提示复位；
  //   呈现层：srcCanvas 丢弃 + viewDirty，下一帧必然是整幅重绘（不会拿旧垫底帧混画）；
  //   数据源层：直播 → 重发 live_start（后端 start() 对同一 id 是「先停旧会话、
  //     再建新会话」，即解析/连接/解码全部重来），并就地更新会话信息 + 房间元数据；
  //     桌面 → set_preview_active 关再开，清掉预览关注区，帧总线下一帧必然重拷。
  // 直播重连失败时本地丢弃该会话并回退，避免永久停在封面占位上。
  // 摄像头模式没有「画面源」可刷新，直接忽略。
  let stageRefreshing = false;
  async function refreshStage() {
    if (cameraMode || stageRefreshing) return;
    stageRefreshing = true;
    const btn = $('btnVpRefresh');
    if (btn) btn.classList.add('spin');
    try {
      liveBufReset();
      clearPendingRealtime();
      view = { scale: 1, fx: 0.5, fy: 0.5 };
      viewT = { scale: 1, fx: 0.5, fy: 0.5 };
      ensureViewAnim();
      liveAnchor = null;
      liveAnchorPrev = null;
      srcCanvas = null;
      viewDirty = true;
      previewErrShown = false;
      if (liveOn()) {
        const s = activeSession();
        if (!s) { exitLiveMode(); return; }
        showCover(s.id);   // 重取期间回到封面（文案反映真实状态）
        const info = await invoke('live_start', { platform: s.platform, room: s.room });
        // 新会话的连接信息（分辨率/时长/标题都可能已变）：就地合并回会话条目，
        // note 等本地字段保留（Object.assign 只覆盖列出的键）。
        Object.assign(s, {
          w: info.w, h: info.h, title: info.title, nickname: info.nickname,
          cover: info.cover, room_id: info.room_id, duration: info.duration, status: 'live',
        });
        refreshRoomMeta(s.id);   // 侧栏标题/封面/开播状态同步（不阻塞重连后的取帧）
        renderLiveTabs();
        log('已刷新直播间 ' + s.room, 'info');
      } else {
        // 清一次预览关注区：帧总线下一帧必然重拷该区域（而不是复用缓存瓦片）。
        await invoke('set_preview_active', { active: false });
        await invoke('set_preview_active', { active: true });
        log('已刷新画面', 'info');
      }
      refreshPreview().catch(() => {});
    } catch (e) {
      log('刷新失败: ' + e, 'error');
      if (liveOn()) {
        const s = activeSession();
        if (s) closeLive(s.id);   // 重连失败：丢弃会话，回到桌面或剩余会话
      }
    } finally {
      if (btn) btn.classList.remove('spin');
      stageRefreshing = false;
    }
  }
  $('btnVpRefresh').addEventListener('click', (e) => {
    e.stopPropagation();
    refreshStage();
  });

  // ---------- FPS 徽标：暂停/恢复预览 ----------
  const PAUSE_ICON = '<svg width="9" height="9" viewBox="0 0 24 24" fill="currentColor"><rect x="6" y="5" width="4" height="14"/><rect x="14" y="5" width="4" height="14"/></svg>';
  $('fpsMeter').addEventListener('click', async (e) => {
    e.stopPropagation();
    const el = $('fpsMeter');
    if (!paused) {
      paused = true;
      stopPreview();
      el.classList.add('paused');
      el.innerHTML = PAUSE_ICON;
      if (liveOn()) return;   // 直播画面天然冻结在最后一帧
      const origW = preview ? preview.mon.w : 1920;
      capturePauseShot(origW).then(() => {
        if (paused) drawView();
        invoke('set_preview_active', { active: false }).catch(() => {});
      });
    } else {
      paused = false;
      pauseShot = null;
      el.classList.remove('paused');
      el.textContent = '--';
      invoke('set_preview_active', { active: true }).catch(() => {});
      startPreview();
    }
  });
  // 键盘可达：Tab 聚焦后 Enter/空格等价于点击（暂停/继续）。
  $('fpsMeter').addEventListener('keydown', (e) => {
    if (e.key !== 'Enter' && e.key !== ' ') return;
    e.preventDefault();
    $('fpsMeter').click();
  });

  // ---------- 渲染 ----------
  function renderScan() {
    const btn = $('btnStart');
    // 摄像头取景模式：按钮语义优先变为「停止摄像扫描」——点击只退出取景，
    // 不停止任何正在运行的扫描（屏幕 / 直播各路状态不受影响）。
    if (cameraMode) {
      // 摄像头扫码运行中：按钮呈「运行态」视觉（与屏幕/直播扫码的停止态一致）。
      btn.classList.add('running');
      btn.innerHTML = '<span>停止摄像扫描</span>';
      const mb0 = $('mBtnScan');
      if (mb0) {
        mb0.classList.add('running');
        mb0.innerHTML = '<span>停止摄像扫描</span>';
      }
      return;
    }
    const t = activeScanTarget();
    const running = !!t && scanningTargets.has(t);
    const stopping = !!t && stoppingTargets.has(t);
    btn.classList.toggle('running', running);
    btn.innerHTML = running
      ? '<span>' + (stopping ? '停止中…' : '停止扫描') + '</span>'
      : '<svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor"><path d="M6 4l14 8-14 8z"/></svg><span>开始扫码</span>';
    // 移动端底部扫码按钮同步状态。
    const mb = $('mBtnScan');
    if (mb) {
      mb.classList.toggle('running', running);
      mb.innerHTML = running
        ? '<span>' + (stopping ? '停止中…' : '停止扫描') + '</span>'
        : '<svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor"><path d="M6 4l14 8-14 8z"/></svg><span>开始扫码</span>';
    }
  }

  function renderOptions() {
    if (!config) return;
    $('alwaysOnTop').checked = !!config.always_on_top;
    $('autoClose').checked = !!config.auto_close;
    $('stopOthersOnSuccess').checked = config.stop_others_on_scan_success !== false;
    stopOthersOnScanSuccess = config.stop_others_on_scan_success !== false;
    // 屏幕扫码两个开关（默认开；`!== false` 兼容旧配置缺字段）。
    screenScanAutoBgEnabled = config.screen_scan_auto_bg !== false;
    screenScanStopOnResume = config.screen_scan_stop_on_resume !== false;
    const ssb = $('screenScanAutoBg');
    if (ssb) ssb.checked = screenScanAutoBgEnabled;
    const ssr = $('screenScanStopOnResume');
    if (ssr) ssr.checked = screenScanStopOnResume;
    // 通知开关（默认开）。
    notifyPersistent = config.notify_persistent !== false;
    notifyLiveStatus = config.notify_live_status !== false;
    const np = $('notifyPersistent');
    if (np) np.checked = notifyPersistent;
    const nl = $('notifyLiveStatus');
    if (nl) nl.checked = notifyLiveStatus;
    $('autoCheckUpdate').checked = config.auto_check_update !== false;
    $('displayName').value = config.display_name || '';
    // 显示帧率页签（30/60/120）。
    const fps = config.display_fps || 60;
    fpsSetting = fps;
    document.querySelectorAll('#fpsSeg button').forEach((b) => {
      b.classList.toggle('on', Number(b.dataset.fps) === fps);
    });
    // 直播画面缓冲（0–2000ms，步长 50；0 = 不缓冲/最低延迟）。后端已把默认值
    // 与档位对齐（normalize_display_delay），这里只需兜住「旧配置没有该字段」。
    liveBufMs = Math.max(0, Math.min(2000, Number(config.display_delay_ms) || 0));
    const dRange = $('delayRange');
    if (dRange) dRange.value = String(liveBufMs);
    const dVal = $('delayVal');
    if (dVal) dVal.textContent = liveBufMs + ' ms';
  }

  function renderAll() {
    renderGame();
    renderFilter();
    renderAccounts();
    renderScan();
    renderOptions();
    renderLiveTabs();
    updateVpInfo();
  }

  function bindOptions() {
    $('alwaysOnTop').addEventListener('change', (e) => saveCfg({ always_on_top: e.target.checked }));
    $('autoClose').addEventListener('change', (e) => saveCfg({ auto_close: e.target.checked }));
    $('stopOthersOnSuccess').addEventListener('change', (e) => {
      stopOthersOnScanSuccess = e.target.checked;
      saveCfg({ stop_others_on_scan_success: e.target.checked });
    });
    const ssbEl = $('screenScanAutoBg');
    if (ssbEl) {
      ssbEl.addEventListener('change', (e) => {
        screenScanAutoBgEnabled = e.target.checked;
        saveCfg({ screen_scan_auto_bg: e.target.checked });
      });
    }
    const ssrEl = $('screenScanStopOnResume');
    if (ssrEl) {
      ssrEl.addEventListener('change', (e) => {
        screenScanStopOnResume = e.target.checked;
        saveCfg({ screen_scan_stop_on_resume: e.target.checked });
      });
    }
    const npEl = $('notifyPersistent');
    if (npEl) {
      npEl.addEventListener('change', (e) => {
        notifyPersistent = e.target.checked;
        saveCfg({ notify_persistent: e.target.checked });
        // 立即生效：关掉时撤下常驻通知，打开且正在扫码时补上。
        syncPersistentNotification();
      });
    }
    const nlEl = $('notifyLiveStatus');
    if (nlEl) {
      nlEl.addEventListener('change', (e) => {
        notifyLiveStatus = e.target.checked;
        saveCfg({ notify_live_status: e.target.checked });
      });
    }
    $('autoCheckUpdate').addEventListener('change', (e) => saveCfg({ auto_check_update: e.target.checked }));
    // 关于弹窗：链接用系统默认浏览器打开；「检查更新」手动触发一次检查。
    const aboutOverlay = $('aboutOverlay');
    document.querySelectorAll('.about-line, .about-head').forEach((el) => {
      el.addEventListener('click', (e) => {
        const a = e.target.closest('a[data-url]');
        if (!a) return;
        e.preventDefault();
        invoke('open_url', { url: a.dataset.url }).catch(() => {});
      });
    });
    $('btnAbout').addEventListener('click', () => {
      if (!aboutOverlay) return;
      aboutOverlay.hidden = false;
      syncAboutVersion();
    });
    // 关于弹窗内的「检查更新」。
    document.querySelectorAll('[data-close]').forEach((b) => {
      if (b.dataset.close === 'aboutOverlay') {
        b.addEventListener('click', () => { aboutOverlay.hidden = true; });
      }
    });
    const btnAboutUpdate = aboutOverlay && aboutOverlay.querySelector('#btnCheckUpdate');
    if (btnAboutUpdate) {
      btnAboutUpdate.addEventListener('click', () => {
        log('正在检查更新…', 'info');
        invoke('check_update', { manual: true }).catch((e) => log(String(e), 'error'));
      });
    }
    document.querySelectorAll('#fpsSeg button').forEach((b) => {
      b.addEventListener('click', () => {
        fpsSetting = Number(b.dataset.fps) || 60;
        document.querySelectorAll('#fpsSeg button').forEach((x) => x.classList.toggle('on', x === b));
        saveCfg({ display_fps: fpsSetting });
        restartPreviewTiming();
      });
    });
    // 直播画面缓冲：拖动时只更新读数（零 IPC），松手才落盘。
    const dRange = $('delayRange');
    if (dRange) {
      dRange.addEventListener('input', () => {
        const v = Math.max(0, Math.min(2000, Math.round((Number(dRange.value) || 0) / 50) * 50));
        liveBufMs = v;
        const el = $('delayVal');
        if (el) el.textContent = v + ' ms';
      });
      dRange.addEventListener('change', () => {
        liveBufMs = Math.max(0, Math.min(2000, Number(dRange.value) || 0));
        // 缓冲口径变了：旧队列按新口径重排没有意义，直接清空（下一帧按新口径入队）。
        liveBufReset();
        saveCfg({ display_delay_ms: liveBufMs });
      });
    }
    const dn = $('displayName');
    dn.addEventListener('change', () => saveCfg({ display_name: dn.value.trim() }));
    dn.addEventListener('keydown', (e) => { if (e.key === 'Enter') { dn.blur(); } });
  }

  /// 版本号单一来源：Cargo.toml（get_app_version）；注入关于弹窗与底部栏迷你版。
  function syncAboutVersion() {
    invoke('get_app_version').then((v) => {
      ['piVer', 'piVerMini'].forEach((id) => {
        const el = $(id);
        if (el) el.textContent = 'v' + v;
      });
    }).catch(() => {});
  }

  /// 应用内停止屏幕共享（等价于系统状态栏的「停止共享」）。
  ///
  /// 停止后必须**同时**停掉正在跑的屏幕扫码循环：否则循环会继续以 60Hz 空转
  /// （拿不到帧 → 每次都走「屏幕捕获失败」分支），既费电又刷日志。
  ///
  /// `opts.quiet`：由「切回前台自动停止」等场景调用时不重复写「已停止屏幕共享」
  /// （调用方已有自己的说明日志），避免一次操作刷两条同类记录。
  async function stopScreenShare(opts) {
    const quiet = !!(opts && opts.quiet);
    try {
      if (scanningTargets.has('screen') || stoppingTargets.has('screen')) {
        stopScanTarget('screen');
      }
      await invoke('stop_screen_capture');
      if (!quiet) log('已停止屏幕共享', 'info');
    } catch (e) {
      log('停止屏幕共享失败：' + String(e), 'error');
    }
  }

  async function init() {
    bindOptions();
    if (listen) {
      await listen('accounts-updated', async () => {
        try { config = await invoke('get_config'); renderAll(); } catch (e) {}
      });
      await listen('scan-status', (ev) => {
        const p = ev.payload || {};
        const t = p.target || 'screen';
        const quiet = stoppingTargets.has(t);   // 主动停止：不重复播报
        if (p.type === 'started') {
          scanningTargets.add(t);
          stoppingTargets.delete(t);
        } else if (p.type === 'stopped' || p.type === 'success') {
          scanningTargets.delete(t);
          stoppingTargets.delete(t);
        }
        // 摄像头扫码成功：自动退出取景模式回到主界面（2026-10-17 用户实测需求）。
        // exitCameraMode 内部幂等（非取景态直接返回），且会 stopCamera
        // （停 camera 扫描 + 停物理相机）并平滑还原布局。
        if (p.type === 'success' && t === 'camera') {
          window.exitCameraMode && window.exitCameraMode();
        }
        // 扫码成功：按设置决定是否一并停止其余各路（默认停止；同一码由共享
        // 票据集防重复确认，此处仅省资源）。
        if (p.type === 'success' && stopOthersOnScanSuccess) {
          for (const other of [...scanningTargets]) {
            if (other !== t) stopScanTarget(other);
          }
        }
        renderScan();
        // 更新对应会话的状态徽标（识别中 / 已确认）；桌面目标无会话徽标。
        if (t.startsWith('live:')) {
          const s = liveSessions.find((x) => x.id === t.slice(5));
          if (s) {
            if (p.type === 'started') s.status = 'scan';
            else if (p.type === 'success') s.status = 'done';
            else if (p.type === 'stopped') s.status = 'live';
            renderLiveTabs();
            if (activeLiveId === s.id) updateVpInfo();
          }
        }
        if (!quiet) log(p.message || '', p.type === 'started' ? 'info' : (p.type || 'info'));
        // 常驻通知要跟着扫码状态走：用户可能**已经**在后台（去游戏里扫码了），
        // 此时界面不再有 visibilitychange，若不在这里主动刷新，通知栏就会一直停在
        // 「没有进行中的扫码任务」——正是最需要它准确的时候。
        syncPersistentNotification();
        // 通知栏提醒（仅 Android；由设置项 notify_live_status 控制）：
        // 界面此时多半在后台（去游戏里扫码了），通知是唯一可见的反馈渠道。
        if (notifyLiveStatus && !quiet) {
          notifyEvent(p.type, t, p.message || '');
        }
      });
      // 更新检查结果（自动检查/手动检查共用：把结果写进日志面板）。
      // Android 的「发现新版本」不带安装包，只带 Release 页面 url——手动检查时
      // 直接交给系统浏览器（桌面端后端会自行下载安装，不会带 url）。
      await listen('update-status', (ev) => {
        const p = ev.payload || {};
        log(p.message || '', p.kind === 'error' ? 'error' : 'info');
        if (p.url) invoke('open_url', { url: p.url }).catch((e) => log(String(e), 'error'));
      });
      // 屏幕共享被系统/用户停止（状态栏「停止共享」/ Android 14 一次性令牌到期 /
      // 系统回收）。此时帧管道已断开，屏幕扫码循环再跑也拿不到帧——停掉它并提示
      // 重新授权，避免用户以为还在扫却毫无反应。
      await listen('capture-stopped', () => {
        // 共享已由系统侧（通知栏「停止共享」）或我们主动结束：清掉「切后台自动停」
        // 的待办标记，避免下次回前台再走一遍无意义的停止流程。
        screenScanAutoBg = false;
        if (scanningTargets.has('screen')) {
          stopScanTarget('screen');
          log('屏幕共享已停止，如需继续屏幕扫码请重新授权', 'warn');
        }
      });
    }
    try { config = await invoke('get_config'); } catch (e) { config = null; log('加载配置失败: ' + e, 'error'); }
    if (config) {
      filter = initialFilterForGame(config.current_game || 'bh3');
    }
    // 恢复界面列宽（侧栏 / 游戏列；拖动结束落盘，见 set_ui_widths）。
    if (config && config.sidebar_w) {
      const appRect = document.querySelector('.app').getBoundingClientRect();
      const w = Math.min(Math.max(config.sidebar_w, 140), Math.min(240, (appRect.width || 760) - 320));
      document.querySelector('.sidebar').style.width = w + 'px';
    }
    if (config && config.game_col_w) {
      const w = Math.min(Math.max(config.game_col_w, 70), 140);
      document.querySelector('.game-wrap').style.width = w + 'px';
      updateChanLabels();
    }
    renderAll();
    invoke('refresh_official_unames').catch(() => {});
    // 弹幕 WS 的实时开播/下播推送（先绑事件，再同步监测列表，避免漏首报）。
    bindLiveStatusEvents();
    // 加载已保存直播间（含未开播）并启动元数据轮询（兜底对账）。
    await reloadLiveRooms();
    pollMetaRound();
    startMetaPoll();
    // 恢复上次观看的直播间（离线显示封面、开播则自动连接）。
    // 仅恢复 `last_live_id` 记录过的直播间；未设定过则中间黑屏（不自动选第一个，
    // 也不显示占位），等待用户主动点击直播间。
    {
      const lastLive = (config && config.last_live_id) || '';
      // 桌面端初始进入底层循环也基于 activeLiveId 决定扫屏幕还是直播；
      // 未恢复时保持 null（黑屏），让用户自己选。
      const has = (id) => liveRooms.some((r) => (r.platform + ':' + r.room) === id);
      if (lastLive && has(lastLive)) {
        switchLive(lastLive);
      }
    }
    // 移动端：初始化标题栏（未恢复直播间时显示屏幕预览；有恢复的话切换流程会更新标题）。
    if (window.__MOBILE__ && window.updateMobileTitle && !activeLiveId) {
      window.updateMobileTitle('屏幕预览', '未连接直播');
    }
    log('应用就绪。添加账号后点击「开始扫码」即可自动识别并登录', 'info');
    // 版本号（单一来源：Cargo.toml）：注入关于弹窗与底部栏迷你版。
    syncAboutVersion();
    // 自动检查更新：延迟 5 秒（让启动期资源先就位）；无更新/失败时后端静默。
    if (config && config.auto_check_update !== false) {
      setTimeout(() => invoke('check_update', { manual: false }).catch(() => {}), 5000);
    }
    startPreview();
  }


  // ===================== 移动端交互（仅 html.mobile 生效） =====================
  if (window.__MOBILE__) {
    // --- 标题栏：主标题 + 副标题；放不下时堆叠两行 ---
    function updateMobileTitle(title, sub) {
      const wrap = document.querySelector(".m-titlewrap");
      const t = $("mTitle"), s = $("mSub");
      if (!t || !s) return;
      t.textContent = title || "";
      s.textContent = sub || "";
      if (!wrap) return;
      // 先按一行试排，测量是否溢出；溢出则改两行堆叠。
      wrap.classList.add("inline");
      requestAnimationFrame(() => {
        const tw = t.scrollWidth, sw = s.scrollWidth;
        const avail = wrap.clientWidth - 8;
        if (tw + sw > avail || tw > avail) wrap.classList.remove("inline");
      });
    }
    window.updateMobileTitle = updateMobileTitle;

    // --- 抽屉滑动模型（2026-10-15：三面板拼接、跟手位移、自动吸附） ---
    // 模型：主界面（1 宽）与左右抽屉（各 0.8 视口宽）拼为 2.6，--m-swipe 为主界面
    // 位移（+ 露左抽屉 / - 露右抽屉，0 为居中）；松手按位置与速度吸附三档。
    const sidebar = document.querySelector(".sidebar");
    const tools = $("toolsOverlay");
    const mobileRoot = document.documentElement;
    const drawerW = () => Math.round(window.innerWidth * 0.8);
    const SWIPE_EDGE = 26; // 屏幕边缘预留（系统返回手势区）
    let swipeTarget = 0; // 当前吸附目标（px）
    let swipeCurrent = 0; // 当前实际位移（逻辑值，由 applySwipe 维护）
    let dragState = null;

    function applySwipe(tx) {
      swipeCurrent = tx;
      mobileRoot.style.setProperty("--m-swipe", tx + "px");
    }
    function settleSwipe(tx) {
      swipeTarget = tx;
      mobileRoot.classList.remove("m-swiping");
      applySwipe(tx);
    }
    function drawerOpen() {
      return swipeTarget > 0 ? "left" : swipeTarget < 0 ? "right" : "";
    }

    function openLeft() {
      settleSwipe(drawerW());
      try { invoke("frontend_log", { level: "info", msg: "移动端：打开直播间抽屉" }); } catch (e) {}
    }
    function closeLeft() { settleSwipe(0); }
    function openRight() {
      settleSwipe(-drawerW());
      // 与桌面端一致：打开设置时顺带请求一次通知权限（幂等，见
      // requestNotifyPermissionOnce）。
      if (typeof requestNotifyPermissionOnce === "function") requestNotifyPermissionOnce();
    }
    function closeRight() { settleSwipe(0); }
    window.closeLeft = closeLeft;
    window.closeRight = closeRight;
    window.closeMGameOverlay = function () { const o = $("mGameOverlay"); if (o) o.hidden = true; };

    // 拼接模型下工具面板常驻（以 transform 藏于屏外，不再用 hidden 开关）。
    if (tools) tools.hidden = false;

    // --- 手势：跟手位移 + 松手吸附（pointer 事件，触屏/鼠标通用） ---
    function anyModalOpen() {
      return Array.from(document.querySelectorAll(".overlay")).some(
        (o) => o.id !== "toolsOverlay" && !o.hidden
      );
    }
    window.__anyModalOpen = anyModalOpen;
    document.addEventListener("pointerdown", (e) => {
      if (anyModalOpen()) return;
      const W = window.innerWidth;
      if (e.clientX < SWIPE_EDGE || e.clientX > W - SWIPE_EDGE) return; // 边缘预留
      // 预览区（canvas）归其自身拖拽/缩放交互（触摸与鼠标均可平移画面），
      // 全屏抽屉手势在此让位；其余区域保持全区域跟手。
      if (e.target.closest(".vp-stage")) return;
      dragState = {
        x: e.clientX, y: e.clientY, base: swipeCurrent,
        lastX: e.clientX, lastT: performance.now(), vx: 0, active: false,
      };
    }, { passive: true });
    document.addEventListener("pointermove", (e) => {
      if (!dragState) return;
      const dx = e.clientX - dragState.x, dy = e.clientY - dragState.y;
      if (!dragState.active) {
        if (Math.abs(dx) < 8) return;
        if (Math.abs(dx) < Math.abs(dy) * 1.15) { dragState = null; return; } // 纵向：放行滚动
        dragState.active = true;
        mobileRoot.classList.add("m-swiping");
      }
      const now = performance.now();
      if (now - dragState.lastT > 15) {
        dragState.vx = (e.clientX - dragState.lastX) / (now - dragState.lastT);
        dragState.lastX = e.clientX;
        dragState.lastT = now;
      }
      const DW = drawerW();
      applySwipe(Math.max(-DW, Math.min(DW, dragState.base + dx)));
    }, { passive: true });
    function endDrag() {
      if (!dragState) return;
      const s = dragState;
      dragState = null;
      if (!s.active) return; // 点按：由下方 click 逻辑处理
      const DW = drawerW();
      const cur = swipeCurrent;
      // 三档吸附（-DW / 0 / +DW）：以「手势起点档位」为基准，一次手势最多跨一档——
      // 从某侧抽屉滑回时吸回中心，而非被速度方向误判到对侧。
      const slots = [-DW, 0, DW];
      const from = slots.reduce((a, b) => (Math.abs(b - s.base) < Math.abs(a - s.base) ? b : a));
      const fromIdx = slots.indexOf(from);
      const moved = cur - from;
      const dir = Math.sign(moved) || Math.sign(s.vx) || 0;
      const fast = Math.abs(s.vx) > 0.3 && Math.sign(s.vx) === dir && dir !== 0;
      const cross = fast || Math.abs(moved) > DW * 0.3;
      const idx = Math.min(2, Math.max(0, fromIdx + (cross ? dir : 0)));
      settleSwipe(slots[idx]);
    }
    document.addEventListener("pointerup", endDrag, { passive: true });
    document.addEventListener("pointercancel", endDrag, { passive: true });

    // 抽屉打开时点击主界面 → 收回（吞掉该次点击，避免误触主界面按钮）。
    // 注：弹层打开时不做拦截（弹层交互优先）；顶栏按钮不拦截（其自身 toggle 逻辑
    // 负责开/关，允许"左开时点右按钮直接切到右侧"）。
    //
    // **弹窗内的点击也必须放行**：弹窗（如「关于」「修改备注」）是挂在整个视口上的
    // 遮罩，用户点它空白处即关闭弹窗——而弹窗是在 mousedown 阶段被置 hidden 的，
    // 等到 click 阶段 `anyModalOpen()` 已经变回 false，于是这段逻辑会把「关弹窗」
    // 误当成「点了主界面」，顺带把侧栏抽屉也收回、直接跳回主界面（用户反馈的问题）。
    // 故在指针按下时**先记下**当时是否有弹窗，click 阶段据此跳过抽屉收回。
    let modalOpenAtPointerDown = false;
    document.addEventListener("pointerdown", () => {
      modalOpenAtPointerDown = anyModalOpen();
    }, { capture: true, passive: true });
    document.addEventListener("click", (e) => {
      if (!drawerOpen()) return;
      // 本次点击起始于弹窗之上：交给弹窗的关闭逻辑，抽屉保持原地。
      if (modalOpenAtPointerDown) { modalOpenAtPointerDown = false; return; }
      if (anyModalOpen()) return;
      // 直接点中弹窗遮罩本身也算弹窗交互（双保险：某些环境 click 会落在遮罩上）。
      if (e.target.classList && e.target.classList.contains("overlay")) return;
      if (e.target.closest("#toolsOverlay .dialog") || e.target.closest(".sidebar") || e.target.closest(".m-topbar")) return;
      settleSwipe(0);
      e.stopPropagation();
      e.preventDefault();
    }, true);

    // 视口变化（旋转/尺寸变化）：按当前档位重新贴合。
    window.addEventListener("resize", () => {
      if (swipeTarget > 0) settleSwipe(drawerW());
      else if (swipeTarget < 0) settleSwipe(-drawerW());
      else applySwipe(0);
    });

    $("mBtnLeft").addEventListener("click", () => {
      if (swipeTarget > 0) closeLeft(); else openLeft();
    });
    $("mBtnRight").addEventListener("click", () => {
      if (swipeTarget < 0) closeRight(); else openRight();
    });
    document.querySelectorAll('[data-close="toolsOverlay"]').forEach((b) => {
      b.addEventListener("click", () => { closeRight(); });
    });

    // --- 游戏 / 区服选择弹窗 ---
    const mGameBtn = $("mGameBtn"), mChanBtn = $("mChanBtn");
    function renderMGameList() {
      const box = $("mGameList");
      if (!box) return;
      box.textContent = "";
      const cur = config ? (config.current_game || "bh3") : "bh3";
      for (const g of GAMES) {
        const it = document.createElement("div");
        it.className = "gitem" + (g.id === cur ? " on" : "");
        it.style.height = "54px";
        it.style.display = "flex";
        it.style.alignItems = "center";
        it.style.padding = "0 12px";
        it.style.borderRadius = "var(--m-r-md, 14px)";
        it.style.border = "1px solid var(--border)";
        it.style.fontSize = "var(--m-fs-lg, 15px)";
        it.textContent = g.name;
        it.addEventListener("click", () => switchGame(g.id));
        box.appendChild(it);
      }
    }
    function renderMChanList() {
      const box = $("mChanList");
      if (!box) return;
      box.textContent = "";
      const g = gameMeta(config ? (config.current_game || "bh3") : "bh3");
      const opts = [];
      if (g.official) opts.push(["official", "官服"]);
      if (g.bili) opts.push(["bilibili", "B 服"]);
      if (g.hyper) opts.push(["hypergryph", "鹰角账号"]);
      // 第三方渠道（渠道登录）：**仅崩坏3** 有渠道服（小米/华为/应用宝等）。
      // 其它米哈游游戏与鹰角游戏都没有渠道服，只上官服 / B服 / 鹰角账号。
      if (g.id === "bh3") {
        for (const c of CHANNELS_UI) opts.push(["channel:" + c.key, c.label]);
      }
      for (const [f, label] of opts) {
        const it = document.createElement("div");
        it.className = "gitem" + (f === filter ? " on" : "");
        it.style.height = "54px";
        it.style.display = "flex";
        it.style.alignItems = "center";
        it.style.padding = "0 12px";
        it.style.borderRadius = "var(--m-r-md, 14px)";
        it.style.border = "1px solid var(--border)";
        it.style.fontSize = "var(--m-fs-lg, 15px)";
        it.textContent = label;
        it.addEventListener("click", () => {
          filter = f;
          renderFilter();
          renderAccounts();
          $("mChanOverlay").hidden = true;
        });
        box.appendChild(it);
      }
    }
    mGameBtn.addEventListener("click", () => { renderMGameList(); $("mGameOverlay").hidden = false; });
    mChanBtn.addEventListener("click", () => { renderMChanList(); $("mChanOverlay").hidden = false; });

    // --- 底部栏 ---
    $("mBtnScan").addEventListener("click", async () => {
      if (cameraMode) { exitCameraMode(); return; }
      // 直播扫码走 FLV→MediaCodec 硬解，**不需要** MediaProjection 屏幕捕获授权
      // （授权框只属于屏幕扫码路径）。仅当目标是桌面（screen）时才请求授权。
      const t = activeScanTarget();
      if (t === 'screen' && !(await ensureScreenCapture())) return;
      $("btnStart").click();
    });
    $("mBtnScreenScan").addEventListener("click", async () => {
      // 屏幕扫码：先确保捕获授权，再切到桌面目标（无直播会话时）并开始扫码。
      if (!(await ensureScreenCapture())) return;
      // 记住当前正在看的直播间：exitLiveMode() 会清空它，回前台时据此恢复
      // （见 visibilitychange 里的 screenScanPrevLive 处理）。
      screenScanPrevLive = activeLiveId;
      exitLiveMode();
      renderScan();
      // 若已在扫屏幕：再点一次 = 停止。
      if (scanningTargets.has("screen") || stoppingTargets.has("screen")) {
        stopScanTarget("screen");
        return;
      }
      setTimeout(() => {
        $("btnStart").click();
        // 屏幕扫码需用户切到游戏/其它应用才能扫到二维码。是否自动切后台由设置项
        // `screen_scan_auto_bg` 控制（默认开）：关掉则停在应用内，由用户自行切换
        // （分屏 / 多任务场景）。
        if (screenScanAutoBgEnabled) {
          vpToast("已开启屏幕扫码，请移动到其他应用扫描二维码");
          screenScanAutoBg = true;
          invoke("move_to_back").catch(() => {});
        } else {
          vpToast("已开启屏幕扫码，请切换到游戏后返回本应用停止扫码");
        }
      }, 50);
    });
    // --- 摄像头扫码取景模式（进入 / 退出） ---
    // 纯视图切换：把 .acct-area（账号 + 游戏/区服选择）高度平滑收到 0，
    // 让出的高度全部交给中间显示区；顶栏与底部操作栏保持不动。
    const acctArea = document.querySelector(".acct-area");
    const camView = $("mCameraView");
    const camBtn = $("mBtnCameraScan");
    function enterCameraMode() {
      if (cameraMode) return;
      cameraSavedH = acctArea ? acctArea.getBoundingClientRect().height : 0;
      cameraMode = true;
      if (acctArea) acctArea.style.height = cameraSavedH + "px";
      mobileRoot.classList.add("m-camera");
      requestAnimationFrame(() => { if (acctArea) acctArea.style.height = "0px"; });
      if (camView) camView.removeAttribute("hidden");
      if (camBtn) camBtn.classList.add("on");
      updateVpInfo();
      renderScan();
      startCamera();
    }
    window.exitCameraMode = function exitCameraMode() {
      if (!cameraMode) return;
      cameraMode = false;
      stopCamera();
      if (camView) camView.setAttribute("hidden", "");
      if (camBtn) camBtn.classList.remove("on");
      renderScan();
      updateVpInfo();
      const finish = () => {
        if (acctArea) acctArea.style.height = "";
        mobileRoot.classList.remove("m-camera");
        if (acctArea) acctArea.removeEventListener("transitionend", finish);
      };
      if (acctArea) {
        acctArea.addEventListener("transitionend", finish);
        acctArea.style.height = cameraSavedH + "px";   // 平滑还原到进入前的高度
        setTimeout(finish, 420);                        // 兜底（transitionend 未触发时）
      } else finish();
    };
    camBtn.addEventListener("click", () => {
      if (cameraMode) window.exitCameraMode();
      else enterCameraMode();
    });

    // --- 系统返回键：抽屉打开 / 摄像头取景时拦截，先回到主界面 ---
    // Android 返回键在 WebView 中触发 history back。用 pushState 垫一层「哨兵」，
    // popstate 时若处于抽屉或取景态则只做界面复位并补回哨兵（不退出应用）。
    (function bindBackHandler() {
      // 是否消费本次返回：取景态 / 抽屉打开 / 有弹窗时先做界面复位。
      // Android 侧 MainActivity 拦截返回键后同步调用本函数（见 patch-gen.ps1 步骤 5a），
      // 返回 false 时由原生层 moveTaskToBack（回桌面保活），不再走 Activity.finish。
      function consumeBack() {
        if (cameraMode) { window.exitCameraMode(); return true; }
        // 弹窗优先于抽屉：侧栏里打开的弹窗（关于 / 修改备注…）应先被关掉，
        // 抽屉保持在原地——否则按一次返回会连带把抽屉收回去、跳回主界面，
        // 与「点空白关弹窗应留在侧栏」的预期不一致（用户反馈）。
        if (window.__anyModalOpen && window.__anyModalOpen()) {
          // 关掉当前打开的所有弹窗（含登录弹窗需取消二维码轮询）。
          document.querySelectorAll(".overlay").forEach((o) => {
            if (o.hidden || o.id === "toolsOverlay") return;
            o.hidden = true;
            if (o.id === "loginOverlay") cancelQr();
          });
          return true;
        }
        if (swipeTarget !== 0) { settleSwipe(0); return true; }
        return false;
      }
      window.__gsHandleBack = consumeBack;

      try { history.pushState({ gsGuard: 1 }, ""); } catch (e) {}
      window.addEventListener("popstate", () => {
        if (consumeBack()) {
          // 补回哨兵：下一次返回继续由本处理器接管。
          try { history.pushState({ gsGuard: 1 }, ""); } catch (e) {}
        }
        // false：哨兵已弹出且无界面可退 → 交给系统（退出应用）。
      });
    })();

    // --- 摄像头捕获 + 预览 + 扫码 ---
    const mcCanvas = $("mcCanvas");
    const mcTip = $("mcCamTip");
    async function startCamera() {
      if (mcTip) mcTip.textContent = "正在启动摄像头…";
      try {
        await invoke("request_camera_permission");
      } catch (e) {
        if (mcTip) mcTip.textContent = "相机权限被拒绝";
        log("相机权限被拒绝：" + String(e), "error");
        return;
      }
      try {
        await invoke("start_camera_capture");
      } catch (e) {
        if (mcTip) mcTip.textContent = "启动相机失败";
        log("启动相机失败：" + String(e), "error");
        return;
      }
      if (mcTip) mcTip.textContent = "";
      // 读取 sensor 方向（预览旋转用）。
      try { mcRotation = await invoke("camera_rotation"); } catch (_) { mcRotation = 90; }
      // 启动摄像头扫码目标（不依赖 activeScanTarget）。
      invoke("start_scan", { target: "camera" }).catch((e) => log(String(e), "error"));
      scheduleCameraFrame();
    }
    function stopCamera() {
      if (cameraPreviewTimer) { clearTimeout(cameraPreviewTimer); cameraPreviewTimer = null; }
      invoke("stop_scan", { target: "camera" }).catch(() => {});
      invoke("stop_camera_capture").catch(() => {});
    }
    function scheduleCameraFrame() {
      if (!cameraMode) return;
      cameraPreviewTimer = setTimeout(pullCameraFrame, 66);
    }
    // 离屏源画布：存放相机原始帧（未旋转）。
    let mcSrc = null;
    let mcRotation = 90;
    async function pullCameraFrame() {
      if (!cameraMode || cameraPreviewBusy) { scheduleCameraFrame(); return; }
      cameraPreviewBusy = true;
      try {
        const buf = await invoke("capture_camera_frame", { maxWidth: 720 });
        if (buf instanceof ArrayBuffer && buf.byteLength > 24) {
          const dv = new DataView(buf);
          const pw = dv.getUint32(0, true), ph = dv.getUint32(4, true);
          if (pw && ph && 24 + pw * ph * 4 <= buf.byteLength) {
            if (!mcSrc) mcSrc = document.createElement("canvas");
            if (mcSrc.width !== pw || mcSrc.height !== ph) {
              mcSrc.width = pw; mcSrc.height = ph;
            }
            const img = new ImageData(new Uint8ClampedArray(buf, 24, pw * ph * 4), pw, ph);
            mcSrc.getContext("2d").putImageData(img, 0, 0);
            drawCameraFrame();
          }
        }
      } catch (_) {}
      cameraPreviewBusy = false;
      scheduleCameraFrame();
    }
    // 把源帧按 sensor 方向旋转 90/270° 后以 contain 适配绘入取景画布。
    function drawCameraFrame() {
      if (!mcSrc) return;
      // 显示区尺寸（取景层容器）。
      const host = camView || mcCanvas;
      const hostW = host.clientWidth || 480;
      const hostH = host.clientHeight || 270;
      const rot = ((mcRotation % 360) + 360) % 360;
      const swap = rot === 90 || rot === 270;
      const srcW = mcSrc.width, srcH = mcSrc.height;
      const outW = swap ? srcH : srcW;
      const outH = swap ? srcW : srcH;
      // contain 适配显示区（不拉伸、不裁切）。
      const scale = Math.min(hostW / outW, hostH / outH);
      const dispW = Math.max(1, Math.round(outW * scale));
      const dispH = Math.max(1, Math.round(outH * scale));
      // 画布物理尺寸 = 显示尺寸（1:1，避免 CSS 拉伸失真）。
      if (mcCanvas.width !== dispW || mcCanvas.height !== dispH) {
        mcCanvas.width = dispW; mcCanvas.height = dispH;
      }
      mcCanvas.style.width = dispW + "px";
      mcCanvas.style.height = dispH + "px";
      const ctx = mcCanvas.getContext("2d");
      ctx.save();
      ctx.translate(dispW / 2, dispH / 2);
      ctx.rotate((rot * Math.PI) / 180);
      const dw = (swap ? dispH : dispW);
      const dh = (swap ? dispW : dispH);
      ctx.drawImage(mcSrc, -dw / 2, -dh / 2, dw, dh);
      ctx.restore();
    }



    // --- 3:2 视口：双击切换 contain/cover + 触摸手势 ---
    (function bindViewportTouch() {
      const cv = $("previewCanvas");
      if (!cv) return;
      // 单指平移 / 双指捏合缩放（双击放大已移除，仅保留双指缩放与拖拽平移）
      let t0 = null, t1 = null, pinchDist0 = 0, scale0 = 1;
      cv.addEventListener("touchstart", (e) => {
        if (!preview) { vpToast('先点「屏幕扫码」授权捕获，再捏合缩放'); return; }
        if (e.touches.length === 1) {
          if (panIsNoop()) vpToast('画面已完整显示，双指放大后可拖动');
          t0 = { x: e.touches[0].clientX, y: e.touches[0].clientY };
          enterDrag();
        } else if (e.touches.length === 2) {
          t0 = { x: e.touches[0].clientX, y: e.touches[0].clientY };
          t1 = { x: e.touches[1].clientX, y: e.touches[1].clientY };
          pinchDist0 = Math.hypot(t1.x - t0.x, t1.y - t0.y);
          scale0 = viewT.scale;
        }
      }, { passive: true });
      cv.addEventListener("touchmove", (e) => {
        if (!preview) return;
        e.preventDefault();
        if (e.touches.length === 1 && t0 && !t1) {
          const dx = e.touches[0].clientX - t0.x;
          const dy = e.touches[0].clientY - t0.y;
          t0 = { x: e.touches[0].clientX, y: e.touches[0].clientY };
          const rect = cv.getBoundingClientRect();
          const k = cv.width / rect.width;
          const r = contentRect();
          const v = viewportFrac();
          viewT.fx -= (dx * k * v.fw) / r.cw;
          viewT.fy -= (dy * k * v.fh) / r.ch;
          viewDirty = true;
          clampViewT();
          ensureViewAnim();
        } else if (e.touches.length === 2 && t1) {
          const d = Math.hypot(e.touches[1].clientX - e.touches[0].clientX, e.touches[1].clientY - e.touches[0].clientY);
          const ratio = pinchDist0 > 0 ? d / pinchDist0 : 1;
          viewT.scale = Math.min(Math.max(scale0 * ratio, 1), 8);
          viewDirty = true;
          clampViewT();
          ensureViewAnim();
        }
      }, { passive: false });
      cv.addEventListener("touchend", (e) => {
        if (e.touches.length === 0) { t0 = null; t1 = null; dragging = false; ensureViewAnim(); }
        else if (e.touches.length === 1) { t1 = null; t0 = { x: e.touches[0].clientX, y: e.touches[0].clientY }; }
      }, { passive: true });
    })();
    // 注：早期「账号区滑动松手跳切」的 bindAcctSwipe 已移除——
    // 全屏跟手滑动统一由文档级手势模型（pointer 事件）处理。
  }
  moduleOk = true;   // 脚本求值完成（自检哨兵）
  init();
})();
