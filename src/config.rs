//! 配置读取：deskpet.toml（与 exe 同目录或工作目录）。
//! API Key 只从配置文件/环境变量读取，绝不写进代码。
//!
//! Config = AI 接入参数；Settings = 行为开关（可在菜单"设置"页实时切换并持久化）。

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// OpenAI 兼容入口，含 /v1（自建网关或任意 OpenAI 兼容服务）
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 识别图片用的模型；缺省用 deepseek-flash（免费模型不支持图片输入）
    pub vision_model: Option<String>,
    pub system_prompt: String,
    pub pet_name: String,
    /// TTS 音色（LiteGate MiMo：mimo_default/冰糖/茉莉/苏打/白桦…）
    pub tts_voice: String,
}

impl Config {
    pub fn ai_ready(&self) -> bool {
        !self.api_key.is_empty()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: "https://your-gateway.example/v1".into(),
            api_key: String::new(),
            model: "nvidia/nemotron-3-super-120b-a12b:free".into(),
            vision_model: Some("deepseek-flash".into()),
            system_prompt: "你是趴在主人 Windows 桌面上的小猫，名字叫团子。用中文回复，简短可爱，不超过 60 字，可以偶尔用颜文字。".into(),
            pet_name: "团子".into(),
            tts_voice: "mimo_default".into(),
        }
    }
}

/// 行为开关清单——**新增字段只改这一处**：Default/parse/persist_toggles
/// 三处代码由宏自动生成（此前要手改三处，漏一处即静默丢配置）。
/// 类型分支：bool / u64 / opt_i64 / opt_usize；第 3 列是默认值（opt 用 none）。
#[macro_export]
macro_rules! for_each_setting {
    (($($args:tt)*) => $m:ident) => {
        $m!($($args)*, {
            (voice, bool, true)
            (pos_x, opt_i64, none)
            (pos_y, opt_i64, none)
            (model_kind, opt_usize, none)
            (model_name, opt_string, none)
            (drink_minutes, u64, 45)
            (sit_minutes, u64, 90)
            (keyboard_link, bool, true)
            (gamepad_link, bool, true)
            (gaze_follow, bool, true)
            (follow_mouse, bool, false)
            (whisper_on, bool, true)
            (quiet, bool, false)
            (onboarded, bool, false)
            (hotkeys, bool, true)
            (typewriter, bool, true)
            (chat_log, bool, true)
        })
    };
    ($m:ident) => {
        $m!( {
            (voice, bool, true)
            (pos_x, opt_i64, none)
            (pos_y, opt_i64, none)
            (model_kind, opt_usize, none)
            (model_name, opt_string, none)
            (drink_minutes, u64, 45)
            (sit_minutes, u64, 90)
            (keyboard_link, bool, true)
            (gamepad_link, bool, true)
            (gaze_follow, bool, true)
            (follow_mouse, bool, false)
            (whisper_on, bool, true)
            (quiet, bool, false)
            (onboarded, bool, false)
            (hotkeys, bool, true)
            (typewriter, bool, true)
            (chat_log, bool, true)
        })
    };
}

macro_rules! setting_default {
    (bool, $d:tt) => { $d };
    (u64, $d:tt) => { $d };
    (opt_i64, none) => { None };
    (opt_usize, none) => { None };
    (opt_string, none) => { None };
}

macro_rules! default_fields {
    ({ $(($name:ident, $ty:ident, $d:tt))* }) => {
        Self { $( $name: setting_default!($ty, $d) ),* }
    };
}

macro_rules! parse_setting_value {
    ($v:ident, $name:ident, bool, $d:tt) => {
        $v.get(stringify!($name)).and_then(|x| x.as_bool()).unwrap_or($d)
    };
    ($v:ident, $name:ident, u64, $d:tt) => {
        $v.get(stringify!($name)).and_then(|x| x.as_integer()).map(|x| x as u64).unwrap_or($d)
    };
    ($v:ident, $name:ident, opt_i64, none) => {
        $v.get(stringify!($name)).and_then(|x| x.as_integer()).map(|x| x as i64)
    };
    ($v:ident, $name:ident, opt_usize, none) => {
        $v.get(stringify!($name)).and_then(|x| x.as_integer()).map(|x| x as usize)
    };
    ($v:ident, $name:ident, opt_string, none) => {
        $v.get(stringify!($name)).and_then(|x| x.as_str()).map(|x| x.to_string())
    };
}

macro_rules! parse_fields {
    ($v:ident, { $(($name:ident, $ty:ident, $d:tt))* }) => {
        Self { $( $name: parse_setting_value!($v, $name, $ty, $d) ),* }
    };
}

/// 行为开关（运行时可变，菜单"设置"页持久化）
#[derive(Debug, Clone)]
pub struct Settings {
    pub voice: bool,
    /// 上次退出时的宠物位置（恢复用）
    pub pos_x: Option<i64>,
    pub pos_y: Option<i64>,
    /// 上次选中的模型（注册表下标；旧版遗留，新版本以 model_name 为准）
    pub model_kind: Option<usize>,
    /// 上次选中的模型名（增删模型后下标会漂移，名字不会）
    pub model_name: Option<String>,
    pub drink_minutes: u64,
    pub sit_minutes: u64,
    pub keyboard_link: bool,
    pub gamepad_link: bool,
    pub gaze_follow: bool,
    pub follow_mouse: bool,
    pub whisper_on: bool,
    /// 勿扰模式：停碎碎念/提醒/语音，互动仍可用
    pub quiet: bool,
    /// 首次引导已看过（只提示一次）
    pub onboarded: bool,
    /// 全局快捷键（Ctrl+Shift+D/T/C/H/Q）
    pub hotkeys: bool,
    /// 气泡逐字打字机效果
    pub typewriter: bool,
    /// 聊天记录落盘 chat_log.jsonl（含对话内容，隐私敏感，可关）
    pub chat_log: bool,
}

impl Default for Settings {
    fn default() -> Self {
        for_each_setting!(default_fields)
    }
}

impl Settings {
    pub fn parse(text: &str) -> Self {
        let Ok(v) = text.parse::<toml::Value>() else {
            return Self::default();
        };
        for_each_setting!((v) => parse_fields)
    }
}

/// 首次运行自动生成的配置模板：**全行注释 = 全部使用默认值（离线模式）**。
/// 想启用 AI 聊天/语音，取消 base_url/api_key 注释填入网关与密钥即可；
/// 之后在菜单里切换开关会经 persist_settings 重写本文件（注释会丢，已填的值保留）。
pub const DEFAULT_TEMPLATE: &str = r#"# deskpet 配置（首次运行自动生成；本文件所有行都是注释 = 全部使用默认值）
#
# ---- 启用 AI 聊天 / 语音（当前未启用）----
# 取消下面两行注释，填入你的 OpenAI 兼容网关地址与密钥，然后重启桌宠：
# base_url = "https://your-gateway.example/v1"
# api_key = "在这里填你的密钥"
# 聊天模型（默认走免费模型）；拖图识别用的视觉模型（免费模型不支持图片）
# model = "nvidia/nemotron-3-super-120b-a12b:free"
# vision_model = "deepseek-flash"
# 猫的名字与性格提示词
# pet_name = "团子"
# system_prompt = "你是趴在主人 Windows 桌面上的小猫，名字叫团子。用中文回复，简短可爱，不超过 60 字，可以偶尔用颜文字。"
# 语音音色（mimo_default/冰糖/茉莉/苏打/白桦…）
# tts_voice = "mimo_default"
#
# ---- 行为开关（右键菜单可实时切换并自动写回本文件）----
# voice = true          # 语音播报总开关
# quiet = false         # 勿扰模式（不主动说话不出声）
# typewriter = true     # 打字机气泡
# chat_log = true       # 聊天记录落盘 chat_log.jsonl（隐私敏感可关）
# hotkeys = true        # 全局快捷键 Ctrl+Shift+D勿扰/T待办/C聊天/H隐藏/Q退出
# drink_minutes = 45    # 喝水提醒间隔（0=关）
# sit_minutes = 90      # 久坐提醒间隔（0=关）
"#;

/// 若目标路径不存在则写入模板；已存在（含空文件）绝不覆盖。
/// 返回是否实际写入了文件。
pub fn ensure_default_template_at(path: &Path) -> bool {
    if path.exists() {
        return false;
    }
    std::fs::write(path, DEFAULT_TEMPLATE).is_ok()
}

/// 首次运行时在 exe 旁生成配置模板（不影响本次启动的默认值解析）
pub fn ensure_default_template() -> bool {
    match candidate_paths().first().cloned() {
        Some(path) => ensure_default_template_at(&path),
        None => false,
    }
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("deskpet.toml"));
        }
    }
    v.push(PathBuf::from("deskpet.toml"));
    v
}

pub fn load_text() -> Option<String> {
    candidate_paths()
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
}

pub fn load() -> Option<Config> {
    load_text().map(|text| parse_config(&text))
}

/// 解析 AI 配置（解析失败/缺字段回退默认值；默认值只在 Config::default() 一处）
pub fn parse_config(text: &str) -> Config {
    let Ok(v) = text.parse::<toml::Value>() else {
        return Config::default();
    };
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
    let mut cfg = Config {
        api_key: s("api_key")
            .filter(|k| !k.is_empty())
            .or_else(|| std::env::var("DESKPET_API_KEY").ok())
            .unwrap_or_default(),
        ..Config::default()
    };
    if let Some(bu) = s("base_url").filter(|x| !x.is_empty()) {
        cfg.base_url = bu;
    }
    if let Some(m) = s("model").filter(|x| !x.is_empty()) {
        cfg.model = m;
    }
    if let Some(vm) = s("vision_model").filter(|x| !x.is_empty()) {
        cfg.vision_model = Some(vm);
    }
    if let Some(sp) = s("system_prompt").filter(|x| !x.trim().is_empty()) {
        cfg.system_prompt = sp;
    }
    if let Some(name) = s("pet_name").filter(|x| !x.trim().is_empty()) {
        cfg.pet_name = name;
    }
    if let Some(voice) = s("tts_voice").filter(|x| !x.is_empty()) {
        cfg.tts_voice = voice;
    }
    cfg
}

/// 解析行为开关
pub fn parse_settings(text: &str) -> Settings {
    Settings::parse(text)
}

/// 把行为开关合并写回 deskpet.toml（保留其他字段，如 api_key）。
/// 原子写：先写临时文件再改名，进程中途被杀不会留下半截配置。
pub fn persist_settings(pairs: &[(String, toml::Value)]) {
    let target = candidate_paths().first().cloned();
    let Some(path) = target else { return };
    let existing = std::fs::read_to_string(&path).ok();
    let mut table = existing
        .as_deref()
        .and_then(|t| t.parse::<toml::Value>().ok())
        .and_then(|v| v.as_table().cloned())
        .unwrap_or_default();
    for (k, val) in pairs {
        table.insert(k.clone(), val.clone());
    }
    if let Ok(out) = toml::to_string(&table) {
        let header = "# deskpet 配置（由菜单-设置页与手写配置共用）\n";
        let tmp = path.with_extension("toml.tmp");
        let wrote = std::fs::write(&tmp, format!("{header}\n{out}"))
            .and_then(|_| std::fs::rename(&tmp, &path));
        if wrote.is_err() {
            // 磁盘满/只读/权限：开关状态静默丢失会很迷惑，记进日志
            crate::dwarn("persist-config", "deskpet.toml 写入失败（磁盘满或不可写？）");
        }
    } else {
        crate::dwarn("persist-config", "deskpet.toml 序列化失败");
    }
}

/// 配置诊断：deskpet.toml 存在但解析失败时返回提示
/// （字段缺省静默回退默认值没问题；整个文件坏掉用户该知道）。
pub fn config_diag(text: Option<&str>) -> Option<String> {
    let text = text?;
    if text.parse::<toml::Value>().is_err() {
        return Some("deskpet.toml 格式有误，本次启动按默认配置运行".into());
    }
    // 单字段校验：base_url 必须是 http(s) URL，写错时聊天必然失败
    if let Ok(v) = text.parse::<toml::Value>() {
        if let Some(bu) = v.get("base_url").and_then(|x| x.as_str()) {
            if !bu.is_empty() && !bu.starts_with("http://") && !bu.starts_with("https://") {
                return Some("deskpet.toml 的 base_url 应以 http:// 或 https:// 开头".into());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_defaults_without_fields() {
        let c = parse_config("");
        assert_eq!(c.model, "nvidia/nemotron-3-super-120b-a12b:free");
        assert_eq!(c.pet_name, "团子");
        assert!(!c.ai_ready(), "没有 api_key 时应为离线模式");
    }

    #[test]
    fn parse_config_overrides_fields() {
        let c = parse_config("model = \"my-model\"\napi_key = \"sk-lg-test\"\npet_name = \"橘子\"");
        assert_eq!(c.model, "my-model");
        assert_eq!(c.api_key, "sk-lg-test");
        assert_eq!(c.pet_name, "橘子");
        assert!(c.ai_ready());
    }

    /// 回归：base_url 空值回退默认、非空覆盖默认（曾在此处遗漏覆盖分支）
    #[test]
    fn parse_config_base_url_fallback_and_override() {
        assert_eq!(parse_config("").base_url, Config::default().base_url);
        let c = parse_config("base_url = \"https://example.invalid/v1\"");
        assert_eq!(c.base_url, "https://example.invalid/v1");
    }

    /// vision_model 空值与其他字段一致：回退默认（免费模型无视觉，默认走付费视觉模型）
    #[test]
    fn parse_config_empty_vision_model_falls_back() {
        assert_eq!(parse_config("").vision_model, Some("deepseek-flash".into()));
        assert_eq!(parse_config("vision_model = \"\"").vision_model, Some("deepseek-flash".into()));
    }

    /// 回归：自动生成的模板全行注释，解析必须等于全默认（离线模式）——
    /// 生成模板这个动作不能改变任何运行行为
    #[test]
    fn default_template_parses_to_offline_defaults() {
        assert_eq!(parse_config(DEFAULT_TEMPLATE), Config::default());
        let st = parse_settings(DEFAULT_TEMPLATE);
        let d = Settings::default();
        assert_eq!(st.voice, d.voice);
        assert_eq!(st.quiet, d.quiet);
        assert_eq!(st.hotkeys, d.hotkeys);
        assert_eq!(st.typewriter, d.typewriter);
        assert_eq!(st.chat_log, d.chat_log);
        assert_eq!(st.drink_minutes, d.drink_minutes);
        assert_eq!(st.sit_minutes, d.sit_minutes);
    }

    /// ensure_default_template_at：不存在才写，已存在绝不覆盖
    #[test]
    fn template_only_created_when_missing() {
        let dir = std::env::temp_dir().join(format!("deskpet_tpl_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("deskpet.toml");
        assert!(ensure_default_template_at(&path), "首次应写入");
        assert!(path.exists());
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(!ensure_default_template_at(&path), "已存在不得覆盖");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        // 用户手填的配置也不会被覆盖
        std::fs::write(&path, "api_key = \"x\"").unwrap();
        assert!(!ensure_default_template_at(&path));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "api_key = \"x\"");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_diag_flags_broken_toml_and_bad_url() {
        assert_eq!(config_diag(None), None, "无配置文件不提示");
        assert_eq!(config_diag(Some("api_key = \"x\"\n")), None, "合法配置不提示");
        assert!(config_diag(Some("这不是 toml {{{")).is_some(), "解析失败要提示");
        assert!(
            config_diag(Some("base_url = \"your-gateway.example/v1\"")).is_some(),
            "base_url 缺协议要提示"
        );
        assert_eq!(config_diag(Some("base_url = \"\"")), None, "空 base_url 走默认，不提示");
    }

    #[test]
    fn parse_config_broken_toml_falls_back_to_defaults() {
        let c = parse_config("这不是 toml {{{");
        assert_eq!(c.model, "nvidia/nemotron-3-super-120b-a12b:free");
    }

    #[test]
    fn settings_parse_defaults_and_overrides() {
        let d = Settings::parse("");
        assert!(d.voice && d.keyboard_link && d.gaze_follow);
        assert!(!d.follow_mouse);
        assert_eq!(d.drink_minutes, 45);
        let s = Settings::parse("voice = false\nfollow_mouse = true\nsit_minutes = 30");
        assert!(!s.voice);
        assert!(s.follow_mouse);
        assert_eq!(s.sit_minutes, 30);
    }
}
