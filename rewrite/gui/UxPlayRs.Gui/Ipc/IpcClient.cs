using System.Collections.Concurrent;
using System.IO;
using System.IO.Pipes;
using System.Text;
using System.Text.Json.Nodes;

namespace UxPlayRs.Gui.Ipc;

/// <summary>
/// NDJSON JSON-RPC 2.0 client for the airplayd named pipe.
/// The transport is a (TextReader, TextWriter) pair so unit tests can drive
/// the protocol without a real pipe.
/// </summary>
public sealed class IpcClient : IAsyncDisposable
{
    public const string DefaultPipeName = "uxplay-rs-airplayd";
    public const string FullPipeName = @"\\.\pipe\uxplay-rs-airplayd";

    private readonly Func<CancellationToken, Task<(TextReader Reader, TextWriter Writer)>> _transportFactory;
    private readonly ConcurrentDictionary<long, TaskCompletionSource<JsonNode>> _pending = new();
    private readonly SemaphoreSlim _writeLock = new(1, 1);
    private readonly TimeSpan _requestTimeout;
    private long _nextId;
    private TextWriter? _writer;
    private CancellationTokenSource? _loopCts;
    private Task? _loopTask;
    private int _disposed;

    public event Action<string, string?>? StateChanged;
    public event Action<string, string>? LogReceived;
    public event Action<long?>? VideoWindow;

    public bool IsConnected => _loopTask is { IsCompleted: false };

    public IpcClient(string pipeName = DefaultPipeName, TimeSpan? requestTimeout = null)
        : this(ct => OpenPipeAsync(pipeName, ct), requestTimeout)
    {
    }

    internal IpcClient(TextReader reader, TextWriter writer, TimeSpan? requestTimeout = null)
        : this(_ => Task.FromResult<(TextReader, TextWriter)>((reader, writer)), requestTimeout)
    {
    }

    private IpcClient(
        Func<CancellationToken, Task<(TextReader Reader, TextWriter Writer)>> transportFactory,
        TimeSpan? requestTimeout)
    {
        _transportFactory = transportFactory;
        _requestTimeout = requestTimeout ?? TimeSpan.FromSeconds(5);
    }

    private static async Task<(TextReader, TextWriter)> OpenPipeAsync(string pipeName, CancellationToken ct)
    {
        var pipe = new NamedPipeClientStream(".", pipeName, PipeDirection.InOut, PipeOptions.Asynchronous);
        await pipe.ConnectAsync(ct).ConfigureAwait(false);
        // No-BOM UTF-8: the engine parses raw NDJSON and rejects a BOM preamble.
        var encoding = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false);
        var reader = new StreamReader(pipe, encoding, leaveOpen: false);
        var writer = new StreamWriter(pipe, encoding, leaveOpen: false) { AutoFlush = true, NewLine = "\n" };
        return (reader, writer);
    }

    /// <summary>Connect (with retry) and start the receive loop.</summary>
    public async Task ConnectAsync(CancellationToken ct = default)
    {
        await DisconnectAsync().ConfigureAwait(false);
        Exception? last = null;
        var delay = TimeSpan.FromMilliseconds(200);
        for (var attempt = 0; attempt < 10; attempt++)
        {
            ct.ThrowIfCancellationRequested();
            try
            {
                var (reader, writer) = await _transportFactory(ct).ConfigureAwait(false);
                _writer = writer;
                _loopCts = CancellationTokenSource.CreateLinkedTokenSource(ct);
                _loopTask = Task.Run(() => ReceiveLoopAsync(reader, _loopCts.Token), ct);
                return;
            }
            catch (Exception ex) when (ex is IOException or TimeoutException or UnauthorizedAccessException)
            {
                last = ex;
                await Task.Delay(delay, ct).ConfigureAwait(false);
                delay = TimeSpan.FromMilliseconds(Math.Min(delay.TotalMilliseconds * 2, 2000));
            }
        }
        throw new IOException($"Could not connect to {FullPipeName}", last);
    }

    public async Task DisconnectAsync()
    {
        try { _loopCts?.Cancel(); } catch { }
        if (_loopTask is not null)
        {
            try { await _loopTask.ConfigureAwait(false); } catch { }
            _loopTask = null;
        }
        _loopCts?.Dispose();
        _loopCts = null;
        _writer = null;
    }

    private async Task ReceiveLoopAsync(TextReader reader, CancellationToken ct)
    {
        try
        {
            while (!ct.IsCancellationRequested)
            {
                var line = await reader.ReadLineAsync(ct).ConfigureAwait(false);
                if (line is null)
                    break; // remote closed
                if (string.IsNullOrWhiteSpace(line))
                    continue;
                HandleLine(line);
            }
        }
        catch (OperationCanceledException) { }
        catch (IOException) { }
        catch (ObjectDisposedException) { }
        finally
        {
            foreach (var kv in _pending)
            {
                if (_pending.TryRemove(kv.Key, out var tcs))
                    tcs.TrySetException(new IOException("IPC connection closed"));
            }
            try { (reader as IDisposable)?.Dispose(); } catch { }
        }
    }

    internal void HandleLine(string line)
    {
        JsonNode? node;
        try { node = JsonNode.Parse(line); }
        catch { return; } // malformed: ignore
        if (node is not JsonObject obj)
            return;

        // Response (id matches a pending request)?
        if (obj.TryGetPropertyValue("id", out var idNode)
            && idNode is JsonValue idValue
            && idValue.TryGetValue<long>(out var id)
            && _pending.TryRemove(id, out var tcs))
        {
            if (obj.TryGetPropertyValue("result", out var result) && result is not null)
                tcs.TrySetResult(result);
            else if (obj.TryGetPropertyValue("error", out var err) && err is JsonObject errObj)
                tcs.TrySetException(new InvalidOperationException(
                    errObj["message"]?.GetValue<string>() ?? "RPC error"));
            else
                tcs.TrySetException(new InvalidOperationException("Malformed RPC response"));
            return;
        }

        // Notification (no id).
        var method = obj["method"]?.GetValue<string>();
        var p = obj["params"] as JsonObject;
        switch (method)
        {
            case "state" when p is not null:
                StateChanged?.Invoke(p["state"]?.GetValue<string>() ?? "idle",
                    p["detail"]?.GetValue<string>());
                break;
            case "log" when p is not null:
                LogReceived?.Invoke(p["level"]?.GetValue<string>() ?? "info",
                    p["message"]?.GetValue<string>() ?? string.Empty);
                break;
            case "video" when p is not null:
                VideoWindow?.Invoke(p["hwnd"]?.GetValue<long?>());
                break;
        }
    }

    public async Task<JsonNode> InvokeAsync(string method, object? @params = null, CancellationToken ct = default)
    {
        var writer = _writer ?? throw new InvalidOperationException("Not connected");
        var id = Interlocked.Increment(ref _nextId);
        var tcs = new TaskCompletionSource<JsonNode>(TaskCreationOptions.RunContinuationsAsynchronously);
        _pending[id] = tcs;

        var request = new JsonObject
        {
            ["jsonrpc"] = "2.0",
            ["id"] = id,
            ["method"] = method,
        };
        if (@params is not null)
            request["params"] = System.Text.Json.JsonSerializer.SerializeToNode(@params, EngineJson.Options);

        await _writeLock.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            await writer.WriteLineAsync(request.ToJsonString()).ConfigureAwait(false);
            await writer.FlushAsync(ct).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            _pending.TryRemove(id, out _);
            throw new IOException("IPC write failed", ex);
        }
        finally
        {
            _writeLock.Release();
        }

        using var timeout = new CancellationTokenSource(_requestTimeout);
        using var linked = CancellationTokenSource.CreateLinkedTokenSource(ct, timeout.Token);
        using var registration = linked.Token.Register(() =>
        {
            if (_pending.TryRemove(id, out var pending))
                pending.TrySetException(new TimeoutException($"RPC '{method}' timed out"));
        });
        return await tcs.Task.ConfigureAwait(false);
    }

    public async ValueTask DisposeAsync()
    {
        if (Interlocked.Exchange(ref _disposed, 1) != 0)
            return;
        await DisconnectAsync().ConfigureAwait(false);
        _writeLock.Dispose();
    }
}
