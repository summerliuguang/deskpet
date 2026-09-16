//! 输入联动：全局键盘低级钩子（打字时猫拍爪）+ XInput 手柄轮询（按键触发反应）。
//!
//! 低占用：钩子回调只写一个事件就返回；手柄 10Hz 轮询，无手柄时开销可忽略。
//! 两者都可通过 deskpet.toml 的 keyboard_link / gamepad_link 关闭。

#[cfg(windows)]
pub mod win {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::OnceLock;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, SetWindowsHookExW, MSG, WH_KEYBOARD_LL, WM_KEYDOWN,
    };
    use windows::Win32::UI::Input::XboxController::{
        XInputGetState, XINPUT_GAMEPAD_A, XINPUT_GAMEPAD_B, XINPUT_GAMEPAD_X, XINPUT_GAMEPAD_Y,
        XINPUT_STATE,
    };

    static KEY_PROXY: OnceLock<crate::PetEventProxy> = OnceLock::new();
    static LAST_FWD_MS: AtomicU64 = AtomicU64::new(0);

    static PAD_PROXY: OnceLock<crate::PetEventProxy> = OnceLock::new();

    /// 键盘低级钩子线程：必须泵消息才会收到事件
    pub fn spawn_keyboard_hook(proxy: crate::PetEventProxy) {
        KEY_PROXY.set(proxy).ok();
        std::thread::spawn(|| unsafe {
            let Ok(_hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), None, 0) else {
                return;
            };
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
        });
    }

    unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // 回调必须快进快出：只做节流判断 + 投递事件
        if code >= 0 && wparam.0 as u32 == WM_KEYDOWN {
            let now = crate::now_ms();
            let last = LAST_FWD_MS.load(Ordering::Relaxed);
            // 100ms 节流：按键长按重复不再高频唤醒事件循环
            if now.saturating_sub(last) >= 100 {
                LAST_FWD_MS.store(now, Ordering::Relaxed);
                if let Some(p) = KEY_PROXY.get() {
                    let _ = p.send_event(crate::PetEvent::Typing);
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    /// XInput 手柄轮询线程：10Hz，按键边沿触发
    pub fn spawn_gamepad_poll(proxy: crate::PetEventProxy) {
        PAD_PROXY.set(proxy).ok();
        std::thread::spawn(|| {
            let mut prev: u16 = 0;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let mut st = XINPUT_STATE::default();
                if unsafe { XInputGetState(0, &mut st) } != 0 {
                    prev = 0;
                    continue;
                }
                let buttons = st.Gamepad.wButtons.0;
                let interesting = (XINPUT_GAMEPAD_A.0 | XINPUT_GAMEPAD_B.0 | XINPUT_GAMEPAD_X.0 | XINPUT_GAMEPAD_Y.0) as u16;
                let pressed = buttons & interesting;
                if pressed & !prev != 0 {
                    if let Some(p) = PAD_PROXY.get() {
                        let _ = p.send_event(crate::PetEvent::Gamepad);
                    }
                }
                prev = buttons;
            }
        });
    }

    /// 全局快捷键线程：Ctrl+Shift + D勿扰 / T待办 / C聊天 / H隐藏显示 / Q退出。
    /// 被其他程序占用的组合注册失败时跳过（不影响其余键）；
    /// 线程常驻，开关由主循环按 settings.hotkeys 过滤。
    pub fn spawn_hotkeys(proxy: crate::PetEventProxy) {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            RegisterHotKey, HOT_KEY_MODIFIERS, MOD_CONTROL, MOD_SHIFT,
        };
        use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};
        std::thread::spawn(move || unsafe {
            let modifier: HOT_KEY_MODIFIERS = MOD_CONTROL | MOD_SHIFT;
            // (事件 id, 虚拟键码)
            let keys = [
                (0u32, b'D' as u32),
                (1, b'T' as u32),
                (2, b'C' as u32),
                (3, b'H' as u32),
                (4, b'Q' as u32),
            ];
            for (id, vk) in keys {
                // None = 关联到调用线程的消息队列（无需窗口）
                let _ = RegisterHotKey(Some(HWND::default()), id as i32, modifier, vk);
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message == WM_HOTKEY {
                    let _ = proxy.send_event(crate::PetEvent::Hotkey(msg.wParam.0 as u32));
                }
            }
        });
    }
}
