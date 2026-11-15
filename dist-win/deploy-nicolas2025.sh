#!/usr/bin/env bash
# nicolas2025 が起動したら実行する: PR #146 の Windows ビルドを配備し、selftest する。
set -euo pipefail
here=$(dirname "$(realpath "$0")")
ssh -o ConnectTimeout=10 nicolas2025 echo up
scp -q "$here/katala-tune-gpui.exe" nicolas2025:.katala-tune/bin/katala-tune-gpui.exe.new
ssh nicolas2025 'cd ~/.katala-tune/bin && mv -f katala-tune-gpui.exe.new katala-tune-gpui.exe && sha256sum katala-tune-gpui.exe'
ssh nicolas2025 'bash -s' < "$here/selftest.sh"
