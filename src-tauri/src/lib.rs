#[allow(unused_imports)]
use tauri::{
    Emitter, LogicalPosition, LogicalSize, Manager, WebviewUrl, WebviewWindow, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_updater::UpdaterExt;
use rdev::{listen, Event, EventType, Key};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
#[cfg(not(debug_assertions))]
use std::time::Duration;
#[cfg(target_os = "macos")]
use tauri_plugin_notification::NotificationExt;

/// 应用 AppUserModelID（AUMID）。
///
/// 必须三处完全一致，否则 Windows 无法把 Toast 归属到本应用，
/// 通知上的「应用名」和「图标」会显示成空白/默认值：
///   1. main.rs 的 SetCurrentProcessExplicitAppUserModelID
///   2. tauri.conf.json 的 identifier
///   3. 注册表 HKCU\Software\Classes\AppUserModelId\<此值> 的 DisplayName / IconUri
///      （由 installer-hooks.nsh 在安装时写入）
pub const APP_ID: &str = "com.dev.pengmaitw";

/// 通知按钮：identifier -> 按钮文案。
/// 前端点击后 Rust 侧 wait_for_action 会收到 identifier，
/// 除了「忽略」以外的动作都视为「查看」，会恢复窗口并触发路由跳转。
const ACTION_VIEW: &str = "default";
const ACTION_IGNORE: &str = "ignore";

// ─────────────────────────── 自绘通知卡片窗口 ───────────────────────────
// 系统 toast 的外观由 Windows 绘制、应用改不了，所以改成自己开一个
// 无边框透明小窗，用本地 notify-popup.html 画卡片；主程序缩到托盘时
// 这个窗口依然存在，所以后台也能弹。

/// 通知卡片窗口的 label（对应 src/notify-popup.html）
const NOTIFY_POPUP_LABEL: &str = "notify_popup";
/// 卡片窗口宽度（逻辑像素；高度随内容动态变化）。
/// 需要放下「订单号 / 客户 / 下单产品 / 下单时间 / 金额」五列表格，
/// 订单号是 WEB+17 位（19 字符），太窄会把它挤成省略号。
const NOTIFY_POPUP_WIDTH: f64 = 600.0;
/// 距屏幕右下角的留白
const NOTIFY_POPUP_MARGIN: f64 = 16.0;

/// 发给通知卡片窗口的载荷。
#[derive(Clone, serde::Serialize)]
struct NotifyPopupPayload {
    /// 通知 id：点击「查看订单」时原样回传，前端据此查路由
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<i32>,
    title: String,
    body: String,
    /// 卡片图标类型：order / info / error
    kind: String,
    /// 有跳转目标时才显示「查看订单」按钮
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    /// 结构化订单信息（orderNo / customer / product / time / amount），
    /// 由前端原样透传，卡片按这些字段渲染成多列表格。
    /// 这里用 Value 而不定义结构体：字段由业务侧决定，壳只负责转发，
    /// 以后加字段不用改 exe 再发一版。
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

/// 计算卡片窗口位置：贴住主显示器工作区（work_area 已排除任务栏）的右下角。
/// 窗口尺寸是逻辑像素，work_area 是物理像素，这里统一换算成逻辑像素。
fn notify_popup_position(win: &WebviewWindow, height: f64) -> Option<(f64, f64)> {
    let monitor = win.primary_monitor().ok().flatten()?;
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let area_x = area.position.x as f64 / scale;
    let area_y = area.position.y as f64 / scale;
    let area_w = area.size.width as f64 / scale;
    let area_h = area.size.height as f64 / scale;
    let x = area_x + area_w - NOTIFY_POPUP_WIDTH - NOTIFY_POPUP_MARGIN;
    let y = area_y + area_h - height - NOTIFY_POPUP_MARGIN;
    Some((x, y))
}

/// 取得（必要时创建）通知卡片窗口。窗口透明、无边框、置顶、不进任务栏、
/// 且 `.focused(false)`——弹通知时不抢焦点，不打断用户正在做的事。
fn ensure_notify_popup(app: &tauri::AppHandle) -> Option<WebviewWindow> {
    if let Some(w) = app.get_webview_window(NOTIFY_POPUP_LABEL) {
        return Some(w);
    }
    let builder = tauri::WebviewWindowBuilder::new(
        app,
        NOTIFY_POPUP_LABEL,
        WebviewUrl::App("notify-popup.html".into()),
    )
    .title("通知")
    .inner_size(NOTIFY_POPUP_WIDTH, 180.0)
    .resizable(false)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .shadow(false)
    .focused(false)
    .visible(false);

    // transparent() 在 macOS 上被 #[cfg(any(not(target_os = "macos"), feature = "macos-private-api"))]
    // 门控（需要私有 API，仅 App Store 之外的分发可用），release 构建下未开该特性会直接编译失败。
    // 这里只在非 macOS 开启透明；macOS 退化为不透明窗口，卡片功能不受影响。
    #[cfg(not(target_os = "macos"))]
    let builder = builder.transparent(true);

    match builder.build() {
        Ok(w) => Some(w),
        Err(e) => {
            eprintln!("[notify-popup] 窗口创建失败: {e}");
            None
        }
    }
}

/// 在屏幕右下角弹出一条自绘通知卡片。
/// 卡片常驻、不自动消失，直到用户点「忽略」/「查看订单」/ ✕。
fn show_notify_popup(
    app: &tauri::AppHandle,
    title: String,
    body: String,
    id: Option<i32>,
    data: Option<serde_json::Value>,
) -> Result<(), String> {
    let win = ensure_notify_popup(app).ok_or_else(|| "通知窗口创建失败".to_string())?;

    let payload = NotifyPopupPayload {
        id,
        title,
        body,
        kind: "order".into(),
        path: Some("/orders".into()),
        data,
    };
    win.emit("notify:show", payload).map_err(|e| e.to_string())?;

    // 先按当前高度摆好位置再显示，避免在高处闪一下再跳
    let h = win
        .outer_size()
        .map(|s| s.height as f64 / win.scale_factor().unwrap_or(1.0))
        .unwrap_or(180.0);
    if let Some((x, y)) = notify_popup_position(&win, h) {
        let _ = win.set_position(LogicalPosition::new(x, y));
    }
    win.show().map_err(|e| e.to_string())?;
    Ok(())
}

/// 卡片内容高度变化 → 同步窗口尺寸并重新贴住右下角（底边固定不动）。
#[tauri::command]
fn notify_popup_resize(app: tauri::AppHandle, height: f64) {
    let Some(win) = app.get_webview_window(NOTIFY_POPUP_LABEL) else {
        return;
    };
    let h = height.clamp(60.0, 2000.0);
    let _ = win.set_size(LogicalSize::new(NOTIFY_POPUP_WIDTH, h));
    if let Some((x, y)) = notify_popup_position(&win, h) {
        let _ = win.set_position(LogicalPosition::new(x, y));
    }
}

/// 卡片全部关完 → 收起窗口（进程仍在托盘运行，主窗口不受影响）。
#[tauri::command]
fn notify_popup_hide(app: tauri::AppHandle) {
    if let Some(win) = app.get_webview_window(NOTIFY_POPUP_LABEL) {
        let _ = win.hide();
    }
}

/// 卡片按钮点击：
/// - view：唤起并聚焦主窗口，再让前端按 id 做路由跳转（复用现有链路）
/// - 其它（忽略 / 关闭）：什么都不做，卡片由前端自己移除
#[tauri::command]
fn notify_popup_action(app: tauri::AppHandle, id: Option<i32>, action: String) {
    if action != "view" {
        eprintln!("[notify-popup] 忽略通知 id={id:?}");
        return;
    }
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
    let payload = NotificationAction {
        id: id.map(|i| i as u32),
    };
    let _ = app.emit("notification:action", payload);
}

/// 由前端调用的自定义命令：在屏幕右下角弹出自绘通知卡片。
/// 前端仍是原来的 `invoke('notify', {title, body, id})`，壳内部换实现即可。
/// `id` 为可选的通知标识，前端用它回查「点击后跳转的路由」。
#[tauri::command]
fn notify(
    app: tauri::AppHandle,
    title: String,
    body: Option<String>,
    id: Option<i32>,
    data: Option<serde_json::Value>,
) -> Result<(), String> {
    eprintln!(
        "[notify] 收到前端通知请求 title={title:?} body={:?} id={id:?} data={:?}",
        body.as_deref().unwrap_or(""),
        data.as_ref().map(|v| v.to_string()).unwrap_or_default()
    );
    show_notify_popup(&app, title, body.unwrap_or_default(), id, data)
}

/// 通知点击动作事件载荷，前端据此恢复窗口并跳转路由。
/// Linux 路径通过 notify-rust 的 wait_for_action 回调 emit 此事件。
#[derive(Clone, serde::Serialize)]
#[allow(dead_code)]
struct NotificationAction {
    id: Option<u32>,
}

/// 更新进度事件载荷，发给「下载进度」小窗（update_progress）。
#[derive(Clone, serde::Serialize)]
struct UpdateProgress {
    /// 已下载字节数
    downloaded: u64,
    /// 总字节数（服务器可能不给 Content-Length，此时为 None）
    total: Option<u64>,
    /// 状态：start / downloading / installing / done / error
    status: String,
    /// error 状态时的错误描述
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
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

/// 返回当前应用版本号（如 "0.3.1"），前端用于升级检查对比
#[tauri::command]
fn get_app_version(app: tauri::AppHandle) -> String {
    app.package_info().version.to_string()
}

/// 检查是否有新版本。
/// `manual`：托盘手动触发时，无更新/失败会弹提示；启动自动与后台定时检测失败一律静默
/// （可能临时没网，不打扰使用）。
/// `periodic`：后台每 60 分钟定时检测——发现新版只发一次系统通知（用户点击才更新），
///             同一版本在本次运行内提醒过就不再打扰。
async fn check_for_updates(app: tauri::AppHandle, manual: bool, periodic: bool) {
    eprintln!("[update] 检查更新 manual={manual} periodic={periodic}");
    let result = match app.updater() {
        Ok(updater) => updater.check().await.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    match result {
        Ok(Some(update)) => {
            let version = update.version.clone();
            eprintln!(
                "[update] 发现新版本 v{version}（当前 v{}）",
                update.current_version
            );
            if periodic {
                // 后台定时：仅提醒一次，点击通知才开始下载更新
                if update_remind_once(&version) {
                    notify_update_available(app, update);
                } else {
                    eprintln!("[update] v{version} 已在本次运行提醒过，跳过");
                }
            } else {
                update_remind_once(&version);
                prompt_update(app, update);
            }
        }
        Ok(None) => {
            eprintln!("[update] 已是最新版本");
            if manual {
                let _ = app
                    .dialog()
                    .message("当前已是最新版本。")
                    .title("检查更新")
                    .buttons(MessageDialogButtons::Ok)
                    .show(|_| {});
            }
        }
        Err(e) => {
            eprintln!("[update] 检查更新失败: {e}");
            if manual {
                let _ = app
                    .dialog()
                    .message(format!("检查更新失败：{e}\n请确认网络连接或稍后重试。"))
                    .title("检查更新")
                    .buttons(MessageDialogButtons::Ok)
                    .show(|_| {});
            }
        }
    }
}

/// 同一版本在本次运行内只提醒一次的标记。返回 `true` 表示「本次还没提醒过」。
fn update_remind_once(version: &str) -> bool {
    static NOTIFIED: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    let mut guard = NOTIFIED
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap();
    if guard.as_deref() == Some(version) {
        false
    } else {
        *guard = Some(version.to_string());
        true
    }
}

/// 后台定时检测发现新版：发一条系统通知提醒，点击通知才开始更新（Windows/Linux）。
/// macOS 的点击回调未接入，退化为直接弹确认框。
fn notify_update_available(app: tauri::AppHandle, update: tauri_plugin_updater::Update) {
    let version = update.version.clone();
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        use notify_rust::Notification;
        let mut n = Notification::new();
        n.summary(&format!("发现新版本 v{version}"))
            .body("点击立即更新（将自动下载并重启）")
            .appname("pengmaitw");
        match n.show() {
            Ok(handle) => {
                eprintln!("[update] 已发送「发现新版本 v{version}」通知");
                let app2 = app.clone();
                let ver = version;
                std::thread::spawn(move || {
                    // 点击后只触发一次（pending.take），避免重复点击重复下载
                    let mut pending = Some(update);
                    handle.wait_for_action(move |action: &str| {
                        if action != "__closed" {
                            if let Some(u) = pending.take() {
                                eprintln!("[update] 用户点击更新通知，开始下载 v{ver}");
                                let app3 = app2.clone();
                                let _ = app2.run_on_main_thread(move || {
                                    download_update(app3, u);
                                });
                            }
                        }
                    });
                });
            }
            Err(e) => {
                eprintln!("[update] 更新提醒通知发送失败: {e}");
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        prompt_update(app, update);
    }
}

/// 弹确认框；确认后异步下载并安装（Windows 由 NSIS 安装器自动重启新版本）。
fn prompt_update(app: tauri::AppHandle, update: tauri_plugin_updater::Update) {
    let version = update.version.clone();
    let current = update.current_version.clone();
    let app2 = app.clone();
    let _ = app
        .dialog()
        .message(format!(
            "检测到新版本 v{version}（当前 v{current}）。\n\n点击「立即更新」将自动下载并安装，完成后应用会自动重启。"
        ))
        .title("发现新版本")
        .buttons(MessageDialogButtons::OkCancelCustom(
            "立即更新".to_string(),
            "稍后".to_string(),
        ))
        .show(move |ok| {
            if ok {
                download_update(app2, update);
            } else {
                eprintln!("[update] 用户选择稍后更新");
            }
        });
}

/// 用户确认后：弹出「下载进度」小窗，后台下载，完成后自动进入安装。
/// Windows：安装由 NSIS 被动模式接管（自带安装进度），装完自动重启；
/// macOS/Linux：装完后自行拉起新版本再退出本进程。
fn download_update(app: tauri::AppHandle, update: tauri_plugin_updater::Update) {
    let version = update.version.clone();
    eprintln!("[update] 开始下载并安装 v{version} …");

    // 下载进度小窗（加载本地 update-progress.html，纯静态页，通过事件收进度）
    if let Err(e) = tauri::WebviewWindowBuilder::new(
        &app,
        "update_progress",
        tauri::WebviewUrl::App("update-progress.html".into()),
    )
    .title("正在更新")
    .inner_size(460.0, 160.0)
    .resizable(false)
    .center()
    .build()
    {
        eprintln!("[update] 进度窗创建失败（将静默下载）: {e}");
    }

    tauri::async_runtime::spawn(async move {
        let emitter = app.clone();
        let _ = emitter.emit(
            "update:progress",
            UpdateProgress {
                downloaded: 0,
                total: None,
                status: "start".into(),
                message: None,
            },
        );

        // 进度回调（每约 256KiB 发一次，避免高频事件刷屏）
        let emitter_prog = emitter.clone();
        let mut acc: u64 = 0;
        let mut last_sent: u64 = 0;
        let on_chunk = move |downloaded: usize, total: Option<u64>| {
            acc += downloaded as u64;
            if acc >= last_sent + 262_144 {
                last_sent = acc;
                let _ = emitter_prog.emit(
                    "update:progress",
                    UpdateProgress {
                        downloaded: acc,
                        total,
                        status: "downloading".into(),
                        message: None,
                    },
                );
            }
        };
        // 下载完成（进入安装阶段）
        let emitter_done = emitter.clone();
        let on_finish = move || {
            let _ = emitter_done.emit(
                "update:progress",
                UpdateProgress {
                    downloaded: 0,
                    total: None,
                    status: "installing".into(),
                    message: None,
                },
            );
        };

        let result = update
            .download_and_install(on_chunk, on_finish)
            .await;
        match result {
            Ok(_) => {
                let _ = emitter.emit(
                    "update:progress",
                    UpdateProgress {
                        downloaded: 0,
                        total: None,
                        status: "done".into(),
                        message: None,
                    },
                );
                eprintln!("[update] 安装完成。");
                // Windows 下 download_and_install 内部会启动 NSIS 安装器并 exit(0)，
                // 不会执行到这里；macOS/Linux 装完后需要自行重启到新版本。
                #[cfg(not(target_os = "windows"))]
                {
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = std::process::Command::new(exe).spawn();
                    }
                }
                std::process::exit(0);
            }
            Err(e) => {
                let msg = format!("下载/安装失败：{e}");
                eprintln!("[update] {msg}");
                let _ = emitter.emit(
                    "update:progress",
                    UpdateProgress {
                        downloaded: 0,
                        total: None,
                        status: "error".into(),
                        message: Some(msg.clone()),
                    },
                );
                let _ = emitter
                    .dialog()
                    .message(&msg)
                    .title("更新失败")
                    .show(|_| {});
            }
        }
    });
}

/// 发送【系统】通知（Windows toast）。
///
/// 目前 notify 命令已改为弹自绘卡片（见 show_notify_popup），本函数保留备用：
/// 系统通知的优势是能进 Windows 通知中心，人不在电脑前时回来还能看到。
/// 若以后想恢复「前台弹卡片 / 后台发系统通知」的双通道，直接重新调用它即可。
///
/// Linux / Windows：走 notify-rust 并阻塞等待点击动作，回调中直接恢复窗口 +
/// emit notification:action 事件给前端做路由跳转。这样可以绕开
/// tauri-plugin-notification 在 Linux 桌面端丢弃 NotificationHandle、
/// 以及在 Windows 上 COM 激活注册可能失败导致 onAction 不触发的问题。
/// macOS：走 tauri-plugin-notification（该平台行为稳定）。
#[allow(dead_code)]
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
        // appname 必须与进程级 AUMID 一致：不一致时 Windows 找不到归属，
        // 通知上的应用名与图标会掉成空白/默认值。
        n.summary(&title).body(&body).appname(APP_ID);
        if let Some(id) = id {
            n.id(id as u32);
        }
        // 两个按钮：查看订单 / 忽略。identifier 会在点击时回传，
        // 由下方 wait_for_action 区分（ignore 只关通知、不跳转）。
        n.action(ACTION_VIEW, "查看订单");
        n.action(ACTION_IGNORE, "忽略");
        match n.show() {
            Ok(handle) => {
                let app2 = app.clone();
                let id_for_emit = id;
                std::thread::spawn(move || {
                    eprintln!("[notify] 等待通知点击（wait_for_action）...");
                    handle.wait_for_action(move |action: &str| {
                        eprintln!("[notify] 收到点击动作 action={action:?}");
                        // "__closed" = 用户直接关掉通知；"ignore" = 点了「忽略」按钮。
                        // 两者都只关通知：不恢复窗口、不跳转路由。
                        if action != "__closed" && action != ACTION_IGNORE {
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
/// 前端把要保存的文件字节递过来：弹「另存为」对话框 → 写盘。
///
/// 为什么不让页面直接 `<a download>`：
/// 主窗口装了 `.on_download(|_, _| true)`，而 wry 收到 true 之后会
/// `args.SetHandled(true)` **接管**这次下载 —— 后果是 WebView2 自带的下载气泡
/// 被完全抑制，保存路径沿用 WebView2 推出来的默认 ResultFilePath。
/// 而页面里全站下载都是 `blob:` URL（既没有文件名也没有目录），推不出合法路径，
/// 于是写盘失败且**零提示**。所以改由前端把字节交过来，这里给它一个正经的保存框。
///
/// 返回 `true` = 已保存；`false` = 用户在对话框里点了取消。
#[tauri::command]
fn save_bytes(app: tauri::AppHandle, filename: String, contents: Vec<u8>) -> Result<bool, String> {
    let Some(file_path) = app
        .dialog()
        .file()
        .set_file_name(&filename)
        .blocking_save_file()
    else {
        return Ok(false); // 用户取消
    };
    let path = file_path.into_path().map_err(|e| e.to_string())?;
    std::fs::write(&path, contents).map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;
    Ok(true)
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            notify,
            open_local_folder,
            get_app_version,
            notify_popup_resize,
            notify_popup_hide,
            notify_popup_action,
            save_bytes
        ])
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

            // 启动时清理 WebView2 的 HTTP 缓存，避免加载失败响应(404)被缓存导致白屏。
            // 必须在创建窗口(WebView)之前执行，否则目录被占用删不掉。
            //
            // ⚠️ 只能删「纯缓存」子目录：EBWebView 整个目录里同时放着
            // Default/Local Storage（登录态 auth_state 就存这里）、IndexedDB、
            // Network/Cookies 等持久数据。以前是整个目录 remove_dir_all，
            // 结果每次启动都把登录态一起清掉——登录页默认勾选的
            // 「记住登录（30 天）」因此从未真正生效过，每次开机都要重新登录。
            if let Ok(appdata) = std::env::var("LOCALAPPDATA") {
                let webview_dir = std::path::Path::new(&appdata)
                    .join("com.dev.pengmaitw")
                    .join("EBWebView");
                // 这些目录删掉只会让下次加载慢一点，不会丢任何用户数据
                const CACHE_SUBDIRS: [&str; 3] = [
                    "Default/Cache",
                    "Default/Code Cache",
                    "Default/GPUCache",
                ];
                for sub in CACHE_SUBDIRS {
                    let dir = webview_dir.join(sub);
                    if dir.exists() {
                        let _ = std::fs::remove_dir_all(&dir);
                    }
                }
            }

            // 创建主窗口（加载业务页面）。
            // 默认加载线上地址；本地联调时可用环境变量指定本地前端：
            //   PENGMAI_FRONTEND_URL=http://localhost:5000 pnpm tauri dev
            let frontend_url = std::env::var("PENGMAI_FRONTEND_URL")
                .unwrap_or_else(|_| "http://110.42.239.85:5000".to_string());
            // 主窗口的 app handle，供 on_new_window 闭包创建子窗口用
            let app_handle = app.handle().clone();
            // 窗口标题固定为「芃麦印刷-版本号」，忽略远程页面的 document.title
            let app_version = app.package_info().version.to_string();
            let main_window_title = format!("芃麦印刷-{app_version}");
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External(frontend_url.parse().unwrap()),
            )
            .title(main_window_title.clone())
            .inner_size(1200.0, 800.0)
            // 远程页面改 document.title 会试图覆盖标题，这里一律改回固定标题
            .on_document_title_changed({
                let fixed = main_window_title.clone();
                move |window, _title| {
                    let _ = window.set_title(&fixed);
                }
            })
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
            let update_i = MenuItem::with_id(app, "update", "检查更新…", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &update_i, &quit_i])?;

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
                    "update" => {
                        // 手动检查更新（有新版会弹确认框）
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            check_for_updates(app, true, false).await;
                        });
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

            // 发布版更新检测：
            //   1) 启动约 8 秒后自动检查一次（有新版会弹确认框）
            //   2) 之后每 60 分钟后台静默定时检测（有新版只发一次通知，点击才更新；失败不打扰）
            // debug 构建不做自动检查，需要时用托盘「检查更新…」手动触发。
            #[cfg(not(debug_assertions))]
            {
                let app_handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(8));
                    let app_startup = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        check_for_updates(app_startup, false, false).await;
                    });
                    // 60 分钟周期定时检测
                    loop {
                        std::thread::sleep(Duration::from_secs(3600));
                        let app_tick = app_handle.clone();
                        tauri::async_runtime::spawn(async move {
                            check_for_updates(app_tick, false, true).await;
                        });
                    }
                });
            }

            // 预创建通知卡片窗口（隐藏状态）：等到第一次来通知才建的话，
            // WebView 初始化会让卡片延迟几百毫秒才出现；提前建好，通知来时
            // 直接 show() 就能秒出。
            let _ = ensure_notify_popup(app.handle());

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
