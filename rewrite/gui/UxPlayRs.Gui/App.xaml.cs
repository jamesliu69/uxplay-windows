using System.IO;
using UxPlayRs.Gui.Settings;
using WpfApplication = System.Windows.Application;
using WpfMessageBox = System.Windows.MessageBox;
using WpfStartupEventArgs = System.Windows.StartupEventArgs;
using WpfExitEventArgs = System.Windows.ExitEventArgs;
using WpfMessageBoxButton = System.Windows.MessageBoxButton;
using WpfMessageBoxImage = System.Windows.MessageBoxImage;

namespace UxPlayRs.Gui;

/// <summary>Application entry: single instance, shared services.</summary>
public partial class App : WpfApplication
{
    private static Mutex? _instanceMutex;

    public AppSettings Settings { get; } = AppSettings.Load();
    public LogRingBuffer Logs { get; } = new();
    public EngineProcessManager EngineProcess { get; } = new();
    public EngineController Controller { get; } = new();

    protected override void OnStartup(WpfStartupEventArgs e)
    {
        _instanceMutex = new Mutex(initiallyOwned: true, name: @"Global\UxPlayRs-Gui-SingleInstance", out var created);
        if (!created)
        {
            WpfMessageBox.Show("UxPlayRs is already running (check the system tray).",
                "UxPlayRs", WpfMessageBoxButton.OK, WpfMessageBoxImage.Information);
            Shutdown();
            return;
        }

        EngineProcess.EngineOutput += line =>
        {
            Logs.Add("engine", line);
            Dispatcher.BeginInvoke(() => (MainWindow as MainWindow)?.AppendLog("engine", line));
        };
        Controller.LogReceived += (level, message) =>
        {
            Logs.Add(level, message);
            Dispatcher.BeginInvoke(() => (MainWindow as MainWindow)?.AppendLog(level, message));
        };

        try
        {
            Directory.CreateDirectory(Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "uxplay-rs", "logs"));
        }
        catch { }

        base.OnStartup(e);
    }

    protected override void OnExit(WpfExitEventArgs e)
    {
        try { Controller.DisposeAsync().AsTask().Wait(TimeSpan.FromSeconds(2)); } catch { }
        EngineProcess.Dispose();
        _instanceMutex?.Dispose();
        base.OnExit(e);
    }
}
