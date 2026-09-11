// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 必须在任何通知插件初始化之前设置进程级 AppUserModelID，
    // 以便 tauri-plugin-notification 的 Windows COM 激活器使用正确的 AUMID 进行注册。
    // 这确保了 Windows Toast 通知点击能正确回传到本进程。
    #[cfg(windows)]
    {
        use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
        use windows::core::HSTRING;
        // 取自 lib.rs 的 APP_ID，与发通知时用的 appname、以及注册表/快捷方式里的
        // AUMID 保持单一来源，避免三处不一致导致通知显示不出应用名和图标。
        let _ = unsafe {
            SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(tauri_app_lib::APP_ID))
        };
    }

    tauri_app_lib::run()
}
