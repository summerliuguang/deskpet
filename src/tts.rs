//! TTS 播放：合成走 LiteGate MiMo（ai::Client::tts），播放走 Win32 PlaySound。
//! 合成与播放都在独立线程：PlaySound 用 SND_SYNC 阻塞的只是这个工作线程，
//! 播完返回后 wav 缓冲自然 drop 回收（旧方案 SND_ASYNC + 故意泄漏会持续累积）。
//! 行为与 ASYNC 一致：新的一次 PlaySound 会打断前一次播放。

#[cfg(windows)]
use std::sync::atomic::{AtomicU64, Ordering};

/// 上次合成开始时间（毫秒时间戳）：1.5 秒内不重复合成。
#[cfg(windows)]
static LAST_START_MS: AtomicU64 = AtomicU64::new(0);

/// enabled = 运行时开关（菜单"语音播报"）；force = 试听等用户主动行为
#[cfg(windows)]
pub fn speak(client: &crate::ai::Client, cfg: &crate::config::Config, enabled: bool, text: &str) {
    speak_opt(client, cfg, enabled, false, text)
}

#[cfg(windows)]
pub fn speak_force(client: &crate::ai::Client, cfg: &crate::config::Config, enabled: bool, text: &str) {
    speak_opt(client, cfg, enabled, true, text)
}

#[cfg(windows)]
fn speak_opt(
    client: &crate::ai::Client,
    cfg: &crate::config::Config,
    enabled: bool,
    force: bool,
    text: &str,
) {
    if !enabled || text.is_empty() {
        return;
    }
    let now = crate::now_ms();
    if !force && now.saturating_sub(LAST_START_MS.load(Ordering::Relaxed)) < 1500 {
        return;
    }
    LAST_START_MS.store(now, Ordering::Relaxed);

    let text: String = text.chars().take(120).collect();
    let client = client.clone();
    let cfg = cfg.clone();
    // detach 线程：合成 + 同步播放 + 缓冲回收，全程不碰事件循环
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

#[cfg(not(windows))]
pub fn speak_force(
    _client: &crate::ai::Client,
    _cfg: &crate::config::Config,
    _enabled: bool,
    _text: &str,
) {
}

/// SND_SYNC 在本线程播完才返回，随后缓冲即可安全释放
#[cfg(windows)]
fn play_wav(wav: &[u8]) {
    use windows::core::PCWSTR;
    use windows::Win32::Media::Audio::{PlaySoundW, SND_MEMORY, SND_SYNC};
    let owned = wav.to_vec();
    unsafe {
        let _ = PlaySoundW(
            PCWSTR(owned.as_ptr() as *const u16),
            None,
            SND_MEMORY | SND_SYNC,
        );
    }
}
