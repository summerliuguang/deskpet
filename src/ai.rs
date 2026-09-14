//! AI 对话客户端：OpenAI 兼容 /chat/completions。
//!
//! 专为自签证书的局域网网关（LiteGate）设计：跳过证书校验；
//! 阻塞式 ureq 调用放在独立线程，UI 不卡顿。

use crate::config::Config;
use serde_json::{json, Value};
use std::io::Read;
use std::sync::Arc;

#[derive(Debug)]
struct NoVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
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

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
}

impl Client {
    pub fn new() -> Self {
        // ureq 启用 rustls/ring、我们的直连依赖又带默认 aws-lc-rs：两个提供者并存时
        // rustls 拒绝自动选择（启动即 panic），这里显式指定用 ring
        let _ = rustls::crypto::ring::default_provider().install_default();
        let verifier = std::sync::Arc::new(NoVerifier);
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
    pub fn chat(&self, cfg: &Config, model: &str, history: &[Value]) -> Result<String, String> {
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
            .map_err(|e| {
                if let ureq::Error::Status(code, r) = e {
                    let text = r.into_string().unwrap_or_default();
                    let msg = serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|v| {
                            v.pointer("/error/message").and_then(|m| m.as_str()).map(String::from)
                        })
                        .unwrap_or_else(|| format!("HTTP {code}"));
                    format!("HTTP {code}: {msg}")
                } else {
                    format!("网络错误：{e}")
                }
            })?;
        let v: Value = serde_json::from_str(&resp.into_string().map_err(|e| e.to_string())?)
            .map_err(|e| format!("响应解析失败：{e}"))?;
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
            None => Err("模型没有返回正文（可能被思维链耗尽 token）".into()),
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
}
