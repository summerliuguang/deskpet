//! 配置读取：deskpet.toml（与 exe 同目录或工作目录）。
//! API Key 只从配置文件/环境变量读取，绝不写进代码。
//!
//! Config = AI 接入参数；Settings = 行为开关（可在菜单"设置"页实时切换并持久化）。

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    /// OpenAI 兼容入口，含 /v1，例如 https://your-gateway.example/v1
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

/// 行为开关（运行时可变，菜单"设置"页持久化）
#[derive(Debug, Clone)]
pub struct Settings {
    pub voice: bool,
    pub drink_minutes: u64,
    pub sit_minutes: u64,
    pub keyboard_link: bool,
    pub gamepad_link: bool,
    pub gaze_follow: bool,
    pub follow_mouse: bool,
    pub whisper_on: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            voice: true,
            drink_minutes: 45,
            sit_minutes: 90,
            keyboard_link: true,
            gamepad_link: true,
            gaze_follow: true,
            follow_mouse: false,
            whisper_on: true,
        }
    }
}

impl Settings {
    pub fn parse(text: &str) -> Self {
        let Ok(v) = text.parse::<toml::Value>() else {
            return Self::default();
        };
        let b = |k: &str, d: bool| v.get(k).and_then(|x| x.as_bool()).unwrap_or(d);
        let u = |k: &str, d: u64| v.get(k).and_then(|x| x.as_integer()).map(|x| x as u64).unwrap_or(d);
        Self {
            voice: b("voice", true),
            drink_minutes: u("drink_minutes", 45),
            sit_minutes: u("sit_minutes", 90),
            keyboard_link: b("keyboard_link", true),
            gamepad_link: b("gamepad_link", true),
            gaze_follow: b("gaze_follow", true),
            follow_mouse: b("follow_mouse", false),
            whisper_on: b("whisper_on", true),
        }
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

/// 解析 AI 配置（解析失败回退全部默认值）
pub fn parse_config(text: &str) -> Config {
    let v = match text.parse::<toml::Value>() {
        Ok(v) => v,
        Err(_) => return Config::default(),
    };
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
    let mut cfg = Config {
        base_url: s("base_url").unwrap_or_else(|| "https://your-gateway.example/v1".into()),
        api_key: s("api_key")
            .filter(|k| !k.is_empty())
            .or_else(|| std::env::var("DESKPET_API_KEY").ok())
            .unwrap_or_default(),
        // 默认聊天走网关上的免费模型（OpenRouter/NIM :free）
        model: s("model")
            .filter(|x| !x.is_empty())
            .unwrap_or_else(|| "nvidia/nemotron-3-super-120b-a12b:free".into()),
        vision_model: s("vision_model")
            .filter(|x| !x.is_empty())
            // 免费模型不支持图片输入，视觉默认走便宜的付费模型（仅拖图时触发）
            .or_else(|| Some("deepseek-flash".into())),
        system_prompt: s("system_prompt").unwrap_or_else(|| {
            "你是趴在主人 Windows 桌面上的小猫，名字叫团子。用中文回复，简短可爱，\
             不超过 60 字，可以偶尔用颜文字。"
                .into()
        }),
        pet_name: {
            let name = s("pet_name").unwrap_or_else(|| "团子".into());
            if name.trim().is_empty() { "团子".into() } else { name }
        },
        tts_voice: s("tts_voice").unwrap_or_else(|| "mimo_default".into()),
    };
    if cfg.system_prompt.trim().is_empty() {
        cfg.system_prompt = Config::default().system_prompt;
    }
    cfg
}

/// 解析行为开关
pub fn parse_settings(text: &str) -> Settings {
    Settings::parse(text)
}

/// 把行为开关合并写回 deskpet.toml（保留其他字段，如 api_key）
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
        let _ = std::fs::write(&path, format!("{header}\n{out}"));
    }
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
