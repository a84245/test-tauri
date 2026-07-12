fn main() {
    // 为应用自定义命令生成 ACL 权限，使其可在 capability 中被授权。
    // 关键：主窗口通过 WebviewUrl::External 加载「远程 URL」，
    // 远程内容默认无权访问任何 IPC/命令；必须显式为自定义命令生成权限，
    // 否则前端 invoke('notify') 会被 ACL 拒绝（notify not allowed. Plugin not found）。
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(
            tauri_build::AppManifest::new().commands(&["notify"]),
        ),
    )
    .expect("failed to run tauri-build");
}
