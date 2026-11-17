!macro NSIS_HOOK_PREINSTALL
  nsExec::ExecToStack 'taskkill /F /T /IM dynamic-island.exe'
  Sleep 350
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToStack 'taskkill /F /T /IM dynamic-island.exe'
  Sleep 350
!macroend
