use tauri::{
    Manager, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_notification::NotificationExt;

/// 由前端调用的自定义命令：用壳（Rust）原生 API 发送系统通知。
/// 这样绕开 Web Notification 的 HTTPS 安全上下文限制，也不经过通知插件的前端 ACL。
#[tauri::command]
fn notify(app: tauri::AppHandle, title: String, body: Option<String>) -> Result<(), String> {
    app.notification()
        .builder()
        .title(title)
        .body(body.unwrap_or_default())
        .show()
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![notify])
        .on_window_event(|window, event| {
            // 拦截主窗口关闭：弹原生对话框，询问「后台挂起」或「退出程序」
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    // 阻止默认关闭行为
                    api.prevent_close();
                    let window = window.clone();
                    let app = window.app_handle().clone();
                    window
                        .dialog()
                        .message("关闭后程序将最小化到系统托盘后台运行，仍可接收系统通知。你也可以选择退出程序。")
                        .title("是否后台挂起？")
                        .buttons(MessageDialogButtons::OkCancelCustom(
                            "后台挂起".to_string(),
                            "退出程序".to_string(),
                        ))
                        .show(move |answer| {
                            if answer {
                                // 后台挂起：隐藏窗口到托盘，进程继续运行
                                let _ = window.hide();
                            } else {
                                // 退出程序
                                app.exit(0);
                            }
                        });
                }
            }
        })
        .setup(|app| {
            // 创建主窗口（加载远程业务页面）
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External("http://110.42.239.85:5000".parse().unwrap()),
            )
            .title("芃麦印刷")
            .inner_size(1200.0, 800.0)
            .build()?;

            // 系统托盘菜单：左键点击恢复主界面，右键弹出菜单
            let show_i = MenuItem::with_id(app, "show", "显示主界面", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &quit_i])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("芃麦印刷")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.unminimize();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    // 左键单击托盘图标 -> 显示并聚焦主窗口
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.unminimize();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
