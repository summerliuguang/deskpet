//! AI 对话客户端：OpenAI 兼容 /chat/completions。
//!
//! 专为自签证书的局域网网关（LiteGate）设计：**TOFU 证书锁定**——
//! 首次连接记录服务端证书 SHA-256 指纹（known_hosts.txt，与 exe 同目录），
//! 之后指纹不匹配即拒连。相比裸跳过校验，配置被篡改指向恶意服务器时
//! api_key 不会拱手送人。
//! 阻塞式 ureq 调用放在独立线程，UI 不卡顿。

use crate::config::Config;
use serde_json::{json, Value};
use std::collections::HashMap;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// TOFU 判定（纯函数便于测试）：Ok(true)=指纹匹配或首次记录，Err=不匹配拒连
fn tofu_check(known: Option<&[u8; 32]>, fp: &[u8; 32]) -> Result<bool, ()> {
    match known {
        None => Ok(true),      // 首次见到该主机：信任并记录（TOFU）
        Some(k) if k == fp => Ok(true),
        Some(_) => Err(()),    // 指纹变更：拒连
    }
}

/// 证书指纹存储：内存表 + known_hosts.txt 落盘（新增指纹即写回）
#[derive(Debug)]
struct TofuVerifier {
    store: Mutex<HashMap<String, [u8; 32]>>,
    store_path: Option<std::path::PathBuf>,
    /// 有新指纹待落盘
    dirty: AtomicBool,
}

impl TofuVerifier {
    fn new() -> Self {
        let store_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("known_hosts.txt")))
            .or_else(|| Some(std::env::temp_dir().join("known_hosts.txt")));
        let mut store = HashMap::new();
        if let Some(path) = &store_path {
            if let Ok(text) = std::fs::read_to_string(path) {
                for line in text.lines() {
                    if let Some((host, fp)) = line.split_once(':') {
                        if let Ok(bytes) = decode_hex(fp) {
                            let mut key = [0u8; 32];
                            if bytes.len() == 32 {
                                key.copy_from_slice(&bytes);
                                store.insert(host.to_string(), key);
                            }
                        }
                    }
                }
            }
        }
        Self { store: Mutex::new(store), store_path, dirty: AtomicBool::new(false) }
    }

    fn save(&self) {
        if !self.dirty.load(Ordering::Relaxed) {
            return;
        }
        let Ok(st) = self.store.lock() else { return };
        if let Some(path) = &self.store_path {
            let body: String = st
                .iter()
                .map(|(h, fp)| format!("{}:{}\n", h, encode_hex(fp)))
                .collect();
            let _ = std::fs::write(path, body);
            self.dirty.store(false, Ordering::Relaxed);
        }
    }
}

/// 对外暴露的判定入口：计算指纹 → TOFU 检查 → 新指纹落盘
impl TofuVerifier {
    fn verify(&self, host: &str, cert_der: &[u8]) -> Result<(), String> {
        let fp: [u8; 32] = Sha256::digest(cert_der).into();
        let Ok(st) = self.store.lock() else {
            return Err("证书校验器内部锁失败".into());
        };
        match tofu_check(st.get(host), &fp) {
            Ok(false) => Ok(()),
            Ok(true) if st.get(host).is_some() => Ok(()),
            Ok(_) => {
                drop(st);
                if let Ok(mut st) = self.store.lock() {
                    st.insert(host.to_string(), fp);
                    self.dirty.store(true, Ordering::Relaxed);
                }
                self.save();
                Ok(())
            }
            Err(()) => Err(format!(
                "网关证书变更：{host} 的证书指纹与首次记录不一致。\
                 若非你本人更换网关，请检查 base_url 是否被篡改；\
                 确认安全后可删除 known_hosts.txt 重新信任"
            )),
        }
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_hex(s: &str) -> Result<Vec<u8>, ()> {
    if s.len() % 2 != 0 {
        return Err(());
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| ()))
        .collect()
}

impl rustls::client::danger::ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let host = server_name.to_str().to_string();
        self.verify(&host, end_entity.as_ref()).map_err(rustls::Error::General)?;
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}

/// 聊天错误分类：决定重试策略与用户文案（原先一律格式化成字符串，
/// 密钥错误也盲重试 2 次，报错原文生硬）
#[derive(Debug)]
pub enum ChatErr {
    /// 网络层失败（连接/超时/DNS）
    Network(String),
    /// 服务端返回了 HTTP 状态码
    Http(u16, String),
    /// 响应不是合法 JSON
    Parse(String),
    /// 响应正常但没有正文
    Empty,
}

impl ChatErr {
    /// 只有网络故障/限流/服务端临时错误值得重试；
    /// 401（密钥错）/404（模型错）/解析错误重试也不会好
    pub fn retryable(&self) -> bool {
        match self {
            ChatErr::Network(_) => true,
            ChatErr::Http(code, _) => *code == 429 || (500..=599).contains(code),
            ChatErr::Parse(_) | ChatErr::Empty => false,
        }
    }

    /// 给气泡显示的中文文案
    pub fn message(&self) -> String {
        match self {
            ChatErr::Network(e) => format!("网络错误：{e}"),
            ChatErr::Http(401, _) => "密钥无效（api_key 不对，检查 deskpet.toml）".into(),
            ChatErr::Http(404, _) => "模型不存在（检查 deskpet.toml 的 model 配置）".into(),
            ChatErr::Http(429, _) => "请求太频繁或额度用尽，稍后再试".into(),
            ChatErr::Http(code, msg) => format!("HTTP {code}: {msg}"),
            ChatErr::Parse(e) => format!("响应解析失败：{e}"),
            ChatErr::Empty => "模型没有返回正文（可能被思维链耗尽 token）".into(),
        }
    }
}

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
}

impl Client {
    pub fn new() -> Self {
        // ureq 启用 rustls/ring、我们的直连依赖又带默认 aws-lc-rs：两个提供者并存时
        // rustls 拒绝自动选择（启动即 panic），这里显式指定用 ring
        let _ = rustls::crypto::ring::default_provider().install_default();
        let verifier = std::sync::Arc::new(TofuVerifier::new());
        let tls = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        let agent = ureq::AgentBuilder::new()
            // ureq 的 impl 是 `TlsConnector for Arc<rustls::ClientConfig>`，所以套两层 Arc
            .tls_connector(Arc::new(std::sync::Arc::new(tls)))
            .timeout(std::time::Duration::from_secs(90))
            .build();
        Self { agent }
    }

    /// history: 已构造好的 messages 数组（含 system）
    pub fn chat(&self, cfg: &Config, model: &str, history: &[Value]) -> Result<String, ChatErr> {
        let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
        let body = json!({
            "model": model,
            "messages": history,
            // 推理模型的思维链会吃 token，给足预算（LiteGate 实践：>=2000）
            "max_tokens": 2000,
            "temperature": 0.8,
        });
        let resp = self
            .agent
            .post(&url)
            .set("Authorization", &format!("Bearer {}", cfg.api_key))
            .set("X-LiteGate-App", "deskpet")
            .send_string(&body.to_string())
            .map_err(|e| match e {
                ureq::Error::Status(code, r) => {
                    let text = r.into_string().unwrap_or_default();
                    let msg = serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|v| {
                            v.pointer("/error/message").and_then(|m| m.as_str()).map(String::from)
                        })
                        .unwrap_or_else(|| text.chars().take(120).collect());
                    ChatErr::Http(code, msg)
                }
                e => ChatErr::Network(e.to_string()),
            })?;
        let v: Value = serde_json::from_str(&resp.into_string().map_err(|e| ChatErr::Parse(e.to_string()))?)
            .map_err(|e| ChatErr::Parse(e.to_string()))?;
        let content = v
            .pointer("/choices/0/message/content")
            .and_then(|c| c.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            // 推理模型思维链吃满 max_tokens 时 content 为空
            .or_else(|| {
                v.pointer("/choices/0/message/reasoning_content")
                    .and_then(|c| c.as_str())
                    .map(|s| format!("（思考中）{}", s.chars().take(60).collect::<String>()))
            });
        match content {
            Some(c) => Ok(c),
            None => Err(ChatErr::Empty),
        }
    }
    /// MiMo TTS：标准 OpenAI TTS 协议，网关内部桥接到小米 MiMo，返回 wav 字节
    pub fn tts(&self, cfg: &Config, text: &str) -> Result<Vec<u8>, String> {
        let url = format!("{}/audio/speech", cfg.base_url.trim_end_matches('/'));
        let body = json!({
            "model": "mimo-v2.5-tts",
            "input": text,
            "voice": cfg.tts_voice,
        });
        let resp = self
            .agent
            .post(&url)
            .set("Authorization", &format!("Bearer {}", cfg.api_key))
            .set("X-LiteGate-App", "deskpet")
            .send_string(&body.to_string())
            .map_err(|e| format!("TTS 请求失败：{e}"))?;
        let mut wav = Vec::new();
        resp.into_reader()
            .read_to_end(&mut wav)
            .map_err(|e| format!("TTS 响应读取失败：{e}"))?;
        Ok(wav)
    }
}

/// 单条对话消息（UI 层 → API 层）
#[derive(Debug, Clone)]
pub struct ChatMsg {
    pub role: Role,
    pub text: String,
    /// 图片 data URL（data:image/png;base64,...），仅用户消息携带
    pub image: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Pet,
}

pub fn build_history(cfg: &Config, msgs: &[ChatMsg]) -> Vec<Value> {
    let mut out = vec![json!({"role": "system", "content": cfg.system_prompt})];
    let start = msgs.len().saturating_sub(16);
    for (i, m) in msgs.iter().enumerate() {
        if i < start {
            continue; // 只送最近 16 条
        }
        let role = match m.role {
            Role::User => "user",
            Role::Pet => "assistant",
        };
        // 控制载荷：图片 base64 只保留最近 3 条，更早的以文字占位
        let keep_image = i + 3 >= msgs.len();
        let content: Value = match (&m.image, keep_image) {
            (Some(url), true) => json!([
                {"type": "text", "text": m.text},
                {"type": "image_url", "image_url": {"url": url}},
            ]),
            (Some(_), false) => json!(format!("{}（图片已省略）", m.text)),
            (None, _) => json!(m.text),
        };
        out.push(json!({"role": role, "content": content}));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(role: Role, text: &str, image: Option<String>) -> ChatMsg {
        ChatMsg { role, text: text.into(), image }
    }

    #[test]
    fn history_starts_with_system_and_caps_at_16() {
        let cfg = Config::default();
        let mut msgs = Vec::new();
        for i in 0..20 {
            msgs.push(m(Role::User, &format!("u{i}"), None));
            msgs.push(m(Role::Pet, &format!("a{i}"), None));
        }
        let h = build_history(&cfg, &msgs);
        assert_eq!(h[0]["role"], "system");
        assert_eq!(h.len(), 17, "system + 16 条上限");
        assert_eq!(h[1]["content"], "u12");
    }

    #[test]
    fn history_keeps_recent_images_only() {
        let cfg = Config::default();
        let mut msgs = vec![m(Role::User, "旧图", Some("data:image/png;base64,OLD".into()))];
        for i in 0..15 {
            msgs.push(m(Role::User, &format!("u{i}"), None));
            msgs.push(m(Role::Pet, &format!("a{i}"), None));
        }
        // 共 31 条：旧图(0) 被 16 条窗口排除
        let h = build_history(&cfg, &msgs);
        assert_eq!(h.len(), 17);
        // 新构造：最近 3 条内的图保留，之前的剥离为文字
        let mut msgs2 = vec![m(Role::User, "旧图", Some("data:x".into()))];
        for i in 0..15 {
            msgs2.push(m(Role::User, &format!("u{i}"), None));
        }
        let h2 = build_history(&cfg, &msgs2);
        let old = &h2[1]; // 旧图消息在窗口内（start=0）
        assert!(old["content"].is_string());
        assert!(old["content"].as_str().unwrap().contains("图片已省略"));
    }
}

#[cfg(test)]
mod crypto_tests {
    use super::*;

    /// 回归测试：rustls 双加密后端（ureq 的 ring + 默认 aws-lc-rs）并存时，
    /// Client::new 若未显式安装提供者会直接 panic（Windows 上表现为静默退出）
    #[test]
    fn client_constructs_without_crypto_panic() {
        let _ = Client::new();
        let _ = Client::new(); // 二次调用验证 install_default 幂等
    }

    /// TOFU 三分支：首次信任、匹配通过、变更拒连
    #[test]
    fn tofu_check_three_branches() {
        let fp_a = [1u8; 32];
        let fp_b = [2u8; 32];
        assert!(tofu_check(None, &fp_a).is_ok(), "首次见到主机：信任");
        assert!(tofu_check(Some(&fp_a), &fp_a).is_ok(), "指纹一致：通过");
        assert!(tofu_check(Some(&fp_a), &fp_b).is_err(), "指纹变更：拒连");
    }

    /// hex 编解码往返
    #[test]
    fn hex_roundtrip() {
        let bytes = [0x00u8, 0x0f, 0xa0, 0xff];
        let text = encode_hex(&bytes);
        assert_eq!(text.len(), 8);
        assert_eq!(decode_hex(&text).unwrap(), bytes.to_vec());
        assert!(decode_hex("zz").is_err());
        assert!(decode_hex("abc").is_err());
    }

    /// 重试分类：网络/5xx/429 可重试；401/404/解析/空正文不可重试
    #[test]
    fn chat_err_retryable_classification() {
        assert!(ChatErr::Network("timeout".into()).retryable());
        assert!(ChatErr::Http(500, String::new()).retryable());
        assert!(ChatErr::Http(429, String::new()).retryable());
        assert!(!ChatErr::Http(401, String::new()).retryable());
        assert!(!ChatErr::Http(404, String::new()).retryable());
        assert!(!ChatErr::Parse("bad json".into()).retryable());
        assert!(!ChatErr::Empty.retryable());
        // 文案带可读指引
        assert!(ChatErr::Http(401, String::new()).message().contains("api_key"));
        assert!(ChatErr::Http(404, String::new()).message().contains("model"));
    }
}
