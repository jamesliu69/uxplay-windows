using System.Text.Json;
using System.Text.Json.Serialization;

namespace UxPlayRs.Gui.Ipc;

/// <summary>
/// JSON options matching the Rust engine contract: camelCase, case-insensitive.
/// </summary>
public static class EngineJson
{
    public static readonly JsonSerializerOptions Options = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        PropertyNameCaseInsensitive = true,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };
}

/// <summary>Engine start parameters (mirrors airplay-ipc EngineParams).</summary>
public sealed class EngineParams
{
    public string Name { get; set; } = "uxplay-rs";
    public string Resolution { get; set; } = "1920x1080";
    public uint MaxFps { get; set; } = 30;
    public bool AudioOnly { get; set; } = false;
    public List<string> ExtraArgs { get; set; } = new();

    public EngineParams Clone() => new()
    {
        Name = Name,
        Resolution = Resolution,
        MaxFps = MaxFps,
        AudioOnly = AudioOnly,
        ExtraArgs = new List<string>(ExtraArgs),
    };
}

/// <summary>Engine status snapshot (mirrors airplay-ipc StatusResponse).</summary>
public sealed class StatusResponse
{
    public bool Running { get; set; }
    public string State { get; set; } = "idle";
    public string Detail { get; set; } = string.Empty;
    public EngineParams Params { get; set; } = new();
}
