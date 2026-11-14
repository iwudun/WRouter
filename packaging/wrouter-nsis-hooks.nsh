; WRouter NSIS 安装器自定义钩子(20261102_01 P5-2)
;
; 由 desktop/src-tauri/tauri.conf.json 的
; `bundle.windows.nsis.installerHooks` 引用。
;
; 背景: Tauri 的 NSIS 模板默认只建**开始菜单**快捷方式,不建桌面快捷方式;
; 而退场的 Electron 侧 electron-builder NSIS 默认是建桌面快捷方式的
; (electron-builder 的 createDesktopShortcut 默认 true)。
; 这里补一个桌面快捷方式,保持用户体感一致;卸载时一并删掉。
;
; 可用宏: NSIS_HOOK_PREINSTALL / NSIS_HOOK_POSTINSTALL /
;          NSIS_HOOK_PREUNINSTALL / NSIS_HOOK_POSTUNINSTALL

!macro NSIS_HOOK_POSTINSTALL
  ; $INSTDIR 下是 Tauri 装好的主程序(Cargo 包名 wrouter-desktop → wrouter-desktop.exe)
  CreateShortCut "$DESKTOP\WRouter.lnk" "$INSTDIR\wrouter-desktop.exe"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Delete "$DESKTOP\WRouter.lnk"
!macroend
