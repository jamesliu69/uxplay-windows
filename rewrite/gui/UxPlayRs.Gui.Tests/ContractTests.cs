using System.Text.Json;
using UxPlayRs.Gui.Ipc;
using UxPlayRs.Gui.Settings;

namespace UxPlayRs.Gui.Tests;

public class ProtocolTests
{
    [Fact]
    public void EngineParams_SerializesCamelCase_WithDefaults()
    {
        var json = JsonSerializer.Serialize(new EngineParams(), EngineJson.Options);
        Assert.Contains("\"resolution\":\"1920x1080\"", json);
        Assert.Contains("\"maxFps\":30", json);
        Assert.Contains("\"audioOnly\":false", json);
        Assert.Contains("\"name\":\"uxplay-rs\"", json);
        Assert.Contains("\"extraArgs\":[]", json);
    }

    [Fact]
    public void EngineParams_AcceptsPartialJson()
    {
        var p = JsonSerializer.Deserialize<EngineParams>("{\"name\":\"My TV\"}", EngineJson.Options)!;
        Assert.Equal("My TV", p.Name);
        Assert.Equal(30u, p.MaxFps);
        Assert.Equal("1920x1080", p.Resolution);
    }

    [Fact]
    public void StatusResponse_DeserializesEnginePayload()
    {
        const string payload =
            "{\"running\":true,\"state\":\"advertising\",\"detail\":\"\",\"params\":{\"name\":\"TV\",\"resolution\":\"1280x720\",\"maxFps\":60,\"audioOnly\":false,\"extraArgs\":[]}}";
        var s = JsonSerializer.Deserialize<StatusResponse>(payload, EngineJson.Options)!;
        Assert.True(s.Running);
        Assert.Equal("advertising", s.State);
        Assert.Equal("1280x720", s.Params.Resolution);
        Assert.Equal(60u, s.Params.MaxFps);
    }

    [Fact]
    public void Controller_Start_UsesTypedContract()
    {
        // anonymous-type wrapper must serialize the "params" envelope
        var json = JsonSerializer.Serialize(new { @params = new EngineParams() }, EngineJson.Options);
        Assert.Contains("\"params\":{\"name\":\"uxplay-rs\"", json);
    }
}

public class SettingsTests : IDisposable
{
    private readonly string _path = Path.Combine(Path.GetTempPath(), $"uxplay-rs-test-{Guid.NewGuid()}.json");

    public void Dispose()
    {
        try { File.Delete(_path); } catch { }
    }

    [Fact]
    public void SaveLoad_RoundTrip()
    {
        var s = new AppSettings { DeviceName = "Living Room", Resolution = "1280x720", MaxFps = 60, AudioOnly = true };
        s.Save(_path);
        var back = AppSettings.Load(_path);
        Assert.Equal("Living Room", back.DeviceName);
        Assert.Equal("1280x720", back.Resolution);
        Assert.Equal(60u, back.MaxFps);
        Assert.True(back.AudioOnly);
    }

    [Fact]
    public void MissingFile_GivesDefaults()
    {
        var s = AppSettings.Load(_path);
        Assert.Equal("uxplay-rs", s.DeviceName);
        Assert.Equal(30u, s.MaxFps);
    }

    [Fact]
    public void ToEngineParams_MapsFields()
    {
        var s = new AppSettings { DeviceName = "TV", Resolution = "640x480", MaxFps = 30, AudioOnly = true };
        var p = s.ToEngineParams();
        Assert.Equal("TV", p.Name);
        Assert.Equal("640x480", p.Resolution);
        Assert.True(p.AudioOnly);
    }
}

public class RingBufferTests
{
    [Fact]
    public void OldestEntries_EvictedBeyondCapacity()
    {
        var buf = new LogRingBuffer(capacity: 3);
        for (var i = 0; i < 5; i++)
            buf.Add("info", $"m{i}");
        Assert.Equal(3, buf.Count);
        var snap = buf.Snapshot();
        Assert.Equal("m2", snap[0].Message);
        Assert.Equal("m4", snap[2].Message);
    }
}
