; Tauri installerHooks — 安装前检测已安装，提示覆盖并清理旧安装目录
; 支持宏：NSIS_HOOK_PREINSTALL / NSIS_HOOK_POSTINSTALL
;
; 注意：自动更新（官方 updater）会以「带 /UPDATE 参数」的方式运行本安装器，
; Tauri NSIS 模板在 .onInit 里已把该参数解析到 $UpdateMode（=1 表示更新）。
; 更新时旧版本替换、同目录覆盖都由模板负责，不能再弹「是否覆盖」确认框
; （否则会卡住无人值守更新），仅普通首次/覆盖安装才提示。

; 安装前：非更新模式才检测是否已安装 → 提示覆盖
!macro NSIS_HOOK_PREINSTALL
  ${If} $UpdateMode = 1
    ; 自动更新：跳过覆盖确认
    StrCpy $R1 ""
  ${Else}
    ; 读取卸载注册表里的安装位置（Tauri 默认写 HKCU）
    ReadRegStr $R0 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\@PRODUCT_NAME@" "InstallLocation"
    ${If} $R0 != ""
      MessageBox MB_YESNO|MB_ICONQUESTION \
        "检测到已安装 @PRODUCT_NAME@。$\n$\n点击「是」将覆盖旧版本（旧安装目录将被清理）。" \
        IDYES proceed IDNO abort_install
      abort_install:
        Abort
      proceed:
        StrCpy $R1 $R0
    ${EndIf}
  ${EndIf}
!macroend

; 安装完成后：如果旧安装目录不同于当前目录，删除旧目录防止多处安装
!macro NSIS_HOOK_POSTINSTALL
  ${If} $R1 != ""
  ${AndIf} $R1 != "$INSTDIR"
    RMDir /r "$R1"
  ${EndIf}
!macroend
