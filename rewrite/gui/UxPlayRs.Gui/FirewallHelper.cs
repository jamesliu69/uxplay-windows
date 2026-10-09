using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;

namespace UxPlayRs.Gui;

/// <summary>Manages the Windows Defender Firewall rule required by AirPlay.</summary>
internal static class FirewallHelper
{
    private const string RuleName = "UxPlayRs AirPlay Receiver";
    private const int DirectionInbound = 1;
    private const int ActionAllow = 1;
    private const int ProfilePrivate = 2;
    private const int ProtocolAny = 256;
    private const int UserCancelledUac = 1223;

    internal enum SetupResult
    {
        AlreadyConfigured,
        Configured,
        Cancelled,
        Failed,
    }

    /// <summary>
    /// Ensures the AirPlay engine can receive mDNS, RTSP, and dynamically
    /// negotiated media connections on trusted private networks.
    /// </summary>
    public static async Task<SetupResult> EnsurePrivateInboundRuleAsync(string enginePath)
    {
        var fullEnginePath = Path.GetFullPath(enginePath);
        if (HasPrivateInboundRule(fullEnginePath))
            return SetupResult.AlreadyConfigured;

        var guiPath = Environment.ProcessPath;
        if (string.IsNullOrWhiteSpace(guiPath) || !File.Exists(guiPath))
            return SetupResult.Failed;

        var startInfo = new ProcessStartInfo
        {
            FileName = guiPath,
            Arguments = $"--configure-firewall \"{fullEnginePath}\"",
            UseShellExecute = true,
            Verb = "runas",
            WindowStyle = ProcessWindowStyle.Hidden,
        };

        try
        {
            using var helper = Process.Start(startInfo);
            if (helper is null)
                return SetupResult.Failed;

            await helper.WaitForExitAsync().ConfigureAwait(false);
            return helper.ExitCode == 0 && HasPrivateInboundRule(fullEnginePath)
                ? SetupResult.Configured
                : SetupResult.Failed;
        }
        catch (Win32Exception ex) when (ex.NativeErrorCode == UserCancelledUac)
        {
            return SetupResult.Cancelled;
        }
    }

    /// <summary>
    /// Handles the elevated helper invocation. Returns false for a normal GUI
    /// launch; otherwise configures the rule and returns the helper exit code.
    /// </summary>
    public static bool TryHandleConfigurationMode(string[] args, out int exitCode)
    {
        exitCode = 0;
        if (args.Length != 2 || !string.Equals(args[0], "--configure-firewall", StringComparison.Ordinal))
            return false;

        try
        {
            ConfigurePrivateInboundRule(Path.GetFullPath(args[1]));
            exitCode = 0;
        }
        catch
        {
            exitCode = 1;
        }

        return true;
    }

    private static bool HasPrivateInboundRule(string enginePath)
    {
        object? policyObject = null;
        object? rulesObject = null;
        try
        {
            var policyType = Type.GetTypeFromProgID("HNetCfg.FwPolicy2", throwOnError: false);
            if (policyType is null)
                return false;

            policyObject = Activator.CreateInstance(policyType);
            if (policyObject is null)
                return false;

            dynamic policy = policyObject;
            rulesObject = policy.Rules;
            dynamic rules = rulesObject;

            foreach (object ruleObject in rules)
            {
                try
                {
                    dynamic rule = ruleObject;
                    var applicationName = (string?)rule.ApplicationName;
                    if (string.Equals((string?)rule.Name, RuleName, StringComparison.Ordinal)
                        && (bool)rule.Enabled
                        && (int)rule.Direction == DirectionInbound
                        && (int)rule.Action == ActionAllow
                        && ((int)rule.Profiles & ProfilePrivate) != 0
                        && string.Equals(applicationName, enginePath, StringComparison.OrdinalIgnoreCase))
                    {
                        return true;
                    }
                }
                finally
                {
                    if (Marshal.IsComObject(ruleObject))
                        Marshal.FinalReleaseComObject(ruleObject);
                }
            }
        }
        catch (COMException)
        {
            return false;
        }
        finally
        {
            ReleaseComObject(rulesObject);
            ReleaseComObject(policyObject);
        }

        return false;
    }

    private static void ConfigurePrivateInboundRule(string enginePath)
    {
        if (!File.Exists(enginePath))
            throw new FileNotFoundException("airplayd.exe not found", enginePath);

        var policyType = Type.GetTypeFromProgID("HNetCfg.FwPolicy2", throwOnError: true)!;
        var ruleType = Type.GetTypeFromProgID("HNetCfg.FWRule", throwOnError: true)!;
        object? policyObject = null;
        object? rulesObject = null;
        object? ruleObject = null;
        try
        {
            policyObject = Activator.CreateInstance(policyType)
                ?? throw new InvalidOperationException("Could not create Windows Firewall policy object.");
            dynamic policy = policyObject;
            rulesObject = policy.Rules;
            dynamic rules = rulesObject;

            try { rules.Remove(RuleName); }
            catch (COMException) { }

            ruleObject = Activator.CreateInstance(ruleType)
                ?? throw new InvalidOperationException("Could not create Windows Firewall rule object.");
            dynamic rule = ruleObject;
            rule.Name = RuleName;
            rule.Description = "Allows AirPlay discovery, control, and mirroring traffic for UxPlayRs.";
            rule.ApplicationName = enginePath;
            rule.Protocol = ProtocolAny;
            rule.Direction = DirectionInbound;
            rule.Action = ActionAllow;
            rule.Profiles = ProfilePrivate;
            rule.Enabled = true;
            rules.Add(rule);
        }
        finally
        {
            ReleaseComObject(ruleObject);
            ReleaseComObject(rulesObject);
            ReleaseComObject(policyObject);
        }
    }

    private static void ReleaseComObject(object? value)
    {
        if (value is not null && Marshal.IsComObject(value))
            Marshal.FinalReleaseComObject(value);
    }
}
