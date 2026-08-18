#[allow(unused_imports)]
use tauri::{
    Emitter, Manager, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use rdev::{listen, Event, EventType, Key};
use std::time::Instant;
#[cfg(target_os = "macos")]
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
    eprintln!(
        "[notify] 收到前端通知请求 title={title:?} body={:?} id={id:?}",
        body.as_deref().unwrap_or("")
    );
    send_notification(&app, title, body.unwrap_or_default(), id)
}

/// 通知点击动作事件载荷，前端据此恢复窗口并跳转路由。
/// Linux 路径通过 notify-rust 的 wait_for_action 回调 emit 此事件。
#[derive(Clone, serde::Serialize)]
#[allow(dead_code)]
struct NotificationAction {
    id: Option<u32>,
}

/// 由前端调用的自定义命令：在 Windows 资源管理器中打开本地挂载盘（P:\）的
/// 对应文件夹并选中文件。配合员工端 rclone + WinFsp 挂载 MinIO 到 P:\ 使用。
///
/// `local_path` 期望是绝对路径，例如：
///   - 选中文件：`P:\Staff_Workspace\...\海报.ai` → explorer /select,"该路径"
///   - 打开文件夹：`P:\Staff_Workspace\...\客户文件夹` → 直接 explorer "该路径"
///
/// 返回值：
///   - `Ok("opened")` 成功启动 explorer
///   - `Err(msg)` 路径不存在 / 挂载盘未就绪 / 启动失败（前端据此回退到预览）
#[tauri::command]
fn open_local_folder(local_path: String) -> Result<String, String> {
    eprintln!("[open_local_folder] 收到本地路径 local_path={local_path:?}");

    // 1) 校验必须是绝对盘符路径（防注入/防穿越）
    if !local_path.contains(':') || local_path.len() < 3 {
        return Err(format!("非法本地路径: {local_path}"));
    }

    // 2) 校验路径存在 —— 若 P:\ 未挂载或文件不存在，立刻失败给前端回退
    let path = std::path::Path::new(&local_path);
    if !path.exists() {
        eprintln!("[open_local_folder] 路径不存在（可能挂载盘未就绪）: {local_path}");
        return Err(format!("路径不存在（请确认本地挂载盘 P:\\ 已启动）: {local_path}"));
    }

    // 3) 区分：文件 → /select 选中；目录 → 直接打开
    let (program, args) = if path.is_dir() {
        ("explorer.exe", vec![local_path.clone()])
    } else {
        // /select,"路径" —— 用逗号分隔参数，路径加引号，文件名含空格也安全
        ("explorer.exe", vec!["/select,".to_string(), local_path.clone()])
    };

    match std::process::Command::new(program)
        .args(&args)
        .spawn()
    {
        Ok(_) => {
            eprintln!("[open_local_folder] 已在资源管理器中打开: {local_path}");
            Ok("opened".to_string())
        }
        Err(e) => {
            eprintln!("[open_local_folder] 启动 explorer 失败: {e}");
            Err(format!("打开资源管理器失败: {e}"))
        }
    }
}

/// 发送系统通知。
/// Linux / Windows：走 notify-rust 并阻塞等待点击动作，回调中直接恢复窗口 +
/// emit notification:action 事件给前端做路由跳转。这样可以绕开
/// tauri-plugin-notification 在 Linux 桌面端丢弃 NotificationHandle、
/// 以及在 Windows 上 COM 激活注册可能失败导致 onAction 不触发的问题。
/// macOS：走 tauri-plugin-notification（该平台行为稳定）。
fn send_notification(
    app: &tauri::AppHandle,
    title: String,
    body: String,
    id: Option<i32>,
) -> Result<(), String> {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
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
                let id_for_emit = id;
                std::thread::spawn(move || {
                    eprintln!("[notify] 等待通知点击（wait_for_action）...");
                    handle.wait_for_action(move |action: &str| {
                        eprintln!("[notify] 收到点击动作 action={action:?}");
                        // "__closed" 表示用户直接关闭（未点击），不恢复窗口
                        if action != "__closed" {
                            // 窗口操作必须在主线程执行（Windows 下跨线程
                            // ShowWindow/SetForegroundWindow 对隐藏窗口无效）
                            let app3 = app2.clone();
                            let _ = app2.run_on_main_thread(move || {
                                if let Some(window) = app3.get_webview_window("main") {
                                    let _ = window.show();
                                    let _ = window.unminimize();
                                    let _ = window.set_focus();
                                    eprintln!("[notify] 窗口已恢复（主线程）");
                                } else {
                                    eprintln!("[notify] 未找到 main 窗口！");
                                }
                            });
                            // 通知前端点击动作，让前端做路由跳转
                            let payload = NotificationAction {
                                id: id_for_emit.map(|i| i as u32),
                            };
                            match app2.emit("notification:action", payload) {
                                Ok(_) => eprintln!("[notify] notification:action 事件已发出"),
                                Err(e) => eprintln!("[notify] 事件发送失败：{e}"),
                            }
                        } else {
                            eprintln!("[notify] 通知被关闭/忽略");
                        }
                    });
                });
                eprintln!("[notify] 通知已发送（notify-rust）title={title:?} id={id:?}");
                Ok(())
            }
            Err(e) => {
                eprintln!("[notify] 通知发送失败：{e}");
                Err(format!("发送通知失败: {e}"))
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        eprintln!("[notify] macOS 发送通知 title={title:?} body={body:?} id={id:?}");
        let mut builder = app
            .notification()
            .builder()
            .title(&title)
            .body(&body);
        if let Some(ref id_val) = id {
            builder = builder.id(*id_val);
        }
        match builder.show() {
            Ok(_) => {
                eprintln!("[notify] macOS 通知已成功发送");
                Ok(())
            }
            Err(e) => {
                eprintln!("[notify] macOS 通知发送失败：{e}");
                Err(e.to_string())
            }
        }
    }
}

/// 全局键盘监听：捕获扫码枪输入（即使窗口失焦/后台运行也能收到）。
/// 识别规则与前端一致：连续字符间隔 <80ms 视为扫码枪，Enter 结束一条。
/// 识别到完整条码后 emit `scan:code` 事件给前端处理。
fn start_scan_listener(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let mut buf = String::new();
        let mut last = Instant::now();

        if let Err(e) = listen(move |event: Event| {
            match event.event_type {
                EventType::KeyPress(Key::Return) => {
                    let code = buf.trim().to_string();
                    buf.clear();
                    if !code.is_empty() {
                        let _ = app.emit("scan:code", code);
                    }
                }
                EventType::KeyPress(_) => {
                    // event.name 是 OS 解释后的字符（尊重大小写/键盘布局），
                    // 只累积单个可打印字符（数字/字母/常用符号）。
                    if let Some(name) = event.name {
                        if name.chars().count() == 1 {
                            let now = Instant::now();
                            let fast = now.duration_since(last).as_millis() < 80;
                            last = now;
                            if fast {
                                buf.push_str(&name);
                            } else {
                                // 慢速输入：视为普通打字，重新开始
                                buf.clear();
                                buf.push_str(&name);
                            }
                        }
                    }
                }
                _ => {}
            }
        }) {
            eprintln!("[scan] 全局键盘监听启动失败: {e:?}");
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![notify, open_local_folder])
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
            // 进程级 AppUserModelID 已在 main.rs 中设置（早于插件初始化），
            // tauri-plugin-notification 的 Windows COM 激活器将使用正确的 AUMID
            // 进行注册，确保 Toast 通知点击能正确回传到本进程。

            // 启动时清理 WebView2 缓存，避免加载失败响应(404)被缓存导致白屏/404
            // 必须在创建窗口(WebView)之前执行，否则缓存目录被占用删不掉
            if let Ok(appdata) = std::env::var("LOCALAPPDATA") {
                let cache_dir = std::path::Path::new(&appdata)
                    .join("com.dev.pengmaitw")
                    .join("EBWebView");
                if cache_dir.exists() {
                    let _ = std::fs::remove_dir_all(&cache_dir);
                }
            }

            // 创建主窗口（加载业务页面）。
            // 默认加载线上地址；本地联调时可用环境变量指定本地前端：
            //   PENGMAI_FRONTEND_URL=http://localhost:5000 pnpm tauri dev
            let frontend_url = std::env::var("PENGMAI_FRONTEND_URL")
                .unwrap_or_else(|_| "http://110.42.239.85:5000".to_string());
            // 主窗口的 app handle，供 on_new_window 闭包创建子窗口用
            let app_handle = app.handle().clone();
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External(frontend_url.parse().unwrap()),
            )
            .title("芃麦印刷")
            .inner_size(1200.0, 800.0)
            // window.open 在应用内新开窗口（预览/工作单等），不弹系统浏览器
            .on_new_window(move |url, features| {
                let handle = app_handle.clone();
                // window.open('') 打开空白窗口供前端 document.write（工作单场景）
                let target = if url.as_str().is_empty() || url.as_str() == "about:blank" {
                    "about:blank".to_string()
                } else {
                    url.to_string()
                };
                let label = format!(
                    "window_{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis()
                );
                let window = tauri::WebviewWindowBuilder::new(
                    &handle,
                    &label,
                    tauri::WebviewUrl::External(target.parse().unwrap()),
                )
                .title(url.as_str())
                .window_features(features)
                .on_document_title_changed(|window, title| {
                    let _ = window.set_title(&title);
                })
                .build();
                match window {
                    Ok(w) => tauri::webview::NewWindowResponse::Create { window: w },
                    Err(_) => tauri::webview::NewWindowResponse::Deny,
                }
            })
            // 放行下载（WebView2 默认弹保存对话框）
            .on_download(|_webview, _event| true)
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

            // 启动全局扫码监听（窗口后台/失焦也能扫）
            start_scan_listener(app.handle().clone());

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
