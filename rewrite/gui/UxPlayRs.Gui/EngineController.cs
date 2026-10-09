using System.Text.Json;
using UxPlayRs.Gui.Ipc;

namespace UxPlayRs.Gui;

/// <summary>
/// High-level facade over <see cref="IpcClient"/> exposing typed engine calls.
/// </summary>
public sealed class EngineController : IAsyncDisposable
{
    private readonly IpcClient _client;

    public event Action<string, string?>? StateChanged
    {
        add => _client.StateChanged += value;
        remove => _client.StateChanged -= value;
    }

    public event Action<string, string>? LogReceived
    {
        add => _client.LogReceived += value;
        remove => _client.LogReceived -= value;
    }

    public bool IsConnected => _client.IsConnected;

    public EngineController(IpcClient? client = null)
    {
        _client = client ?? new IpcClient();
    }

    public Task ConnectAsync(CancellationToken ct = default) => _client.ConnectAsync(ct);

    public Task DisconnectAsync() => _client.DisconnectAsync();

    public async Task<StatusResponse> StatusAsync(CancellationToken ct = default)
    {
        var result = await _client.InvokeAsync("status", null, ct).ConfigureAwait(false);
        return result.Deserialize<StatusResponse>(EngineJson.Options) ?? new StatusResponse();
    }

    public async Task<StatusResponse> StartAsync(EngineParams @params, CancellationToken ct = default)
    {
        var result = await _client.InvokeAsync("start", new { @params }, ct).ConfigureAwait(false);
        return result.Deserialize<StatusResponse>(EngineJson.Options) ?? new StatusResponse();
    }

    public async Task<StatusResponse> StopAsync(CancellationToken ct = default)
    {
        var result = await _client.InvokeAsync("stop", null, ct).ConfigureAwait(false);
        return result.Deserialize<StatusResponse>(EngineJson.Options) ?? new StatusResponse();
    }

    public async Task<StatusResponse> SetParamsAsync(EngineParams @params, CancellationToken ct = default)
    {
        var result = await _client.InvokeAsync("set_params", new { @params }, ct).ConfigureAwait(false);
        return result.Deserialize<StatusResponse>(EngineJson.Options) ?? new StatusResponse();
    }

    public ValueTask DisposeAsync() => _client.DisposeAsync();
}
