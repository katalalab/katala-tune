export MSYS2_ARG_CONV_EXCL="*"
cd ~/.katala-tune
rm -rf st && mkdir st
cat > st/run.bat <<'BAT'
@echo off
set T=%USERPROFILE%\.katala-tune\st
"%USERPROFILE%\.katala-tune\bin\katala-tune-gpui.exe" --selftest --config "%USERPROFILE%\.config\katala-tune\nodes.json" --data "%T%\data" > "%T%\out.txt" 2>&1
BAT
U=$(whoami | sed 's#.*/##')
schtasks /Create /TN KtSelf /TR "$(cygpath -w ~/.katala-tune/st/run.bat)" /SC ONCE /ST 00:00 /IT /F >/dev/null 2>&1
schtasks /Run /TN KtSelf >/dev/null 2>&1
for i in $(seq 1 30); do sleep 2; [ -s st/out.txt ] && break; done
cat st/out.txt; schtasks /Delete /TN KtSelf /F >/dev/null 2>&1; rm -rf st
