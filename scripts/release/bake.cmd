@echo off
if not defined DSH_HOME set "DSH_HOME=%USERPROFILE%\.bake"
rem Node writes the runtime watchdog's fatal-error reports here, without environment variables.
if not exist "%DSH_HOME%\diagnostics\" mkdir "%DSH_HOME%\diagnostics" 2>nul
set "BAKE_CLI=%~dp0..\apps\cli\lib\bin.js"
if /I "%~1"=="tui" goto raw
if /I "%~1"=="headless" goto raw
if /I "%~1"=="plugin" goto raw
if /I "%~1"=="update" goto raw
if /I "%~1"=="--profile" goto raw
node --report-exclude-env --report-exclude-network "--diagnostic-dir=%DSH_HOME%\diagnostics" "%BAKE_CLI%" --profile tui %*
exit /b %ERRORLEVEL%
:raw
node --report-exclude-env --report-exclude-network "--diagnostic-dir=%DSH_HOME%\diagnostics" "%BAKE_CLI%" %*
exit /b %ERRORLEVEL%
