@echo off
if not defined DSH_HOME set "DSH_HOME=%USERPROFILE%\.bake"
set "BAKE_CLI=%~dp0..\apps\cli\lib\bin.js"
if /I "%~1"=="tui" goto raw
if /I "%~1"=="headless" goto raw
if /I "%~1"=="plugin" goto raw
if /I "%~1"=="--profile" goto raw
node "%BAKE_CLI%" --profile tui %*
exit /b %ERRORLEVEL%
:raw
node "%BAKE_CLI%" %*
exit /b %ERRORLEVEL%
