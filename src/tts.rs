//! TTS 播放：合成走 LiteGate MiMo（ai::Client::tts），播放走 Win32 PlaySound。
//! 合成在独立线程，PlaySound SND_ASYNC 不阻塞事件循环。

use std::sync::atomic::{AtomicU64, Ordering};

/// 上次合成开始时间（毫秒时间戳）：1.5 秒内不重复合成。
/// 顺带限制 wav 缓冲泄漏速率（SND_ASYNC 的缓冲必须存活到播完，无法安全回收）。
#[cfg(windows)]
static LAST_START_MS: AtomicU64 = AtomicU64::new(0);

#[cfg(windows)]
/// enabled = 运行时开关（右键菜单"语音：开/关"）；cfg.voice 只作为启动初值
#[cfg(windows)]
pub fn speak(client: &crate::ai::Client, cfg: &crate::config::Config, enabled: bool, text: &str) {
    if !enabled || text.is_empty() {
        return;
    }
    let now = crate::now_ms();
    if now.saturating_sub(LAST_START_MS.load(Ordering::Relaxed)) < 1500 {
        return;
    }
    LAST_START_MS.store(now, Ordering::Relaxed);

    let text: String = text.chars().take(120).collect();
    let client = client.clone();
    let cfg = cfg.clone();
    std::thread::spawn(move || match client.tts(&cfg, &text) {
        Ok(wav) => play_wav(&wav),
        Err(_) => {} // TTS 失败静默，不影响文字回复
    });
}

#[cfg(not(windows))]
pub fn speak(
    _client: &crate::ai::Client,
    _cfg: &crate::config::Config,
    _enabled: bool,
    _text: &str,
) {
}

/// SND_MEMORY 要求缓冲在播放期间存活：故意泄漏（每次几十 KB，可接受）
#[cfg(windows)]
fn play_wav(wav: &[u8]) {
    use windows::core::PCWSTR;
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY};
    let leaked: &[u8] = Box::leak(wav.to_vec().into_boxed_slice());
    unsafe {
        let _ = PlaySoundW(PCWSTR(leaked.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC);
    }
}
