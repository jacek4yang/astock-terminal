[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateRange(1, [int]::MaxValue)][int]$ProcessId,
    [Parameter(Mandatory)][string]$ExpectedExecutablePath,
    [ValidateSet(
        'inspect',
        'move-resize',
        'maximize',
        'restore',
        'minimize',
        'interactive-drag',
        'interactive-double-click',
        'interactive-edge-resize',
        'interactive-context-menu'
    )][string]$Operation = 'inspect',
    [ValidateRange(100, 30000)][int]$TimeoutMs = 10000,
    [int]$X = 120,
    [int]$Y = 120,
    [ValidateRange(640, 10000)][int]$Width = 1440,
    [ValidateRange(480, 10000)][int]$Height = 900,
    [int]$StartX = 0,
    [int]$StartY = 0,
    [int]$EndX = 0,
    [int]$EndY = 0,
    [switch]$AllowInteractiveInput,
    [switch]$SkipSpaceCheck
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck

$expectedPath = [System.IO.Path]::GetFullPath($ExpectedExecutablePath)
if (-not (Test-Path -LiteralPath $expectedPath -PathType Leaf)) {
    throw "Expected packaged executable is missing: $expectedPath"
}
$process = Get-Process -Id $ProcessId -ErrorAction Stop
$actualPath = [System.IO.Path]::GetFullPath($process.Path)
if (-not $actualPath.Equals($expectedPath, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to control an unrelated process. Expected $expectedPath, got $actualPath"
}

$probeTemp = Join-Path $build.Paths.FormalCache 'window-probe-compiler'
New-Item -ItemType Directory -Path $probeTemp -Force | Out-Null
$previousTemp = $env:TEMP
$previousTmp = $env:TMP
$env:TEMP = $probeTemp
$env:TMP = $probeTemp
try {
    if (-not ('AStockDesktopWindowProbe' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

public static class AStockDesktopWindowProbe
{
    public const int SW_HIDE = 0;
    public const int SW_SHOWNORMAL = 1;
    public const int SW_SHOWMINIMIZED = 2;
    public const int SW_SHOWMAXIMIZED = 3;
    public const int SW_RESTORE = 9;
    public const int GWL_STYLE = -16;
    public const int GWL_EXSTYLE = -20;
    public const int GCLP_HICON = -14;
    public const int GCLP_HICONSM = -34;
    public const long WS_THICKFRAME = 0x00040000L;
    public const long WS_MINIMIZEBOX = 0x00020000L;
    public const long WS_MAXIMIZEBOX = 0x00010000L;
    public const long WS_EX_TOOLWINDOW = 0x00000080L;
    public const uint WM_GETICON = 0x007F;
    public const uint ICON_SMALL = 0;
    public const uint ICON_BIG = 1;
    public const uint ICON_SMALL2 = 2;
    public const uint SMTO_ABORTIFHUNG = 0x0002;
    public const uint MOUSEEVENTF_MOVE = 0x0001;
    public const uint MOUSEEVENTF_LEFTDOWN = 0x0002;
    public const uint MOUSEEVENTF_LEFTUP = 0x0004;
    public const uint MOUSEEVENTF_RIGHTDOWN = 0x0008;
    public const uint MOUSEEVENTF_RIGHTUP = 0x0010;
    public const byte VK_ESCAPE = 0x1B;
    public const uint KEYEVENTF_KEYUP = 0x0002;

    [StructLayout(LayoutKind.Sequential)]
    public struct Rect
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct Point
    {
        public int X;
        public int Y;
    }

    public sealed class Snapshot
    {
        public int ProcessId { get; set; }
        public string WindowHandle { get; set; }
        public string ClassName { get; set; }
        public string Title { get; set; }
        public int X { get; set; }
        public int Y { get; set; }
        public int Width { get; set; }
        public int Height { get; set; }
        public int ClientOriginX { get; set; }
        public int ClientOriginY { get; set; }
        public bool Visible { get; set; }
        public bool Maximized { get; set; }
        public bool Minimized { get; set; }
        public bool Resizable { get; set; }
        public bool HasMinimizeBox { get; set; }
        public bool HasMaximizeBox { get; set; }
        public bool TaskbarEligible { get; set; }
        public bool HasLargeIcon { get; set; }
        public bool HasSmallIcon { get; set; }
        public uint Dpi { get; set; }
    }

    private delegate bool EnumWindowsCallback(IntPtr window, IntPtr parameter);

    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumWindowsCallback callback, IntPtr parameter);
    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
    [DllImport("user32.dll")]
    private static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")]
    private static extern bool IsZoomed(IntPtr window);
    [DllImport("user32.dll")]
    private static extern bool IsIconic(IntPtr window);
    [DllImport("user32.dll")]
    private static extern IntPtr GetWindow(IntPtr window, uint command);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassName(IntPtr window, StringBuilder value, int maxCount);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowText(IntPtr window, StringBuilder value, int maxCount);
    [DllImport("user32.dll")]
    private static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")]
    private static extern bool ClientToScreen(IntPtr window, ref Point point);
    [DllImport("user32.dll")]
    private static extern IntPtr GetWindowLongPtr(IntPtr window, int index);
    [DllImport("user32.dll")]
    private static extern IntPtr GetClassLongPtr(IntPtr window, int index);
    [DllImport("user32.dll")]
    private static extern bool ShowWindowAsync(IntPtr window, int command);
    [DllImport("user32.dll")]
    private static extern bool MoveWindow(IntPtr window, int x, int y, int width, int height, bool repaint);
    [DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")]
    private static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")]
    private static extern bool GetCursorPos(out Point point);
    [DllImport("user32.dll")]
    private static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);
    [DllImport("user32.dll")]
    private static extern void keybd_event(byte virtualKey, byte scanCode, uint flags, UIntPtr extraInfo);
    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr window);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr SendMessageTimeout(IntPtr window, uint message, UIntPtr wParam, IntPtr lParam, uint flags, uint timeout, out UIntPtr result);

    public static IntPtr WaitForMainWindow(int processId, int timeoutMs)
    {
        DateTime deadline = DateTime.UtcNow.AddMilliseconds(timeoutMs);
        do
        {
            IntPtr found = IntPtr.Zero;
            EnumWindows(delegate(IntPtr window, IntPtr ignored)
            {
                uint ownerProcess;
                GetWindowThreadProcessId(window, out ownerProcess);
                if (ownerProcess != (uint)processId || !IsWindowVisible(window) || GetWindow(window, 4) != IntPtr.Zero)
                    return true;
                StringBuilder className = new StringBuilder(128);
                GetClassName(window, className, className.Capacity);
                if (className.ToString().IndexOf("Proton", StringComparison.OrdinalIgnoreCase) < 0)
                    return true;
                found = window;
                return false;
            }, IntPtr.Zero);
            if (found != IntPtr.Zero) return found;
            Thread.Sleep(50);
        }
        while (DateTime.UtcNow < deadline);
        return IntPtr.Zero;
    }

    private static bool HasIcon(IntPtr window, uint size, int classIndex)
    {
        UIntPtr result;
        SendMessageTimeout(window, WM_GETICON, new UIntPtr(size), IntPtr.Zero, SMTO_ABORTIFHUNG, 500, out result);
        return result != UIntPtr.Zero || GetClassLongPtr(window, classIndex) != IntPtr.Zero;
    }

    public static Snapshot Inspect(IntPtr window, int processId)
    {
        Rect rect;
        if (!GetWindowRect(window, out rect)) throw new InvalidOperationException("GetWindowRect failed");
        Point client = new Point();
        if (!ClientToScreen(window, ref client)) throw new InvalidOperationException("ClientToScreen failed");
        StringBuilder className = new StringBuilder(128);
        StringBuilder title = new StringBuilder(512);
        GetClassName(window, className, className.Capacity);
        GetWindowText(window, title, title.Capacity);
        long style = GetWindowLongPtr(window, GWL_STYLE).ToInt64();
        long exStyle = GetWindowLongPtr(window, GWL_EXSTYLE).ToInt64();
        return new Snapshot {
            ProcessId = processId,
            WindowHandle = "0x" + window.ToInt64().ToString("x"),
            ClassName = className.ToString(),
            Title = title.ToString(),
            X = rect.Left,
            Y = rect.Top,
            Width = rect.Right - rect.Left,
            Height = rect.Bottom - rect.Top,
            ClientOriginX = client.X,
            ClientOriginY = client.Y,
            Visible = IsWindowVisible(window),
            Maximized = IsZoomed(window),
            Minimized = IsIconic(window),
            Resizable = (style & WS_THICKFRAME) != 0,
            HasMinimizeBox = (style & WS_MINIMIZEBOX) != 0,
            HasMaximizeBox = (style & WS_MAXIMIZEBOX) != 0,
            TaskbarEligible = GetWindow(window, 4) == IntPtr.Zero && (exStyle & WS_EX_TOOLWINDOW) == 0,
            HasLargeIcon = HasIcon(window, ICON_BIG, GCLP_HICON),
            HasSmallIcon = HasIcon(window, ICON_SMALL2, GCLP_HICONSM) || HasIcon(window, ICON_SMALL, GCLP_HICONSM),
            Dpi = GetDpiForWindow(window),
        };
    }

    public static void Show(IntPtr window, int command)
    {
        ShowWindowAsync(window, command);
    }

    public static void MoveResize(IntPtr window, int x, int y, int width, int height)
    {
        ShowWindowAsync(window, SW_RESTORE);
        if (!MoveWindow(window, x, y, width, height, true))
            throw new InvalidOperationException("MoveWindow failed");
    }

    private static void MouseButton(uint down, uint up, int x, int y)
    {
        SetCursorPos(x, y);
        mouse_event(down, 0, 0, 0, UIntPtr.Zero);
        Thread.Sleep(45);
        mouse_event(up, 0, 0, 0, UIntPtr.Zero);
    }

    public static void InteractiveDrag(IntPtr window, int startX, int startY, int endX, int endY)
    {
        Point original;
        GetCursorPos(out original);
        try {
            SetForegroundWindow(window);
            SetCursorPos(startX, startY);
            mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, UIntPtr.Zero);
            for (int step = 1; step <= 8; step++) {
                Thread.Sleep(30);
                SetCursorPos(startX + ((endX - startX) * step / 8), startY + ((endY - startY) * step / 8));
            }
            mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, UIntPtr.Zero);
        } finally {
            mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, UIntPtr.Zero);
            SetCursorPos(original.X, original.Y);
        }
    }

    public static void InteractiveDoubleClick(IntPtr window, int x, int y)
    {
        Point original;
        GetCursorPos(out original);
        try {
            SetForegroundWindow(window);
            MouseButton(MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, x, y);
            Thread.Sleep(70);
            MouseButton(MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, x, y);
        } finally {
            SetCursorPos(original.X, original.Y);
        }
    }

    public static void InteractiveContextMenu(IntPtr window, int x, int y)
    {
        Point original;
        GetCursorPos(out original);
        try {
            SetForegroundWindow(window);
            MouseButton(MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, x, y);
            Thread.Sleep(250);
            keybd_event(VK_ESCAPE, 0, 0, UIntPtr.Zero);
            keybd_event(VK_ESCAPE, 0, KEYEVENTF_KEYUP, UIntPtr.Zero);
        } finally {
            SetCursorPos(original.X, original.Y);
        }
    }

    public static void InteractiveEdgeResize(IntPtr window, int endX, int endY)
    {
        Rect rect;
        if (!GetWindowRect(window, out rect)) throw new InvalidOperationException("GetWindowRect failed");
        InteractiveDrag(window, rect.Right - 2, rect.Bottom - 2, endX, endY);
    }
}
'@
    }
} finally {
    $env:TEMP = $previousTemp
    $env:TMP = $previousTmp
}

$window = [AStockDesktopWindowProbe]::WaitForMainWindow($ProcessId, $TimeoutMs)
if ($window -eq [IntPtr]::Zero) {
    throw "No visible Proton top-level window appeared for process $ProcessId within $TimeoutMs ms."
}

$interactive = $Operation.StartsWith('interactive-', [System.StringComparison]::Ordinal)
if ($interactive -and -not $AllowInteractiveInput) {
    throw "$Operation sends real mouse/keyboard input and requires explicit -AllowInteractiveInput."
}

switch ($Operation) {
    'move-resize' { [AStockDesktopWindowProbe]::MoveResize($window, $X, $Y, $Width, $Height) }
    'maximize' { [AStockDesktopWindowProbe]::Show($window, [AStockDesktopWindowProbe]::SW_SHOWMAXIMIZED) }
    'restore' { [AStockDesktopWindowProbe]::Show($window, [AStockDesktopWindowProbe]::SW_RESTORE) }
    'minimize' { [AStockDesktopWindowProbe]::Show($window, [AStockDesktopWindowProbe]::SW_SHOWMINIMIZED) }
    'interactive-drag' { [AStockDesktopWindowProbe]::InteractiveDrag($window, $StartX, $StartY, $EndX, $EndY) }
    'interactive-double-click' { [AStockDesktopWindowProbe]::InteractiveDoubleClick($window, $StartX, $StartY) }
    'interactive-edge-resize' { [AStockDesktopWindowProbe]::InteractiveEdgeResize($window, $EndX, $EndY) }
    'interactive-context-menu' { [AStockDesktopWindowProbe]::InteractiveContextMenu($window, $StartX, $StartY) }
}

if ($Operation -ne 'inspect') { Start-Sleep -Milliseconds 350 }
[AStockDesktopWindowProbe]::Inspect($window, $ProcessId) | ConvertTo-Json -Depth 5
