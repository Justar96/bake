@echo off
if defined BAKE_HOME if "%BAKE_HOME: =%"=="" set "BAKE_HOME="
if not defined BAKE_HOME if defined DSH_HOME set "BAKE_HOME=%DSH_HOME%"
if not defined BAKE_HOME set "BAKE_HOME=%USERPROFILE%\.bake"
set "DSH_HOME=%BAKE_HOME%"
rem Node writes the runtime watchdog's fatal-error reports here, without environment variables.
if not exist "%BAKE_HOME%\diagnostics\" mkdir "%BAKE_HOME%\diagnostics" 2>nul
set "BAKE_CLI=%~dp0..\apps\cli\lib\bin.js"
if /I "%~1"=="tui" goto raw
if /I "%~1"=="headless" goto raw
if /I "%~1"=="plugin" goto raw
if /I "%~1"=="update" goto raw
if /I "%~1"=="--profile" goto raw
node --report-exclude-env --report-exclude-network "--diagnostic-dir=%BAKE_HOME%\diagnostics" "%BAKE_CLI%" --profile tui %*
exit /b %ERRORLEVEL%
:raw
node --report-exclude-env --report-exclude-network "--diagnostic-dir=%BAKE_HOME%\diagnostics" "%BAKE_CLI%" %*
exit /b %ERRORLEVEL%
