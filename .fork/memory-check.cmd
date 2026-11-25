@echo off
rem fork: your translation memory answering the same in the original app and in this copy (see FORK.md).
rem Run it from the folder it came in (the fork build's "measurement tools"), with the app closed.
rem Your folders are only read: the memory is loaded from a temporary copy, and the results keep the
rem translations as fingerprints only (no translation is written out).
rem   memory-check.cmd                     your translation memory and the app's data folder
rem   memory-check.cmd "<db>" "<data>"     other folders (copies, for instance)
setlocal
set "HERE=%~dp0"
for /f "usebackq delims=" %%D in (`powershell -NoProfile -Command "[Environment]::GetFolderPath('MyDocuments')"`) do set "DOCS=%%D"
set "DB=%DOCS%\VI Translator\translation-db"
set "DATA=%APPDATA%\com.vitranslator.app"
if not "%~1"=="" set "DB=%~1"
if not "%~2"=="" set "DATA=%~2"
echo Translation memory: %DB%
echo App data:           %DATA%
echo.
"%HERE%fork_memory_equivalence-original.exe" dump --db "%DB%" --data "%DATA%" --out "%HERE%memory-original.jsonl" || goto :failed
"%HERE%fork_memory_equivalence-fork.exe" dump --db "%DB%" --data "%DATA%" --out "%HERE%memory-fork.jsonl" || goto :failed
"%HERE%fork_memory_equivalence-fork.exe" compare "%HERE%memory-original.jsonl" "%HERE%memory-fork.jsonl"
if errorlevel 1 goto :different
echo.
echo RESULT: IDENTICAL
pause
exit /b 0
:different
echo.
echo RESULT: DIFFERENT (see above)
pause
exit /b 1
:failed
echo.
echo FAILED (see above)
pause
exit /b 2
