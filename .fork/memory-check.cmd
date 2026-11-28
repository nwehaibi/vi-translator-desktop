@echo off
rem fork: your translation memory in the original app and in this copy (see FORK.md: memory equivalence).
rem Run it from the folder it came in (the fork build's "measurement tools"), with the app closed.
rem Your folders are only read: the memory is loaded from a temporary copy, and the results keep the
rem translations as fingerprints only (no translation is written out).
rem
rem The original's approximate lookup (a misreading: two words run together, a letter lost) is not the
rem same from one run to the next (memory.rs: at most 600 candidates, in a random order; the first of
rem equally close lines wins). So each core is run twice: the original against itself shows how much
rem that alone changes; the original against this copy must differ no more, and never in the lines the
rem memory has (exact lookups), the counts, or what the memory file keeps.
rem   memory-check.cmd                     your translation memory and the app's data folder
rem   memory-check.cmd "<db>" "<data>"     other folders (copies, for instance)
setlocal
set "HERE=%~dp0"
for /f "usebackq delims=" %%D in (`powershell -NoProfile -Command "[Environment]::GetFolderPath('MyDocuments')"`) do set "DOCS=%%D"
set "DB=%DOCS%\VI Translator\translation-db"
set "DATA=%APPDATA%\com.vitranslator.app"
if not "%~1"=="" set "DB=%~1"
if not "%~2"=="" set "DATA=%~2"
set "O=%HERE%fork_memory_equivalence-original.exe"
set "F=%HERE%fork_memory_equivalence-fork.exe"
set "SUM=%HERE%memory-check-summary.txt"
set "DET=%HERE%memory-check-details.txt"
set "BAD=0"
echo Translation memory: %DB%
echo App data:           %DATA%
echo.
if exist "%SUM%" del "%SUM%"
if exist "%DET%" del "%DET%"
rem (an earlier check's two files, if they are here: compared again, by kind)
if exist "%HERE%memory-original.jsonl" if exist "%HERE%memory-fork.jsonl" (
  "%F%" compare "%HERE%memory-original.jsonl" "%HERE%memory-fork.jsonl" --label "earlier check: original vs this copy" --summary-to "%SUM%" 2>> "%DET%"
  if errorlevel 2 set "BAD=1"
  echo.
)
echo [1/4] the original, first run
"%O%" dump --db "%DB%" --data "%DATA%" --out "%HERE%memory-original-1.jsonl" || goto :failed
echo [2/4] the original, second run
"%O%" dump --db "%DB%" --data "%DATA%" --out "%HERE%memory-original-2.jsonl" || goto :failed
echo [3/4] this copy, first run
"%F%" dump --db "%DB%" --data "%DATA%" --out "%HERE%memory-fork-1.jsonl" || goto :failed
echo [4/4] this copy, second run
"%F%" dump --db "%DB%" --data "%DATA%" --out "%HERE%memory-fork-2.jsonl" || goto :failed
echo.
"%F%" compare "%HERE%memory-original-1.jsonl" "%HERE%memory-original-2.jsonl" --label "original vs original" --summary-to "%SUM%" 2>> "%DET%"
if errorlevel 2 set "BAD=1"
"%F%" compare "%HERE%memory-fork-1.jsonl" "%HERE%memory-fork-2.jsonl" --label "this copy vs this copy" --summary-to "%SUM%" 2>> "%DET%"
if errorlevel 2 set "BAD=1"
"%F%" compare "%HERE%memory-original-1.jsonl" "%HERE%memory-fork-1.jsonl" --label "original vs this copy (1)" --summary-to "%SUM%" 2>> "%DET%"
if errorlevel 2 set "BAD=1"
"%F%" compare "%HERE%memory-original-2.jsonl" "%HERE%memory-fork-2.jsonl" --label "original vs this copy (2)" --summary-to "%SUM%" 2>> "%DET%"
if errorlevel 2 set "BAD=1"
cls
echo ===================== SUMMARY =====================
type "%SUM%"
echo ===================================================
echo (each comparison's examples: memory-check-details.txt)
echo.
if "%BAD%"=="1" goto :different
echo RESULT: KEYS, SOURCES, COUNTS AND FILE THE SAME in every comparison.
echo         Misreadings: compare "original vs original" with "original vs this copy" above.
pause
exit /b 0
:different
echo RESULT: DIFFERENT in keys, counts or file (see the summary above and memory-check-summary.txt)
pause
exit /b 1
:failed
echo.
echo FAILED (see above)
pause
exit /b 2
