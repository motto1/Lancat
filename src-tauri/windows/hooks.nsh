; Lancat NSIS installer hooks (bundle.windows.nsis.installerHooks in tauri.conf.json).
;
; Windows Firewall: peers reach Lancat on TCP 47820 (HTTP/WebSocket) and UDP 47820 (discovery
; probes) / 5353 (mDNS). Without an inbound rule Windows asks on first launch; when that prompt is
; dismissed (or answered while on a "Public" network) Windows silently drops inbound connections:
; the device still dials others, so texts work, but nobody can connect to it.
; The installer runs per user (no admin rights), so the rules are added through one elevated
; cmd.exe (a single UAC prompt; declining it just skips the rules). Old rules for this exe,
; including "Block" rules left by a dismissed prompt (block wins over allow), are removed first.
; All network profiles (private, public, domain): Lancat must work on any network; switching a
; trusted network to Private is only recommended (diagnostics page).

!macro LANCAT_FW_DELETE
  ExecShellWait "runas" "$SYSDIR\cmd.exe" '/c netsh advfirewall firewall delete rule name=all program="$INSTDIR\${MAINBINARYNAME}.exe"' SW_HIDE
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ExecShellWait "runas" "$SYSDIR\cmd.exe" '/c netsh advfirewall firewall delete rule name=all program="$INSTDIR\${MAINBINARYNAME}.exe" & netsh advfirewall firewall add rule name="Lancat" dir=in action=allow program="$INSTDIR\${MAINBINARYNAME}.exe" protocol=TCP localport=47820 profile=any & netsh advfirewall firewall add rule name="Lancat" dir=in action=allow program="$INSTDIR\${MAINBINARYNAME}.exe" protocol=UDP localport=47820,5353 profile=any' SW_HIDE
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro LANCAT_FW_DELETE
!macroend
