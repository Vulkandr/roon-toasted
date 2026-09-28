; This Source Code Form is subject to the terms of the Mozilla Public
; License, v. 2.0. If a copy of the MPL was not distributed with this
; file, You can obtain one at https://mozilla.org/MPL/2.0/.

; Roon: Toasted additions to Tauri's Windows installer
; (tauri.conf.json > bundle > windows > nsis > installerHooks).
;
; Tauri already handles the rest: the uninstaller removes the Start with
; Windows entry (named after the product, "Roon Toasted"), and offers a
; "Delete the application data" checkbox, unticked, for settings and pairing.

!macro NSIS_HOOK_POSTINSTALL
  ; Apps & Features shows the real name, colon included (file names, and so
  ; the product name, can't have one)
  WriteRegStr SHCTX "${UNINSTKEY}" "DisplayName" "Roon: Toasted"
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Remove the roon-toasted:// link the app registers at startup (kept when
  ; the uninstaller runs as part of an update)
  ${If} $UpdateMode <> 1
    DeleteRegKey HKCU "Software\Classes\roon-toasted"
  ${EndIf}
!macroend
