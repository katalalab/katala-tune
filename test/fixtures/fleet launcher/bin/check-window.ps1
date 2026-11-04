Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class FleetConsole {
    [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow();
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr handle);
}
'@
if ([FleetConsole]::IsWindowVisible([FleetConsole]::GetConsoleWindow())) { 'console=visible' } else { 'console=hidden' }
