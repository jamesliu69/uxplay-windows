using System.IO;
using System.Windows;
using UxPlayRs.Gui.Ipc;
using UxPlayRs.Gui.Settings;
using Forms = System.Windows.Forms;
using WpfComboBox = System.Windows.Controls.ComboBox;
using WpfComboBoxItem = System.Windows.Controls.ComboBoxItem;
using WpfMessageBox = System.Windows.MessageBox;
using WpfMessageBoxButton = System.Windows.MessageBoxButton;
using WpfMessageBoxImage = System.Windows.MessageBoxImage;

namespace UxPlayRs.Gui;

public partial class MainWindow : Window
{
    private readonly App _app;
    private Forms.NotifyIcon? _tray;
    private bool _isExiting;

    public MainWindow()
    {
        InitializeComponent();
        _app = (App)System.Windows.Application.Current;

        LoadSettingsIntoUi();
        SetupTray();
        RefreshButtons(running: false, state: "idle");
        AppendLog("info", "UxPlayRs GUI started. Press Start to launch the engine.");
    }

    private void LoadSettingsIntoUi()
    {
        var s = _app.Settings;
        DeviceNameBox.Text = s.DeviceName;
        SelectComboByTag(ResolutionCombo, s.Resolution);
        SelectComboByTag(FpsCombo, s.MaxFps.ToString());
        AudioOnlyCheck.IsChecked = s.AudioOnly;
        try { AutostartCheck.IsChecked = AutostartHelper.IsEnabled(); }
        catch { AutostartCheck.IsChecked = false; }
    }

    private static void SelectComboByTag(WpfComboBox combo, string tag)
    {
        foreach (WpfComboBoxItem item in combo.Items)
        {
            if ((item.Tag as string) == tag)
            {
                combo.SelectedItem = item;
                return;
            }
        }
        combo.SelectedIndex = 0;
    }

    private EngineParams CollectParams() => new()
    {
        Name = string.IsNullOrWhiteSpace(DeviceNameBox.Text) ? "uxplay-rs" : DeviceNameBox.Text.Trim(),
        Resolution = (ResolutionCombo.SelectedItem as WpfComboBoxItem)?.Tag as string ?? "1920x1080",
        MaxFps = uint.TryParse((FpsCombo.SelectedItem as WpfComboBoxItem)?.Tag as string, out var fps) ? fps : 30,
        AudioOnly = AudioOnlyCheck.IsChecked == true,
    };

    private void SaveUiToSettings()
    {
        var p = CollectParams();
        var s = _app.Settings;
        s.DeviceName = p.Name;
        s.Resolution = p.Resolution;
        s.MaxFps = p.MaxFps;
        s.AudioOnly = p.AudioOnly;
        try { s.Save(); } catch { }
        try { AutostartHelper.SetEnabled(AutostartCheck.IsChecked == true); } catch { }
    }

    private void SetupTray()
    {
        _tray = new Forms.NotifyIcon
        {
            Text = "UxPlayRs",
            Visible = true,
        };
        try
        {
            _tray.Icon = System.Drawing.SystemIcons.Application;
        }
        catch { }
        var menu = new Forms.ContextMenuStrip();
        menu.Items.Add("Show", null, (_, _) => ShowMainWindow());
        menu.Items.Add("Start", null, async (_, _) => await StartEngineAsync());
        menu.Items.Add("Stop", null, async (_, _) => await StopEngineAsync());
        menu.Items.Add("Exit", null, (_, _) => ExitApp());
        _tray.ContextMenuStrip = menu;
        _tray.DoubleClick += (_, _) => ShowMainWindow();
    }

    private void ShowMainWindow()
    {
        Show();
        WindowState = WindowState.Normal;
        Activate();
    }

    private void ExitApp()
    {
        _isExiting = true;
        if (_tray is not null)
        {
            _tray.Visible = false;
            _tray.Dispose();
            _tray = null;
        }
        System.Windows.Application.Current.Shutdown();
    }

    protected override void OnClosing(System.ComponentModel.CancelEventArgs e)
    {
        if (!_isExiting)
        {
            e.Cancel = true;
            Hide();
            _tray?.ShowBalloonTip(2000, "UxPlayRs", "Still running in the system tray.", Forms.ToolTipIcon.Info);
        }
        base.OnClosing(e);
    }

    private void RefreshButtons(bool running, string state)
    {
        StartButton.IsEnabled = !running;
        StopButton.IsEnabled = running;
        StatusText.Text = running ? $"Running ({state})" : "Stopped";
        PipeText.Text = _app.Controller.IsConnected ? "connected to airplayd" : "not connected";
    }

    private async void OnStartClicked(object sender, RoutedEventArgs e) => await StartEngineAsync();

    private async void OnStopClicked(object sender, RoutedEventArgs e) => await StopEngineAsync();

    private async Task StartEngineAsync()
    {
        SaveUiToSettings();
        StartButton.IsEnabled = false;
        try
        {
            var engineExe = EngineProcessManager.FindEngineExe()
                ?? throw new FileNotFoundException("airplayd.exe not found");
            var firewall = await FirewallHelper.EnsurePrivateInboundRuleAsync(engineExe);
            if (firewall == FirewallHelper.SetupResult.Cancelled)
            {
                AppendLog("warn", "Windows Firewall access was not granted; iPhone discovery or connection may be blocked.");
            }
            else if (firewall == FirewallHelper.SetupResult.Failed)
            {
                AppendLog("warn", "Could not configure the Windows Firewall rule; iPhone discovery or connection may be blocked.");
            }
            else if (firewall == FirewallHelper.SetupResult.Configured)
            {
                AppendLog("info", "Windows Firewall private-network rule configured for airplayd.");
            }

            _app.EngineProcess.Start(engineExe);
            AppendLog("info", "airplayd process started.");
            await _app.Controller.ConnectAsync();
            PipeText.Text = "connected to airplayd";
            var status = await _app.Controller.StartAsync(CollectParams());
            DetailText.Text = status.Detail;
            if (!status.Running)
            {
                AppendLog("error", $"Engine failed to start: {status.Detail}");
                await _app.Controller.DisconnectAsync();
                _app.EngineProcess.Stop();
                RefreshButtons(running: false, state: status.State);
                return;
            }

            RefreshButtons(running: true, state: status.State);
            AppendLog("info", $"Engine state: {status.State}");
        }
        catch (Exception ex)
        {
            try { await _app.Controller.DisconnectAsync(); } catch { }
            _app.EngineProcess.Stop();
            DetailText.Text = ex.Message;
            AppendLog("error", $"Start failed: {ex.Message}");
            RefreshButtons(running: false, state: "idle");
        }
    }

    private async Task StopEngineAsync()
    {
        try
        {
            if (_app.Controller.IsConnected)
                await _app.Controller.StopAsync();
        }
        catch (Exception ex)
        {
            AppendLog("warn", $"Stop RPC failed (stopping process anyway): {ex.Message}");
        }
        finally
        {
            try { await _app.Controller.DisconnectAsync(); }
            catch (Exception ex) { AppendLog("warn", $"IPC disconnect failed: {ex.Message}"); }
        }
        _app.EngineProcess.Stop();
        RefreshButtons(running: false, state: "idle");
        AppendLog("info", "Engine stopped.");
    }

    private void OnSaveLogClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            var dir = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "uxplay-rs", "logs");
            Directory.CreateDirectory(dir);
            var path = Path.Combine(dir, $"session-{DateTime.Now:yyyyMMdd-HHmmss}.txt");
            File.WriteAllLines(path, _app.Logs.Snapshot().Select(x => $"[{x.Time:HH:mm:ss}] {x.Level}: {x.Message}"));
            AppendLog("info", $"Log saved to {path}");
        }
        catch (Exception ex)
        {
            WpfMessageBox.Show($"Could not save log: {ex.Message}", "UxPlayRs",
                WpfMessageBoxButton.OK, WpfMessageBoxImage.Warning);
        }
    }

    internal void AppendLog(string level, string message)
    {
        if (!Dispatcher.CheckAccess())
        {
            Dispatcher.BeginInvoke(() => AppendLog(level, message));
            return;
        }
        LogList.Items.Add($"[{DateTime.Now:HH:mm:ss}] {level}: {message}");
        while (LogList.Items.Count > 1000)
            LogList.Items.RemoveAt(0);
        LogList.ScrollIntoView(LogList.Items[^1]);
    }
}
