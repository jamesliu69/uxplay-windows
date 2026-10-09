using System.IO;
using System.Text.Json;
using UxPlayRs.Gui.Ipc;

namespace UxPlayRs.Gui.Settings;

/// <summary>Persisted GUI settings (%APPDATA%\uxplay-rs\settings.json).</summary>
public sealed class AppSettings
{
    public string DeviceName { get; set; } = "uxplay-rs";
    public string Resolution { get; set; } = "1920x1080";
    public uint MaxFps { get; set; } = 30;
    public bool AudioOnly { get; set; } = false;
    public bool RunAtLogin { get; set; } = false;

    public EngineParams ToEngineParams() => new()
    {
        Name = DeviceName,
        Resolution = Resolution,
        MaxFps = MaxFps,
        AudioOnly = AudioOnly,
    };

    public static string DefaultPath
    {
        get
        {
            var dir = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "uxplay-rs");
            return Path.Combine(dir, "settings.json");
        }
    }

    public static AppSettings Load(string? path = null)
    {
        path ??= DefaultPath;
        try
        {
            if (File.Exists(path))
            {
                var json = File.ReadAllText(path);
                return JsonSerializer.Deserialize<AppSettings>(json, EngineJson.Options) ?? new AppSettings();
            }
        }
        catch { /* corrupt file: fall back to defaults */ }
        return new AppSettings();
    }

    public void Save(string? path = null)
    {
        path ??= DefaultPath;
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllText(path, JsonSerializer.Serialize(this, EngineJson.Options));
    }
}

/// <summary>Thread-safe fixed-capacity log ring buffer.</summary>
public sealed class LogRingBuffer
{
    private readonly Queue<(DateTime Time, string Level, string Message)> _queue = new();
    private readonly object _lock = new();

    public int Capacity { get; }

    public LogRingBuffer(int capacity = 1000)
    {
        Capacity = capacity;
    }

    public void Add(string level, string message)
    {
        lock (_lock)
        {
            _queue.Enqueue((DateTime.Now, level, message));
            while (_queue.Count > Capacity)
                _queue.Dequeue();
        }
    }

    public IReadOnlyList<(DateTime Time, string Level, string Message)> Snapshot()
    {
        lock (_lock)
        {
            return _queue.ToList();
        }
    }

    public int Count
    {
        get { lock (_lock) { return _queue.Count; } }
    }
}
