use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::crypto;

/// 游戏。支持米哈游系（崩坏3/星穹铁道/原神/绝区零）与鹰角系（终末地/明日方舟）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Game {
    /// 崩坏3
    Bh3,
    /// 崩坏：星穹铁道
    Hsr,
    /// 原神
    Genshin,
    /// 绝区零
    Zzz,
    /// 明日方舟：终末地（鹰角系）
    Endfield,
    /// 明日方舟（鹰角系）
    Arknights,
}

impl Game {
    /// 从字符串解析游戏（前端传参用）。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "bh3" => Some(Game::Bh3),
            "hsr" => Some(Game::Hsr),
            "genshin" => Some(Game::Genshin),
            "zzz" => Some(Game::Zzz),
            "endfield" => Some(Game::Endfield),
            "arknights" => Some(Game::Arknights),
            _ => None,
        }
    }

    /// 是否为鹰角系游戏（用鹰角账号确认，而非米哈游/B站账号）。
    pub fn is_hypergryph(self) -> bool {
        matches!(self, Game::Endfield | Game::Arknights)
    }

    /// 游戏中文显示名（日志与提示文案用）。
    pub fn label(self) -> &'static str {
        match self {
            Game::Bh3 => "崩坏3",
            Game::Hsr => "崩坏：星穹铁道",
            Game::Genshin => "原神",
            Game::Zzz => "绝区零",
            Game::Endfield => "明日方舟：终末地",
            Game::Arknights => "明日方舟",
        }
    }

    /// 该游戏的米哈游 app_id（B 服 combo/panda 与官服确认用）。
    /// 鹰角系游戏无此概念（返回 0，不参与米哈游确认流程）。
    pub fn app_id(self) -> i64 {
        match self {
            Game::Bh3 => 1,
            Game::Hsr => 8,
            Game::Genshin => 4,
            Game::Zzz => 12,
            Game::Endfield | Game::Arknights => 0,
        }
    }
}

/// 渠道
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Bilibili,
    Official,
}

impl Channel {
    /// 从字符串解析渠道（前端传参用）。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "bilibili" => Some(Channel::Bilibili),
            "official" => Some(Channel::Official),
            _ => None,
        }
    }

    /// 渠道中文名。
    pub fn label(self) -> &'static str {
        match self {
            Channel::Bilibili => "B 服",
            Channel::Official => "官服",
        }
    }
}

/// B 站账号令牌（一个账号一条记录）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BiliToken {
    pub uid: String,
    pub access_key: String,
    pub uname: String,
    /// 所属游戏 id（如 "bh3"）。
    #[serde(default)]
    pub game: String,
    /// 用户备注名（双击账号行编辑；有则主界面显示「备注名（原名）」）。
    #[serde(default)]
    pub alias: String,
    /// 用户备注信息（双击账号行编辑，右侧靠右显示）。
    #[serde(default)]
    pub remark: String,
    pub last_login_succ: bool,
    /// 最近一次作为当前账号使用 / 登录的时间（Unix 秒）。
    #[serde(default)]
    pub last_used_at: i64,
}

/// 官服账号令牌（一个账号一条记录）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OfficialToken {
    pub uid: String,
    pub stoken: String,
    pub mid: String,
    /// 米游社昵称（登录/校验响应中获取，仅用于展示）。
    #[serde(default)]
    pub uname: String,
    /// 所属游戏 id（如 "bh3"）。
    #[serde(default)]
    pub game: String,
    /// 用户备注名（双击账号行编辑；有则主界面显示「备注名（原名）」）。
    #[serde(default)]
    pub alias: String,
    /// 用户备注信息（双击账号行编辑，右侧靠右显示）。
    #[serde(default)]
    pub remark: String,
    pub last_login_succ: bool,
    /// 最近一次作为当前账号使用 / 登录的时间（Unix 秒）。
    #[serde(default)]
    pub last_used_at: i64,
}

/// 直播间条目（受监控的直播间；含元数据缓存，非敏感数据明文存储）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveHistoryEntry {
    /// 房间号（数字字符串；B站/抖音通用）。
    pub room: String,
    /// 直播平台（bilibili / douyin）。
    #[serde(default = "default_live_platform")]
    pub platform: String,
    /// 用户备注（可选，如主播名）。
    #[serde(default)]
    pub note: String,
    /// 最后使用时间（Unix 秒）。
    pub last_used: i64,
    /// 缓存：直播间标题。
    #[serde(default)]
    pub title: String,
    /// 缓存：主播昵称。
    #[serde(default)]
    pub nickname: String,
    /// 缓存：封面 URL。
    #[serde(default)]
    pub cover: String,
    /// 缓存：真实房间号。
    #[serde(default)]
    pub room_id: String,
    /// 缓存：开播状态（1=直播中）。
    #[serde(default)]
    pub live_status: i64,
    /// 缓存：开播时刻（Unix 秒）。
    #[serde(default)]
    pub live_start: i64,
    /// 缓存：元数据最后刷新时间（Unix 秒）。
    #[serde(default)]
    pub last_checked: i64,
}

fn default_live_platform() -> String {
    "bilibili".to_string()
}

/// 受监控直播间上限（含桌面共 30 个，超出丢弃最旧）。
pub const LIVE_HISTORY_MAX: usize = 30;

/// 应用配置（持久化）。
/// 同渠道支持多账号：`*_accounts` 保存全部登录记录，`active_*` 以 uid 指向当前使用的账号。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub bili_accounts: Vec<BiliToken>,
    pub official_accounts: Vec<OfficialToken>,
    /// 鹰角账号（终末地/明日方舟用；独立账号体系，手机号+验证码登录）。
    pub hypergryph_accounts: Vec<crate::sdk::hypergryph::HypergryphToken>,
    /// 当前游戏（决定 biz_key 与确认端点）。默认崩坏3。
    pub current_game: Game,
    /// 各游戏「上次选中的 B 服账号」uid（game → uid）。
    #[serde(default)]
    pub active_bili: std::collections::HashMap<String, String>,
    /// 各游戏「上次选中的官服账号」uid（game → uid）。
    #[serde(default)]
    pub active_official: std::collections::HashMap<String, String>,
    /// 各游戏「上次选中的鹰角账号」phone（game → phone）。
    #[serde(default)]
    pub active_hypergryph: std::collections::HashMap<String, String>,
    /// 各游戏「上次使用的渠道」，切换游戏时恢复（game → channel）。
    #[serde(default)]
    pub game_channel: std::collections::HashMap<String, Option<Channel>>,
    /// 游戏内显示的账号名（B 服 combo/panda confirm 的 asterisk_name 字段）。
    /// 空则用默认「游戏扫码器用户」。
    #[serde(default)]
    pub display_name: String,
    /// 启动后自动使用上次使用的账号并登录（即自动开始扫描）。
    pub auto_login_last: bool,
    /// 窗口置顶。
    pub always_on_top: bool,
    pub auto_close: bool,
    /// 显示区域（预览）目标帧率：30 / 60 / 120。默认 60。
    #[serde(default = "default_display_fps")]
    pub display_fps: u32,
    pub win_x: Option<i32>,
    pub win_y: Option<i32>,
    pub device_id: String,
    /// 真实设备指纹（米哈游登录风控必需；空则由 getFp 接口获取后写入）。
    #[serde(default)]
    pub device_fp: String,
    /// 设备指纹随机种子（getFp 用）。
    #[serde(default)]
    pub fp_seed_id: String,
    #[serde(default)]
    pub fp_seed_time: String,
    /// 直播间使用历史（最近优先，去重，上限 LIVE_HISTORY_MAX）。
    #[serde(default)]
    pub live_history: Vec<LiveHistoryEntry>,
}

/// display_fps 的 serde 默认值（旧配置无此字段时用 60）。
fn default_display_fps() -> u32 {
    60
}

/// 显示帧率白名单（设置项仅 30/60/120；配置被手改时收敛到 60）。
pub fn normalize_display_fps(v: u32) -> u32 {
    match v {
        30 | 60 | 120 => v,
        _ => 60,
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            bili_accounts: Vec::new(),
            official_accounts: Vec::new(),
            hypergryph_accounts: Vec::new(),
            current_game: Game::Bh3,
            active_bili: std::collections::HashMap::new(),
            active_official: std::collections::HashMap::new(),
            active_hypergryph: std::collections::HashMap::new(),
            game_channel: std::collections::HashMap::new(),
            display_name: String::new(),
            auto_login_last: false,
            always_on_top: false,
            auto_close: false,
            display_fps: default_display_fps(),
            win_x: None,
            win_y: None,
            device_id: String::new(),
            device_fp: String::new(),
            fp_seed_id: String::new(),
            fp_seed_time: String::new(),
            live_history: Vec::new(),
        }
    }
}

impl AppConfig {
    /// 当前激活的 B 服账号。
    pub fn active_bili(&self, game: &str) -> Option<&BiliToken> {
        let uid = self.active_bili.get(game)?;
        self.bili_accounts
            .iter()
            .find(|t| t.uid == *uid && t.game == game)
    }

    /// 某游戏当前激活的官服账号。
    pub fn active_official(&self, game: &str) -> Option<&OfficialToken> {
        let uid = self.active_official.get(game)?;
        self.official_accounts
            .iter()
            .find(|t| t.uid == *uid && t.game == game)
    }

    /// 某游戏当前激活的鹰角账号。
    pub fn active_hypergryph(
        &self,
        game: &str,
    ) -> Option<&crate::sdk::hypergryph::HypergryphToken> {
        let phone = self.active_hypergryph.get(game)?;
        self.hypergryph_accounts
            .iter()
            .find(|t| t.phone == *phone && t.game == game)
    }

    /// 新增或更新鹰角账号并设为激活。
    pub fn upsert_hypergryph(&mut self, token: crate::sdk::hypergryph::HypergryphToken) {
        if token.phone.is_empty() {
            return;
        }
        let phone = token.phone.clone();
        let game = token.game.clone();
        let mut token = token;
        match self
            .hypergryph_accounts
            .iter_mut()
            .find(|t| t.phone == phone && t.game == game)
        {
            Some(existing) => {
                token.alias = std::mem::take(&mut existing.alias);
                token.remark = std::mem::take(&mut existing.remark);
                *existing = token;
            }
            None => self.hypergryph_accounts.push(token),
        }
        self.active_hypergryph.insert(game, phone);
    }

    /// 删除某游戏的鹰角账号。
    pub fn remove_hypergryph(&mut self, game: &str, phone: &str) {
        self.hypergryph_accounts
            .retain(|t| !(t.phone == phone && t.game == game));
        if self.active_hypergryph.get(game).map(|s| s.as_str()) == Some(phone) {
            self.active_hypergryph.remove(game);
        }
    }

    /// 新增或更新 B 服账号并设为激活（登录成功时调用）。
    /// B 服与官服**各自**保留一个激活账号（游戏登录码无法从 URL 区分平台，
    /// 确认阶段按已登录账号逐个尝试），故此处**不清除**官服的激活账号。
    pub fn upsert_bili(&mut self, token: BiliToken) {
        if token.uid.is_empty() {
            return;
        }
        let uid = token.uid.clone();
        let game = token.game.clone();
        let mut token = token;
        match self
            .bili_accounts
            .iter_mut()
            .find(|t| t.uid == uid && t.game == game)
        {
            // 更新已存在账号时保留用户备注（登录响应不含 alias/remark）。
            Some(existing) => {
                token.alias = std::mem::take(&mut existing.alias);
                token.remark = std::mem::take(&mut existing.remark);
                *existing = token;
            }
            None => self.bili_accounts.push(token),
        }
        self.active_bili.insert(game.clone(), uid.clone());
        self.game_channel
            .insert(game.clone(), Some(Channel::Bilibili));
        self.touch(&game, Channel::Bilibili, &uid);
    }

    /// 新增或更新官服账号并设为激活（登录成功时调用）。
    /// 同样**不清除** B 服的激活账号（见 `upsert_bili`）。
    pub fn upsert_official(&mut self, token: OfficialToken) {
        if token.uid.is_empty() {
            return;
        }
        let uid = token.uid.clone();
        let game = token.game.clone();
        let mut token = token;
        match self
            .official_accounts
            .iter_mut()
            .find(|t| t.uid == uid && t.game == game)
        {
            // 更新已存在账号时保留用户备注（登录响应不含 alias/remark）。
            Some(existing) => {
                token.alias = std::mem::take(&mut existing.alias);
                token.remark = std::mem::take(&mut existing.remark);
                *existing = token;
            }
            None => self.official_accounts.push(token),
        }
        self.active_official.insert(game.clone(), uid.clone());
        self.game_channel
            .insert(game.clone(), Some(Channel::Official));
        self.touch(&game, Channel::Official, &uid);
    }

    /// 设为某游戏当前使用的账号：更新该游戏的 active_uid 与渠道。
    pub fn set_current(&mut self, game: &str, channel: Channel, uid: &str) -> bool {
        let ok = match channel {
            Channel::Bilibili => {
                if self
                    .bili_accounts
                    .iter()
                    .any(|t| t.uid == uid && t.game == game)
                {
                    self.active_bili.insert(game.to_string(), uid.to_string());
                    true
                } else {
                    false
                }
            }
            Channel::Official => {
                if self
                    .official_accounts
                    .iter()
                    .any(|t| t.uid == uid && t.game == game)
                {
                    self.active_official
                        .insert(game.to_string(), uid.to_string());
                    true
                } else {
                    false
                }
            }
        };
        if ok {
            self.game_channel.insert(game.to_string(), Some(channel));
            self.touch(game, channel, uid);
        }
        ok
    }

    /// 记录某游戏某账号的最近使用时间（Unix 秒）。
    fn touch(&mut self, game: &str, channel: Channel, uid: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        match channel {
            Channel::Bilibili => {
                if let Some(t) = self
                    .bili_accounts
                    .iter_mut()
                    .find(|t| t.uid == uid && t.game == game)
                {
                    t.last_used_at = now;
                }
            }
            Channel::Official => {
                if let Some(t) = self
                    .official_accounts
                    .iter_mut()
                    .find(|t| t.uid == uid && t.game == game)
                {
                    t.last_used_at = now;
                }
            }
        }
    }

    /// 游戏内显示名：配置非空则用之，否则默认值。
    pub fn effective_display_name(&self) -> String {
        let n = self.display_name.trim();
        if n.is_empty() {
            "游戏扫码器用户".to_string()
        } else {
            n.to_string()
        }
    }

    /// 某游戏当前渠道激活的账号标识（供前端显示「将用哪个账号」）。
    pub fn current_uid(&self, game: &str) -> Option<&str> {
        match self.game_channel.get(game).copied().flatten()? {
            Channel::Bilibili => self.active_bili.get(game).map(|s| s.as_str()),
            Channel::Official => self.active_official.get(game).map(|s| s.as_str()),
        }
    }

    /// 删除某游戏的 B 服账号；若删除的是激活账号则清空激活。
    pub fn remove_bili(&mut self, game: &str, uid: &str) {
        self.bili_accounts
            .retain(|t| !(t.uid == uid && t.game == game));
        if self.active_bili.get(game).map(|s| s.as_str()) == Some(uid) {
            self.active_bili.remove(game);
            // 当前渠道是 B 服但已无账号 → 回落到官服（若有）。
            if self.game_channel.get(game).copied().flatten() == Some(Channel::Bilibili)
                && !self.active_bili.contains_key(game)
            {
                let fallback = if self.active_official.contains_key(game) {
                    Some(Channel::Official)
                } else {
                    None
                };
                self.game_channel.insert(game.to_string(), fallback);
            }
        }
    }

    /// 删除某游戏的官服账号；若删除的是激活账号则清空激活。
    pub fn remove_official(&mut self, game: &str, uid: &str) {
        self.official_accounts
            .retain(|t| !(t.uid == uid && t.game == game));
        if self.active_official.get(game).map(|s| s.as_str()) == Some(uid) {
            self.active_official.remove(game);
            if self.game_channel.get(game).copied().flatten() == Some(Channel::Official)
                && !self.active_official.contains_key(game)
            {
                let fallback = if self.active_bili.contains_key(game) {
                    Some(Channel::Bilibili)
                } else {
                    None
                };
                self.game_channel.insert(game.to_string(), fallback);
            }
        }
    }

    /// 配置目录名。
    const DIR: &'static str = "游戏扫码器";

    /// 配置文件路径：%APPDATA%\<AppName>\config.json
    ///
    /// 直接取 `%APPDATA%`（= `dirs::config_dir()` 在 Windows 上的取值）：本工具
    /// 仅支持 Windows，去掉 `dirs` 依赖（省一个 crate 的体积与依赖面）。
    pub fn path() -> PathBuf {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        base.join(Self::DIR).join("config.json")
    }

    /// 从磁盘加载；损坏/无效时备份并回退默认值。
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<AppConfig>(&text) {
                Ok(mut cfg) => {
                    cfg.decrypt_secrets();
                    cfg.display_fps = normalize_display_fps(cfg.display_fps);
                    cfg
                }
                Err(_) => {
                    Self::backup_corrupt(&path);
                    AppConfig::default()
                }
            },
            Err(_) => AppConfig::default(),
        }
    }

    /// 记录直播间使用历史。
    ///
    /// **顺序语义（重要）**：左侧列表顺序只在用户拖拽（`reorder_live_history`）时变化。
    /// 因此本方法对**已存在**条目只更新字段（备注/元数据缓存/使用时间），保持其在列表
    /// 中的既有位置；**新**条目追加到末尾。此前实现在每次连接时把条目「去重置顶」
    /// （insert(0)），等效于按最近使用自动排序——用户拖拽排好的顺序在重启后会被
    /// 打乱，已按「固定顺序持久化」的要求修正。
    ///
    /// 超上限（`LIVE_HISTORY_MAX`）时淘汰**最久未使用**的一条（而非截断末尾——
    /// 追加语义下末尾是最新添加的房间）。
    /// `platform` 为 "bilibili" / "douyin"，兼容旧值 "bili"。
    pub fn push_live_history(&mut self, room: &str, platform: &str, note: &str) {
        let room = room.trim();
        if room.is_empty() {
            return;
        }
        let platform = if platform == "douyin" {
            "douyin"
        } else {
            "bilibili"
        };
        let note = note.trim();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        // 已存在：原位更新（备注留空则保留旧备注），位置不变。
        if let Some(e) = self
            .live_history
            .iter_mut()
            .find(|e| e.room == room && e.platform == platform)
        {
            if !note.is_empty() {
                e.note = note.to_string();
            }
            e.last_used = now;
            return;
        }
        // 新条目：追加到末尾（既有条目顺序保持不变）。
        self.live_history.push(LiveHistoryEntry {
            room: room.to_string(),
            platform: platform.to_string(),
            note: note.to_string(),
            last_used: now,
            title: String::new(),
            nickname: String::new(),
            cover: String::new(),
            room_id: String::new(),
            live_status: 0,
            live_start: 0,
            last_checked: 0,
        });
        // 超上限：淘汰最久未使用的一条（同 last_used 时取列表中最靠前者）。
        if self.live_history.len() > LIVE_HISTORY_MAX {
            if let Some(idx) = self
                .live_history
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(i, _)| i)
            {
                self.live_history.remove(idx);
            }
        }
    }

    /// 按给定 "platform:room" 顺序重排 live_history（拖拽排序持久化）。
    pub fn reorder_live_history(&mut self, order: &[String]) {
        let mut reordered: Vec<LiveHistoryEntry> = Vec::with_capacity(self.live_history.len());
        for id in order {
            if let Some(pos) = self
                .live_history
                .iter()
                .position(|e| format!("{}:{}", e.platform, e.room) == *id)
            {
                reordered.push(self.live_history.remove(pos));
            }
        }
        // 追加未在 order 中的剩余条目（保持原相对顺序）。
        reordered.append(&mut self.live_history);
        self.live_history = reordered;
    }

    /// 设置某直播间的备注（按 platform+room 精确匹配）。
    pub fn set_live_note(&mut self, room: &str, platform: &str, note: &str) {
        for e in self.live_history.iter_mut() {
            if e.room == room && e.platform == platform {
                e.note = note.to_string();
            }
        }
    }

    /// 更新某直播间的元数据缓存（按 (platform, room) 匹配）。
    #[allow(clippy::too_many_arguments)]
    pub fn update_live_meta(
        &mut self,
        room: &str,
        platform: &str,
        title: &str,
        nickname: &str,
        cover: &str,
        room_id: &str,
        live_status: i64,
        live_start: i64,
    ) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        for e in self.live_history.iter_mut() {
            if e.room == room && e.platform == platform {
                if !title.is_empty() {
                    e.title = title.to_string();
                }
                if !nickname.is_empty() {
                    e.nickname = nickname.to_string();
                }
                if !cover.is_empty() {
                    e.cover = cover.to_string();
                }
                if !room_id.is_empty() {
                    e.room_id = room_id.to_string();
                }
                e.live_status = live_status;
                e.live_start = live_start;
                e.last_checked = now;
            }
        }
    }

    /// 删除指定直播间的历史条目（按 platform+room 精确删除）。返回是否存在过。
    pub fn remove_live_history(&mut self, room: &str, platform: &str) -> bool {
        let before = self.live_history.len();
        self.live_history
            .retain(|e| !(e.room == room && e.platform == platform));
        self.live_history.len() != before
    }

    /// 保存到磁盘（原子写：先写临时文件再 rename，避免写一半崩溃损坏配置）。
    /// Windows 上 rename 通过 MoveFileEx(REPLACE_EXISTING) 实现，可安全覆盖目标。
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // 敏感字段（access_key / stoken）落盘前用 DPAPI 加密；内存中始终明文。
        let mut to_write = self.clone();
        to_write.encrypt_secrets();
        let text = serde_json::to_string_pretty(&to_write).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)
    }

    /// 加密敏感字段（access_key / stoken）。仅用于落盘，内存中保持明文。
    pub fn encrypt_secrets(&mut self) {
        for a in self.bili_accounts.iter_mut() {
            a.access_key = crypto::protect_secret(&a.access_key);
        }
        for a in self.official_accounts.iter_mut() {
            a.stoken = crypto::protect_secret(&a.stoken);
        }
    }

    /// 解密敏感字段。旧版明文（无 `dpapi:` 前缀）原样保留，向后兼容。
    /// 解密失败时置空（避免把密文当 token 使用），用户需重新登录该账号。
    pub fn decrypt_secrets(&mut self) {
        for a in self.bili_accounts.iter_mut() {
            a.access_key = crypto::unprotect_secret(&a.access_key);
        }
        for a in self.official_accounts.iter_mut() {
            a.stoken = crypto::unprotect_secret(&a.stoken);
        }
    }

    /// 备份损坏配置文件（追加时间戳），不崩溃。
    fn backup_corrupt(path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup = path.with_extension(format!("json.corrupt.{stamp}"));
        let _ = std::fs::rename(path, backup);
    }
}
