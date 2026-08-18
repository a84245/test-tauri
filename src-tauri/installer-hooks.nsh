; Tauri installerHooks — 安装前检测已安装，提示覆盖并清理旧安装目录
; 支持宏：NSIS_HOOK_PREINSTALL / NSIS_HOOK_POSTINSTALL

; 安装前：检测是否已安装 → 提示覆盖
!macro NSIS_HOOK_PREINSTALL
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
!macroend

; 安装完成后：如果旧安装目录不同于当前目录，删除旧目录防止多处安装
!macro NSIS_HOOK_POSTINSTALL
  ${If} $R1 != ""
  ${AndIf} $R1 != "$INSTDIR"
    RMDir /r "$R1"
  ${EndIf}
!macroend
