using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json.Nodes;
using UxPlayRs.Gui.Ipc;

namespace UxPlayRs.Gui.Tests;

/// <summary>Fake engine speaking NDJSON JSON-RPC over TCP loopback.</summary>
internal sealed class FakeEngine : IAsyncDisposable
{
    private readonly TcpListener _listener;
    private TcpClient? _serverSide;
    private StreamReader? _requests;
    private StreamWriter? _responses;

    public FakeEngine()
    {
        _listener = new TcpListener(IPAddress.Loopback, 0);
        _listener.Start();
        var port = ((IPEndPoint)_listener.LocalEndpoint).Port;

        var tcp = new TcpClient();
        tcp.Connect(IPAddress.Loopback, port);
        _serverSide = _listener.AcceptTcpClient();

        var stream = tcp.GetStream();
        Client = new IpcClient(
            new StreamReader(stream, new UTF8Encoding(encoderShouldEmitUTF8Identifier: false), leaveOpen: false),
            new StreamWriter(stream, new UTF8Encoding(encoderShouldEmitUTF8Identifier: false), leaveOpen: false)
            { AutoFlush = true, NewLine = "\n" });
    }

    public IpcClient Client { get; }

    public async Task<JsonObject> ReadRequestAsync()
    {
        _requests ??= new StreamReader(_serverSide!.GetStream(), Encoding.UTF8, leaveOpen: true);
        var line = await _requests.ReadLineAsync();
        Assert.NotNull(line);
        return (JsonObject)JsonNode.Parse(line!)!;
    }

    public async Task RespondAsync(long id, string stateJson)
    {
        _responses ??= new StreamWriter(_serverSide!.GetStream(), Encoding.UTF8, leaveOpen: true)
        { AutoFlush = true, NewLine = "\n" };
        await _responses.WriteLineAsync(
            "{\"jsonrpc\":\"2.0\",\"id\":" + id + ",\"result\":" + stateJson + "}");
    }

    public async Task NotifyAsync(string methodJson)
    {
        _responses ??= new StreamWriter(_serverSide!.GetStream(), Encoding.UTF8, leaveOpen: true)
        { AutoFlush = true, NewLine = "\n" };
        await _responses.WriteLineAsync(methodJson);
    }

    public async ValueTask DisposeAsync()
    {
        await Client.DisposeAsync();
        _requests?.Dispose();
        _responses?.Dispose();
        _serverSide?.Dispose();
        _listener.Stop();
    }
}

public class IpcTests
{
    private const string IdleStatus =
        "{\"running\":false,\"state\":\"idle\",\"detail\":\"\",\"params\":{\"name\":\"uxplay-rs\",\"resolution\":\"1920x1080\",\"maxFps\":30,\"audioOnly\":false,\"extraArgs\":[]}}";

    [Fact]
    public async Task Status_RoundTrip()
    {
        await using var engine = new FakeEngine();
        await engine.Client.ConnectAsync();
        var invoke = engine.Client.InvokeAsync("status");

        var req = await engine.ReadRequestAsync();
        Assert.Equal("status", req["method"]?.GetValue<string>());
        var id = req["id"]!.GetValue<long>();
        await engine.RespondAsync(id, IdleStatus);

        var result = await invoke;
        Assert.Equal("idle", result["state"]?.GetValue<string>());
        Assert.False(result["running"]!.GetValue<bool>());
    }

    [Fact]
    public async Task Start_SendsCamelCaseParams()
    {
        await using var engine = new FakeEngine();
        await engine.Client.ConnectAsync();
        var invoke = engine.Client.InvokeAsync("start", new
        {
            @params = new { name = "TV", resolution = "1280x720", maxFps = 60, audioOnly = false }
        });

        var req = await engine.ReadRequestAsync();
        // raw payload must use camelCase names (engine contract)
        Assert.Equal(60, req["params"]?["params"]?["maxFps"]?.GetValue<int>());
        Assert.Equal("1280x720", req["params"]?["params"]?["resolution"]?.GetValue<string>());
        Assert.Equal(false, req["params"]?["params"]?["audioOnly"]?.GetValue<bool>());
        await engine.RespondAsync(req["id"]!.GetValue<long>(), IdleStatus);
        await invoke;
    }

    [Fact]
    public async Task MalformedLines_AreSkipped()
    {
        await using var engine = new FakeEngine();
        await engine.Client.ConnectAsync();
        var invoke = engine.Client.InvokeAsync("status");
        var req = await engine.ReadRequestAsync();

        await engine.NotifyAsync("this is not json {{{");
        await engine.RespondAsync(req["id"]!.GetValue<long>(), IdleStatus);

        var result = await invoke; // must still resolve
        Assert.Equal("idle", result["state"]?.GetValue<string>());
    }

    [Fact]
    public async Task StateNotification_RaisesEvent()
    {
        await using var engine = new FakeEngine();
        string? seenState = null;
        engine.Client.StateChanged += (s, _) => seenState = s;
        await engine.Client.ConnectAsync();

        await engine.NotifyAsync(
            "{\"jsonrpc\":\"2.0\",\"method\":\"state\",\"params\":{\"state\":\"streaming\"}}");
        await Task.Delay(500);
        Assert.Equal("streaming", seenState);
    }

    [Fact]
    public async Task LogNotification_RaisesEvent()
    {
        await using var engine = new FakeEngine();
        string? seenMsg = null;
        engine.Client.LogReceived += (_, m) => seenMsg = m;
        await engine.Client.ConnectAsync();

        await engine.NotifyAsync(
            "{\"jsonrpc\":\"2.0\",\"method\":\"log\",\"params\":{\"level\":\"warn\",\"message\":\"hello\"}}");
        await Task.Delay(500);
        Assert.Equal("hello", seenMsg);
    }
}
