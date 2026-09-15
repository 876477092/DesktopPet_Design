; =============================================================================
; DesktopPet · NSIS 安装器定制钩子（S6-M3，T-17）
;
; 通过 `tauri.conf.json` 的 `bundle.windows.nsis.installerHooks` 接入官方
; installer.nsi 模板（Tauri 2 支持：NSIS_HOOK_PREINSTALL / NSIS_HOOK_POSTINSTALL /
; NSIS_HOOK_PREUNINSTALL / NSIS_HOOK_POSTUNINSTALL，模板对应位置 `!insertmacro`）。
;
; 本文件只定义宏，不做顶层逻辑（顶层代码在 `!include` 处编译，早于模板
; `!define` 段，禁止引用 ${PRODUCTNAME}/${BUNDLEID} 等——宏体在插入点展开，
; 插入点在 Section 内，晚于全部 `!define`，故宏体内可安全引用）。
;
; 职责：
;   1. 精简包（webviewInstallMode=skip）安装前检测 WebView2 注册表，
;      缺失时提示用户（不阻断安装，运行时兜底；离线包安装器自带运行时，跳过检测）；
;   2. 卸载保留存档（`02 §2.4` / 03 S6-M3 卡）：卸载前把存档目录备份到临时目录，
;      卸载完成后恢复——无论用户是否勾选「删除应用数据」，`%APPDATA%\com.desktoppet.app`
;      下的用户存档都**不会被卸载器删除**（产品约定：删除存档走设置页「数据」Tab）。
; =============================================================================

!ifndef DESKTOPPET_INSTALLER_NSH
!define DESKTOPPET_INSTALLER_NSH

; ---------------------------------------------------------------------------
; 安装前钩子
; ---------------------------------------------------------------------------
!macro NSIS_HOOK_PREINSTALL
  ; S6-M3 卡要点 1：精简包依赖系统 WebView2——安装前检测注册表，缺失提示。
  ; 离线包（INSTALLWEBVIEW2MODE=offlineInstaller）由安装器负责装运行时，跳过检测。
  !if "${INSTALLWEBVIEW2MODE}" == "skip"
    ReadRegStr $R0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
    ${If} $R0 == ""
      ReadRegStr $R0 HKCU "SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
    ${EndIf}
    ${If} $R0 == ""
      MessageBox MB_ICONINFORMATION|MB_YESNO "未检测到 Microsoft Edge WebView2 运行时。精简安装包依赖系统 WebView2（Windows 10/11 通常已内置）。是否仍继续安装？" IDYES +2
      Abort
    ${EndIf}
    DetailPrint "已检测到系统 WebView2 运行时（版本 $R0）。"
  !endif
!macroend

; ---------------------------------------------------------------------------
; 卸载前钩子：备份存档（S6-M3「卸载保留存档」）
; ---------------------------------------------------------------------------
!macro NSIS_HOOK_PREUNINSTALL
  ; 更新 / 无存档 → 跳过（UpdateMode=1 时不卸载数据）。
  ${If} $UpdateMode = 1
    Goto desktoppet_preuninstall_done
  ${EndIf}
  ${IfNot} ${FileExists} "$APPDATA\${BUNDLEID}\save\*.*"
    Goto desktoppet_preuninstall_done
  ${EndIf}
  RMDir /r "$TEMP\DesktopPet-save-backup"
  CreateDirectory "$TEMP\DesktopPet-save-backup"
  CopyFiles /SILENT "$APPDATA\${BUNDLEID}\save" "$TEMP\DesktopPet-save-backup"
  DetailPrint "已备份桌面宠物存档 → $TEMP\DesktopPet-save-backup"
  desktoppet_preuninstall_done:
!macroend

; ---------------------------------------------------------------------------
; 卸载后钩子：恢复存档（无论卸载器是否删除了应用数据，存档都保留）
; ---------------------------------------------------------------------------
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode = 1
    Goto desktoppet_postuninstall_done
  ${EndIf}
  ${IfNot} ${FileExists} "$TEMP\DesktopPet-save-backup\save\*.*"
    Goto desktoppet_postuninstall_done
  ${EndIf}
  CreateDirectory "$APPDATA\${BUNDLEID}"
  CopyFiles /SILENT "$TEMP\DesktopPet-save-backup\save" "$APPDATA\${BUNDLEID}"
  RMDir /r "$TEMP\DesktopPet-save-backup"
  DetailPrint "桌面宠物存档已保留：$APPDATA\${BUNDLEID}\save（卸载不删档）"
  desktoppet_postuninstall_done:
!macroend

!endif ; DESKTOPPET_INSTALLER_NSH
