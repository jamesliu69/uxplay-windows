using System.Diagnostics;
using System.IO;

namespace UxPlayRs.Gui;

/// <summary>
/// Finds, starts and stops airplayd.exe. Engine stdout/stderr lines are
/// forwarded to <see cref="EngineOutput"/>.
/// </summary>
public sealed class EngineProcessManager : IDisposable
{
    public event Action<string>? EngineOutput;

    private Process? _process;
    private bool _disposed;

    /// <summary>Locate airplayd.exe: next to the GUI, else the dev-built engine.</summary>
    public static string? FindEngineExe()
    {
        var nextToGui = Path.Combine(AppContext.BaseDirectory, "airplayd.exe");
        if (File.Exists(nextToGui))
            return nextToGui;
        // Dev loop: rewrite/engine/target/{release,debug}/airplayd.exe
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        for (var i = 0; i < 8 && dir is not null; i++, dir = dir.Parent)
        {
            foreach (var cfg in new[] { "release", "debug" })
            {
                var candidate = Path.Combine(dir.FullName, "engine", "target", cfg, "airplayd.exe");
                if (File.Exists(candidate))
                    return candidate;
            }
        }
        return null;
    }

    public bool IsRunning => _process is { HasExited: false };

    public void Start(string? exePath = null)
    {
        if (IsRunning)
            return;
        exePath ??= FindEngineExe() ?? throw new FileNotFoundException("airplayd.exe not found");
        _process = new Process
        {
            StartInfo = new ProcessStartInfo
            {
                FileName = exePath,
                UseShellExecute = false,
                CreateNoWindow = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            },
            EnableRaisingEvents = true,
        };
        _process.OutputDataReceived += (_, e) => { if (e.Data is not null) EngineOutput?.Invoke(e.Data); };
        _process.ErrorDataReceived += (_, e) => { if (e.Data is not null) EngineOutput?.Invoke(e.Data); };
        _process.Start();
        _process.BeginOutputReadLine();
        _process.BeginErrorReadLine();
    }

    public void Stop()
    {
        var p = _process;
        _process = null;
        if (p is null)
            return;
        try
        {
            if (!p.HasExited)
            {
                p.Kill(entireProcessTree: true);
                p.WaitForExit(3000);
            }
        }
        catch { /* already gone */ }
        finally
        {
            p.Dispose();
        }
    }

    public void Dispose()
    {
        if (_disposed)
            return;
        _disposed = true;
        Stop();
    }
}
