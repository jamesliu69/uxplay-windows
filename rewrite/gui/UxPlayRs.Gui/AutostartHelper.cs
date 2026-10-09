using Microsoft.Win32;
using System.IO;

namespace UxPlayRs.Gui;

/// <summary>Run-at-login via HKCU\...\Run.</summary>
public static class AutostartHelper
{
    private const string RunKey = @"Software\Microsoft\Windows\CurrentVersion\Run";
    private const string ValueName = "UxPlayRs";

    private static string ExePath =>
        Environment.ProcessPath ?? System.Reflection.Assembly.GetExecutingAssembly().Location;

    public static bool IsEnabled()
    {
        using var key = Registry.CurrentUser.OpenSubKey(RunKey, writable: false);
        var value = key?.GetValue(ValueName) as string;
        return value is not null && value.Contains(Path.GetFileName(ExePath), StringComparison.OrdinalIgnoreCase);
    }

    public static void SetEnabled(bool enabled)
    {
        using var key = Registry.CurrentUser.OpenSubKey(RunKey, writable: true)
            ?? Registry.CurrentUser.CreateSubKey(RunKey);
        if (key is null)
            throw new InvalidOperationException("Cannot open Run key");
        if (enabled)
            key.SetValue(ValueName, $"\"{ExePath}\" --minimized");
        else
            key.DeleteValue(ValueName, throwOnMissingValue: false);
    }
}
