; "Open in RCode" shell verbs for folders, folder backgrounds, and drives.
; HKCU matches installer currentUser scope. %V = clicked path.
; NoWorkingDirectory keeps Explorer from overriding %V (System32 on Drive).

!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr HKCU "Software\Classes\Directory\shell\OpenInRCode" "" "Open in RCode"
  WriteRegStr HKCU "Software\Classes\Directory\shell\OpenInRCode" "Icon" '"$INSTDIR\rcode.exe",0'
  WriteRegStr HKCU "Software\Classes\Directory\shell\OpenInRCode" "NoWorkingDirectory" ""
  WriteRegStr HKCU "Software\Classes\Directory\shell\OpenInRCode\command" "" '"$INSTDIR\rcode.exe" "%V"'

  WriteRegStr HKCU "Software\Classes\Directory\Background\shell\OpenInRCode" "" "Open in RCode"
  WriteRegStr HKCU "Software\Classes\Directory\Background\shell\OpenInRCode" "Icon" '"$INSTDIR\rcode.exe",0'
  WriteRegStr HKCU "Software\Classes\Directory\Background\shell\OpenInRCode" "NoWorkingDirectory" ""
  WriteRegStr HKCU "Software\Classes\Directory\Background\shell\OpenInRCode\command" "" '"$INSTDIR\rcode.exe" "%V"'

  WriteRegStr HKCU "Software\Classes\Drive\shell\OpenInRCode" "" "Open in RCode"
  WriteRegStr HKCU "Software\Classes\Drive\shell\OpenInRCode" "Icon" '"$INSTDIR\rcode.exe",0'
  WriteRegStr HKCU "Software\Classes\Drive\shell\OpenInRCode" "NoWorkingDirectory" ""
  WriteRegStr HKCU "Software\Classes\Drive\shell\OpenInRCode\command" "" '"$INSTDIR\rcode.exe" "%V"'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegKey HKCU "Software\Classes\Directory\shell\OpenInRCode"
  DeleteRegKey HKCU "Software\Classes\Directory\Background\shell\OpenInRCode"
  DeleteRegKey HKCU "Software\Classes\Drive\shell\OpenInRCode"
!macroend
