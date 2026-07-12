use tauri::{
    Emitter, Manager, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_notification::NotificationExt;

/// 由前端调用的自定义命令：用壳（Rust）原生 API 发送系统通知。
/// 这样绕开 Web Notification 的 HTTPS 安全上下文限制，也不经过通知插件的前端 ACL。
/// `id` 为可选的通知标识，前端用它回查「点击后跳转的路由」。
#[tauri::command]
fn notify(
    app: tauri::AppHandle,
    title: String,
    body: Option<String>,
    id: Option<i32>,
) -> Result<(), String> {
    send_notification(&app, title, body.unwrap_or_default(), id)
}

/// 通知点击动作事件载荷，前端据此恢复窗口并跳转路由。
#[derive(Clone, serde::Serialize)]
struct NotificationAction {
    id: Option<u32>,
}

/// 发送系统通知。
/// Windows / macOS（主要支持平台）走官方 tauri-plugin-notification，
/// 点击由前端 plugin-notification 的 onAction 接收，自动恢复窗口并跳路由，
/// 这是主路径，行为稳定。
/// Linux 桌面端 tauri-plugin-notification 的 show() 会丢弃 NotificationHandle，
/// 收不到点击回调，因此改走 notify-rust 并阻塞等待点击（见下方注释）。
fn send_notification(
    app: &tauri::AppHandle,
    title: String,
    body: String,
    id: Option<i32>,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        use notify_rust::Notification;
        let mut n = Notification::new();
        n.summary(&title).body(&body).appname("pengmaitw");
        if let Some(id) = id {
            n.id(id as u32);
        }
        // 注册默认动作，确保点击通知体或动作按钮都能触发 "default"
        n.action("default", "打开");
        match n.show() {
            Ok(handle) => {
                let app2 = app.clone();
                std::thread::spawn(move || {
                    handle.wait_for_action(move |action: &str| {
                        // "__closed" 表示用户直接关闭（未点击），不恢复窗口
                        if action != "__closed" {
                            if let Some(window) = app2.get_webview_window("main") {
                                let _ = window.show();
                                let _ = window.unminimize();
                                let _ = window.set_focus();
                            }
                            // 通知前端点击动作，让前端做路由跳转（Linux 下
                            // tauri-plugin-notification 的 onAction 不会触发）。
                            let _ = app2.emit("notification:action", NotificationAction {
                                id: id.map(|i| i as u32),
                            });
                        }
                    });
                });
                Ok(())
            }
            Err(e) => Err(format!("发送通知失败: {e}")),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut builder = app
            .notification()
            .builder()
            .title(title)
            .body(body);
        if let Some(id) = id {
            builder = builder.id(id);
        }
        builder.show().map_err(|e| e.to_string())
    }
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
            // Windows 通知（Toast）的点击激活依赖进程级 AppUserModelID，
            // 且必须与「开始菜单快捷方式」注册的一致。否则系统会把 Toast 点击
            // 当成幽灵事件静默丢弃，前端 onAction 不触发（窗口打不开、路由不跳转）。
            #[cfg(windows)]
            {
                use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
                use windows::core::HSTRING;
                let _ = unsafe {
                    SetCurrentProcessExplicitAppUserModelID(&HSTRING::from("com.dev.pengmaitw"))
                };
            }

            // 创建主窗口（加载业务页面）。
            // 默认加载线上地址；本地联调时可用环境变量指定本地前端：
            //   PENGMAI_FRONTEND_URL=http://localhost:5000 pnpm tauri dev
            let frontend_url = std::env::var("PENGMAI_FRONTEND_URL")
                .unwrap_or_else(|_| "http://110.42.239.85:5000".to_string());
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External(frontend_url.parse().unwrap()),
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
