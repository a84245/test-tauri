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
        let _ = unsafe {
            SetCurrentProcessExplicitAppUserModelID(&HSTRING::from("com.dev.pengmaitw"))
        };
    }

    tauri_app_lib::run()
}
