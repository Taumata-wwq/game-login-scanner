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
    /// 所属游戏 id（如 "bh3"）；空表示旧数据（归入崩坏3）。
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
    /// 所属游戏 id（如 "bh3"）；空表示旧数据（归入崩坏3）。
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
    pub interval_ms: u64,
    /// 游戏内显示的账号名（B 服 combo/panda confirm 的 asterisk_name 字段）。
    /// 空则用默认「游戏扫码器用户」。
    #[serde(default)]
    pub display_name: String,
    /// 启动后自动使用上次使用的账号并登录（即自动开始扫描）。
    pub auto_login_last: bool,
    /// 窗口置顶。
    pub always_on_top: bool,
    pub auto_close: bool,
    pub clear_clipboard: bool,
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
    /// 旧版「启动自动扫描」设置（迁移到 auto_login_last 后不再写出）。
    #[serde(skip_serializing)]
    pub auto_start_scan: bool,
    /// 旧版单账号字段：仅用于读取旧配置并迁移，保存时不再写出。
    #[serde(skip_serializing)]
    pub bili: Option<BiliToken>,
    #[serde(skip_serializing)]
    pub official: Option<OfficialToken>,
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
            interval_ms: 150,
            display_name: String::new(),
            auto_login_last: false,
            always_on_top: false,
            auto_close: false,
            clear_clipboard: false,
            win_x: None,
            win_y: None,
            device_id: String::new(),
            device_fp: String::new(),
            fp_seed_id: String::new(),
            fp_seed_time: String::new(),
            auto_start_scan: false,
            bili: None,
            official: None,
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
        self.game_channel.insert(game.clone(), Some(Channel::Bilibili));
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
        self.game_channel.insert(game.clone(), Some(Channel::Official));
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

    /// 把旧版单账号字段迁移进账号列表（读取时调用一次）。
    pub fn migrate_legacy(&mut self) {
        // 旧版「启动自动扫描」→ 新版「启动自动登录上次账号」
        if self.auto_start_scan {
            self.auto_login_last = true;
            self.auto_start_scan = false;
        }
        let mut legacy_bili = self.bili.take();
        if let Some(b) = legacy_bili.take() {
            if !b.uid.is_empty() && !b.access_key.is_empty() {
                let mut b = b;
                if b.game.is_empty() {
                    b.game = "bh3".to_string();
                }
                let g = b.game.clone();
                if !self.bili_accounts.iter().any(|t| t.uid == b.uid && t.game == g) {
                    self.active_bili.insert(g.clone(), b.uid.clone());
                    self.game_channel
                        .entry(g)
                        .or_insert(Some(Channel::Bilibili));
                    self.bili_accounts.push(b);
                }
            }
        }
        let mut legacy_official = self.official.take();
        if let Some(o) = legacy_official.take() {
            if !o.uid.is_empty() && !o.stoken.is_empty() {
                let mut o = o;
                if o.game.is_empty() {
                    o.game = "bh3".to_string();
                }
                let g = o.game.clone();
                if !self.official_accounts.iter().any(|t| t.uid == o.uid && t.game == g) {
                    self.active_official.insert(g.clone(), o.uid.clone());
                    self.game_channel
                        .entry(g)
                        .or_insert(Some(Channel::Official));
                    self.official_accounts.push(o);
                }
            }
        }
    }

    /// 配置文件路径：%APPDATA%\<AppName>\config.json
    pub fn path() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        base.join("崩坏3扫码器").join("config.json")
    }

    /// 从磁盘加载；损坏/无效时备份并回退默认值。
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<AppConfig>(&text) {
                Ok(mut cfg) => {
                    cfg.migrate_legacy();
                    cfg.decrypt_secrets();
                    cfg.sanitize();
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

    /// 把非法字段收敛到合理范围。
    pub fn sanitize(&mut self) {
        if self.interval_ms < 50 {
            self.interval_ms = 150;
        }
        if self.interval_ms > 5000 {
            self.interval_ms = 5000;
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
