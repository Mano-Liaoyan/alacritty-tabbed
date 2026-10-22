param(
    [string]$Exe = "$PSScriptRoot\..\target\release\alacritty.exe",
    [switch]$KeepOpen,
    [switch]$SkipKeyboard
)
$ErrorActionPreference = 'Stop'
$testWork = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\work'))
New-Item -ItemType Directory -Path $testWork -Force | Out-Null
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class TabNative {
    public delegate bool EnumProc(IntPtr hwnd, IntPtr param);
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left,Top,Right,Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X,Y; }
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc callback,IntPtr param);
    [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr parent,EnumProc callback,IntPtr param);
    [DllImport("user32.dll")] public static extern IntPtr GetParent(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd,out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern IntPtr FindWindow(string cls,string title);
    public static IntPtr Find(string cls) { return FindWindow(cls,null); }
    public static void Key(IntPtr hwnd,ushort key) { PostMessage(hwnd,0x100,(IntPtr)key,IntPtr.Zero); }
    [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr hwnd,ref Point point);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hwnd,out Rect rect);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd,out Rect rect);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hwnd,uint message,IntPtr wParam,IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd,int command);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr hwnd,int x,int y,int w,int h,bool redraw);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr hwnd,uint flags);
    [StructLayout(LayoutKind.Sequential)] public struct KeyInput {public ushort Key,Scan;public uint Flags,Time;public UIntPtr Extra;}
    [StructLayout(LayoutKind.Sequential)] public struct MouseInput {public int X,Y;public uint Data,Flags,Time;public UIntPtr Extra;}
    [StructLayout(LayoutKind.Explicit)] public struct InputData {
        [FieldOffset(0)] public KeyInput Keyboard;
        [FieldOffset(0)] public MouseInput Mouse;
    }
    [StructLayout(LayoutKind.Sequential)] public struct Input {public uint Type;public InputData Data;}
    [DllImport("user32.dll")] static extern uint SendInput(uint count,Input[] inputs,int size);
    [DllImport("user32.dll")] static extern uint MapVirtualKey(uint code,uint mapType);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd,StringBuilder text,int max);
    public static IntPtr Host(int pid) {
        IntPtr result=IntPtr.Zero;
        EnumWindows((h,p)=>{uint owner;GetWindowThreadProcessId(h,out owner);
            if(owner==pid && IsWindowVisible(h) && GetParent(h)==IntPtr.Zero && Title(h).Contains("Alacritty Tabs")) result=h;
            return true;},IntPtr.Zero);
        return result;
    }
    public static IntPtr[] Children(IntPtr host) {
        var result=new List<IntPtr>();
        EnumChildWindows(host,(h,p)=>{if(GetParent(h)==host) result.Add(h);return true;},IntPtr.Zero);
        return result.ToArray();
    }
    public static string Title(IntPtr hwnd) {var b=new StringBuilder(512);GetWindowText(hwnd,b,b.Capacity);return b.ToString();}
    public static void Click(IntPtr hwnd,int x,int y,bool middle=false) {
        var point=(IntPtr)((y<<16)|(x&65535));
        PostMessage(hwnd,0x200,IntPtr.Zero,point);
        PostMessage(hwnd,middle?0x207u:0x201u,(IntPtr)(middle?16:1),point);
        PostMessage(hwnd,middle?0x208u:0x202u,IntPtr.Zero,point);
    }
    public static void Drag(IntPtr hwnd,int fromX,int toX,int y) {
        PostMessage(hwnd,0x200,IntPtr.Zero,(IntPtr)((y<<16)|(fromX&65535)));
        PostMessage(hwnd,0x201,(IntPtr)1,(IntPtr)((y<<16)|(fromX&65535)));
        for(int step=1;step<=8;step++) {
            int x=fromX+(toX-fromX)*step/8;
            PostMessage(hwnd,0x200,(IntPtr)1,(IntPtr)((y<<16)|(x&65535)));
        }
        PostMessage(hwnd,0x202,IntPtr.Zero,(IntPtr)((y<<16)|(toX&65535)));
    }
    static void Send(IntPtr host,List<Input> inputs) {
        if(GetAncestor(GetForegroundWindow(),2)!=host)
            throw new InvalidOperationException("Refusing keyboard input: test window is not foreground");
        if(SendInput((uint)inputs.Count,inputs.ToArray(),Marshal.SizeOf<Input>())!=inputs.Count)
            throw new InvalidOperationException("SendInput failed");
    }
    static Input Key(ushort key,uint flags,ushort scan=0) {
        if(key!=0){scan=(ushort)MapVirtualKey(key,0);key=0;flags|=8;}
        return new Input{Type=1,Data=new InputData{Keyboard=new KeyInput{Key=key,Scan=scan,Flags=flags}}};
    }
    public static void Shortcut(IntPtr host,params ushort[] keys) {
        foreach(var key in keys){Send(host,new List<Input>{Key(key,0)});System.Threading.Thread.Sleep(30);}
        for(int i=keys.Length-1;i>=0;i--){Send(host,new List<Input>{Key(keys[i],2)});System.Threading.Thread.Sleep(30);}
    }
    // Posted to one window, so it needs no foreground desktop; modifier state is not faked.
    public static void PostKey(IntPtr hwnd,ushort key) {
        var down=(IntPtr)(1|((int)MapVirtualKey(key,0)<<16));
        PostMessage(hwnd,0x100,(IntPtr)key,down);
        PostMessage(hwnd,0x101,(IntPtr)key,(IntPtr)((long)down|0xC0000000L));
    }
    public static void Text(IntPtr host,string text) {
        var inputs=new List<Input>();
        foreach(char c in text){inputs.Add(Key(0,4,c));inputs.Add(Key(0,6,c));}
        inputs.Add(Key(13,0));inputs.Add(Key(13,2));
        Send(host,inputs);
    }
}
'@

function Assert($condition,[string]$message) {
    if(-not $condition){throw "FAIL: $message"}
    Write-Output "PASS: $message"
}
function Wait-For([scriptblock]$condition,[string]$message) {
    $deadline=[DateTime]::UtcNow.AddSeconds(12)
    do {
        if(& $condition){Write-Output "PASS: $message";return}
        Start-Sleep -Milliseconds 100
    } while([DateTime]::UtcNow -lt $deadline)
    throw "FAIL: $message"
}
# Mirrors Header::new: tabs sit in a track after the brand, followed by the 32-DIP + button and
# its 22-DIP profile dropdown. Three 40-DIP caption buttons sit inside the 8-DIP window margin,
# with the 32-DIP settings button 4 DIPs to their left.
function Get-TabLayout {
    $rect=[TabNative+Rect]::new()
    [void][TabNative]::GetClientRect($script:hostWindow,[ref]$rect)
    $scale=[TabNative]::GetDpiForWindow($script:hostWindow)/96.0
    $px={param($value) [Math]::Round($value*$scale)}
    $brand=if($rect.Right -ge (& $px 780)){& $px 160}else{& $px 52}
    $pad=& $px 3; $gap=& $px 4; $minTab=[Math]::Max(1,(& $px 124)); $control=& $px 32; $nav=& $px 28
    $controlsStart=$rect.Right-(& $px 8)-3*(& $px 40)
    $settingsLeft=$controlsStart-(& $px 4)-$control
    $available=[Math]::Max(1,$settingsLeft-(& $px 48)-$brand-2*$pad-(& $px 6)-$control-(& $px 22))
    $count=@([TabNative]::Children($script:hostWindow)).Count
    $overflow=$count -gt [Math]::Max(1,[Math]::Floor($available/$minTab))
    if($overflow){$available=[Math]::Max(1,$available-2*$nav-(& $px 6))}
    $visible=[Math]::Min($count,[Math]::Max(1,[Math]::Floor($available/$minTab)))
    $step=[Math]::Min([Math]::Floor($available/[Math]::Max(1,$visible)),(& $px 236))
    $trackRight=$brand+$pad+$visible*$step-$gap+$pad
    $plus=$trackRight+(& $px 6)+$(if($overflow){2*$nav}else{0})+$control/2
    [pscustomobject]@{Scale=$scale;FirstTab=$brand+$pad;Step=$step;Plus=$plus;Settings=$settingsLeft+$control/2}
}
function Click-Plus {
    $layout=Get-TabLayout
    [TabNative]::Click($script:hostWindow,[int]$layout.Plus,[int](24*$layout.Scale))
}
function Click-Tab([int]$Index,[switch]$Middle) {
    $layout=Get-TabLayout
    $x=$layout.FirstTab+$Index*$layout.Step+[Math]::Round(40*$layout.Scale)
    [TabNative]::Click($script:hostWindow,[int]$x,[int](24*$layout.Scale),$Middle.IsPresent)
}
function Click-FirstTab([switch]$Middle) { Click-Tab 0 -Middle:$Middle }
# Fullscreen: the host has no frame and the terminal fills the client area below the header.
function Test-Fullscreen {
    $frame=[TabNative+Rect]::new(); $hostRect=[TabNative+Rect]::new(); $childRect=[TabNative+Rect]::new()
    $hostOrigin=[TabNative+Point]::new(); $childOrigin=[TabNative+Point]::new()
    $child=@([TabNative]::Children($script:hostWindow) | Where-Object { [TabNative]::IsWindowVisible($_) })[0]
    [void][TabNative]::GetWindowRect($script:hostWindow,[ref]$frame)
    [void][TabNative]::GetClientRect($script:hostWindow,[ref]$hostRect)
    [void][TabNative]::GetClientRect($child,[ref]$childRect)
    [void][TabNative]::ClientToScreen($script:hostWindow,[ref]$hostOrigin)
    [void][TabNative]::ClientToScreen($child,[ref]$childOrigin)
    $bar=[Math]::Round(48*[TabNative]::GetDpiForWindow($script:hostWindow)/96.0)
    $hostOrigin.X -eq $frame.Left -and $hostOrigin.Y -eq $frame.Top -and
        $hostRect.Right -eq ($frame.Right-$frame.Left) -and $hostRect.Bottom -eq ($frame.Bottom-$frame.Top) -and
        $childOrigin.X -eq $hostOrigin.X -and $childOrigin.Y -eq ($hostOrigin.Y+$bar) -and
        $childRect.Right -eq $hostRect.Right -and $childRect.Bottom -eq ($hostRect.Bottom-$bar)
}
function Send-F11 {
    $child=@([TabNative]::Children($script:hostWindow) | Where-Object { [TabNative]::IsWindowVisible($_) })[0]
    [TabNative]::PostKey($child,0x7A)
}
function Click-Caption([int]$FromRight) {
    $rect=[TabNative+Rect]::new()
    [void][TabNative]::GetClientRect($script:hostWindow,[ref]$rect)
    $scale=[TabNative]::GetDpiForWindow($script:hostWindow)/96.0
    [TabNative]::Click($script:hostWindow,[int]($rect.Right-(8+40*$FromRight+20)*$scale),[int](24*$scale))
}

$configPath=Join-Path $testWork 'test-tabs.toml'
$config=@'
[general]
working_directory = 'C:\dev'
[window]
tabs = true
startup_mode = "Maximized"
[font]
size = 16
normal = { family = "Maple Mono NF CN", style = "Regular" }
[terminal.shell]
program = 'C:\Program Files\PowerShell\7\pwsh.exe'
args = ["-NoLogo", "-NoProfile"]
'@
[IO.File]::WriteAllText($configPath,$config,[Text.UTF8Encoding]::new($false))
$proc=Start-Process -FilePath $Exe -ArgumentList "--config-file `"$configPath`" -vv --print-events" -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput "$testWork\tab-test.log" -RedirectStandardError "$testWork\tab-test-error.log"
$script:hostWindow=[IntPtr]::Zero
try {
    Wait-For { $script:hostWindow=[TabNative]::Host($proc.Id); $script:hostWindow -ne [IntPtr]::Zero } 'tab host becomes visible'
    Wait-For { @([TabNative]::Children($script:hostWindow)).Count -eq 1 } 'first terminal is a child of the tab host'
    Assert ([TabNative]::IsZoomed($script:hostWindow)) 'host opens maximized'
    $frame=[TabNative+Rect]::new()
    $clientOrigin=[TabNative+Point]::new()
    [void][TabNative]::GetWindowRect($script:hostWindow,[ref]$frame)
    [void][TabNative]::ClientToScreen($script:hostWindow,[ref]$clientOrigin)
    # A maximized Win32 outer frame can extend beyond the monitor by its resize margin.
    $scale=[TabNative]::GetDpiForWindow($script:hostWindow)/96.0
    Assert (($clientOrigin.Y-$frame.Top) -lt 16*$scale) 'tabs occupy the title bar without a second native caption'
    $first=[TabNative]::Children($script:hostWindow)[0]
    Click-Plus
    Wait-For { @([TabNative]::Children($script:hostWindow)).Count -eq 2 } 'plus button creates second terminal in same window'
    Click-Plus
    Wait-For { @([TabNative]::Children($script:hostWindow)).Count -eq 3 } 'plus button creates third terminal in same window'
    Wait-For { Select-String -Path "$testWork\tab-test.log" -SimpleMatch 'Windows tabs: 3 terminal(s)' -Quiet } 'third terminal finishes attaching'
    $children=@([TabNative]::Children($script:hostWindow))
    Wait-For { @($children | Where-Object { [TabNative]::IsWindowVisible($_) }).Count -eq 1 } 'only selected tab is visible'
    $scale=[TabNative]::GetDpiForWindow($script:hostWindow)/96.0
    Click-FirstTab
    Wait-For { [TabNative]::IsWindowVisible($first) } 'clicking first tab restores its terminal'
    $layout=Get-TabLayout
    $grab=$layout.FirstTab+[Math]::Round(40*$layout.Scale)
    [TabNative]::Drag($script:hostWindow,[int]$grab,[int]($grab+$layout.Step),[int](24*$layout.Scale))
    Click-Tab 1
    Wait-For { [TabNative]::IsWindowVisible($first) } 'dragging the first tab moves it to the second slot'
    Click-Tab 0
    Wait-For { -not [TabNative]::IsWindowVisible($first) } 'the former second tab now occupies the first slot'
    $layout=Get-TabLayout
    $grab=$layout.FirstTab+$layout.Step+[Math]::Round(40*$layout.Scale)
    [TabNative]::Drag($script:hostWindow,[int]$grab,[int]($grab-$layout.Step),[int](24*$layout.Scale))
    Click-FirstTab
    Wait-For { [TabNative]::IsWindowVisible($first) } 'dragging it back restores the original order'
    # The dropdown beside + lists profiles; Enter on the first one opens it as a tab.
    $layout=Get-TabLayout
    [TabNative]::Click($script:hostWindow,[int]($layout.Plus+[Math]::Round(27*$layout.Scale)),[int](24*$layout.Scale))
    $dropdown=[IntPtr]::Zero
    Wait-For { $script:dropdown=[TabNative]::Find('AlacrittyTabsDropdown'); $script:dropdown -ne [IntPtr]::Zero } 'the profile dropdown opens beside the + button'
    [TabNative]::Key($script:dropdown,0x28)
    [TabNative]::Key($script:dropdown,0x0D)
    Wait-For { @([TabNative]::Children($script:hostWindow)).Count -eq 4 } 'choosing a profile opens it as a tab in the same window'
    Wait-For { [TabNative]::Find('AlacrittyTabsDropdown') -eq [IntPtr]::Zero } 'the dropdown closes after a choice'
    Click-Tab 3 -Middle
    Wait-For { @([TabNative]::Children($script:hostWindow)).Count -eq 3 } 'the profile tab closes independently'
    # The gear left of the caption buttons opens the settings panel.
    $layout=Get-TabLayout
    [TabNative]::Click($script:hostWindow,[int]$layout.Settings,[int](24*$layout.Scale))
    $settingsWindow=[IntPtr]::Zero
    Wait-For { $script:settingsWindow=[TabNative]::Find('AlacrittyTabsSettings'); $script:settingsWindow -ne [IntPtr]::Zero } 'the settings button opens the settings panel'
    [void][TabNative]::PostMessage($script:settingsWindow,0x10,[IntPtr]::Zero,[IntPtr]::Zero)
    Wait-For { [TabNative]::Find('AlacrittyTabsSettings') -eq [IntPtr]::Zero } 'the settings panel closes without changes'
    # F11 in the focused terminal toggles fullscreen of the whole host, keeping the header.
    Send-F11
    Wait-For { Test-Fullscreen } 'F11 makes the terminal fill the host below the header edge to edge'
    Assert (-not [TabNative]::IsZoomed($script:hostWindow)) 'fullscreen replaces the maximized state'
    Send-F11
    Wait-For { [TabNative]::IsZoomed($script:hostWindow) -and -not (Test-Fullscreen) } 'F11 again restores the maximized window'
    Send-F11
    Wait-For { Test-Fullscreen } 'fullscreen can be entered repeatedly'
    Click-Caption 1
    Wait-For { [TabNative]::IsZoomed($script:hostWindow) -and -not (Test-Fullscreen) } 'the header restore button leaves fullscreen'
    if(-not $SkipKeyboard) {
        [void][TabNative]::SetForegroundWindow($script:hostWindow)
        Wait-For { [TabNative]::GetAncestor([TabNative]::GetForegroundWindow(),2) -eq $script:hostWindow } 'keyboard test is restricted to its own foreground window'
        Start-Sleep -Milliseconds 700
        $marker1=Join-Path $testWork ("tab-input-"+[guid]::NewGuid().ToString('N')+'.txt')
        $marker2=Join-Path $testWork ("tab-input-"+[guid]::NewGuid().ToString('N')+'.txt')
        [TabNative]::Text($script:hostWindow,"[IO.File]::WriteAllText('$marker1', [string]`$PID)")
        Wait-For {Test-Path -LiteralPath $marker1} 'typed command reaches the selected PowerShell session'
        [TabNative]::Shortcut($script:hostWindow,[ushort[]]@(0x11,0x09))
        Wait-For {-not [TabNative]::IsWindowVisible($first)} 'Ctrl+Tab switches away from first tab'
        [TabNative]::Text($script:hostWindow,"[IO.File]::WriteAllText('$marker2', [string]`$PID)")
        Wait-For {Test-Path -LiteralPath $marker2} 'typing after switching reaches the new session'
        Assert ((Get-Content -LiteralPath $marker1) -ne (Get-Content -LiteralPath $marker2)) 'tab input is isolated between distinct PowerShell sessions'
        [TabNative]::Shortcut($script:hostWindow,[ushort[]]@(0x11,0x31))
        Wait-For {[TabNative]::IsWindowVisible($first)} 'Ctrl+1 selects first tab before closing any tabs'
        [TabNative]::Shortcut($script:hostWindow,[ushort[]]@(0x11,0x10,0x54))
        Wait-For {@([TabNative]::Children($script:hostWindow)).Count -eq 4} 'Ctrl+Shift+T creates a tab'
        Wait-For {Select-String -Path "$testWork\tab-test.log" -SimpleMatch 'Windows tabs: 4 terminal(s)' -Quiet} 'keyboard-created tab finishes attaching'
        [TabNative]::Shortcut($script:hostWindow,[ushort[]]@(0x11,0x10,0x57))
        Wait-For {@([TabNative]::Children($script:hostWindow)).Count -eq 3} 'Ctrl+Shift+W closes the selected tab'
        [TabNative]::Shortcut($script:hostWindow,[ushort[]]@(0x11,0x31))
        Wait-For {[TabNative]::IsWindowVisible($first)} 'Ctrl+1 selects first tab'
        [TabNative]::Shortcut($script:hostWindow,[ushort[]]@(0x11,0x10,0x55))
        Wait-For {
            @([TabNative]::Children($script:hostWindow)).Count -eq 4 -and
            @(Get-CimInstance Win32_Process -Filter "ParentProcessId = $($proc.Id)" | Where-Object Name -eq 'wsl.exe').Count -eq 1
        } 'Ctrl+Shift+U creates a WSL tab inside the same host'
        [TabNative]::Shortcut($script:hostWindow,[ushort[]]@(0x11,0x10,0x57))
        Wait-For {@([TabNative]::Children($script:hostWindow)).Count -eq 3} 'WSL tab closes independently'
        [TabNative]::Shortcut($script:hostWindow,[ushort[]]@(0x11,0x31))
        Wait-For {[TabNative]::IsWindowVisible($first)} 'PowerShell tab remains available after WSL closes'
    } else { Write-Output "SKIP: keyboard and WSL shortcut checks (no interactive foreground desktop)" }
    if(-not $SkipKeyboard) {
        Add-Type -AssemblyName System.Drawing
        $captureRect=[TabNative+Rect]::new()
        [void][TabNative]::GetWindowRect($script:hostWindow,[ref]$captureRect)
        $bitmap=[Drawing.Bitmap]::new($captureRect.Right-$captureRect.Left,$captureRect.Bottom-$captureRect.Top)
        $graphics=[Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CopyFromScreen($captureRect.Left,$captureRect.Top,0,0,$bitmap.Size)
            $bitmap.Save("$testWork\windows-tabs-preview.png")
        } finally {$graphics.Dispose();$bitmap.Dispose()}
    }
    Click-Caption 1
    Wait-For {-not [TabNative]::IsZoomed($script:hostWindow)} 'header restore button restores the window'
    Click-Caption 1
    Wait-For {[TabNative]::IsZoomed($script:hostWindow)} 'header maximize button maximizes the window'
    Click-Caption 2
    Wait-For {[TabNative]::IsIconic($script:hostWindow)} 'header minimize button minimizes the window'
    [void][TabNative]::ShowWindow($script:hostWindow,9)
    [void][TabNative]::SetForegroundWindow($script:hostWindow)
    if([TabNative]::IsZoomed($script:hostWindow)) {
        Click-Caption 1
        Wait-For {-not [TabNative]::IsZoomed($script:hostWindow)} 'minimized window restores and can return to normal size'
    }
    [void][TabNative]::MoveWindow($script:hostWindow,80,80,1200,850,$true)
    Start-Sleep -Milliseconds 700
    $hostRect=[TabNative+Rect]::new()
    $childRect=[TabNative+Rect]::new()
    [void][TabNative]::GetClientRect($script:hostWindow,[ref]$hostRect)
    [void][TabNative]::GetClientRect($first,[ref]$childRect)
    $scale=[TabNative]::GetDpiForWindow($script:hostWindow)/96.0
    # Each side reserves the 8-DIP window margin plus the 4-DIP inset inside the terminal panel.
    Assert ([Math]::Abs(($hostRect.Right-$childRect.Right)-24*$scale) -le 3) 'terminal width follows host resize and leaves the panel margins'
    Assert ([Math]::Abs(($hostRect.Bottom-$childRect.Bottom)-64*$scale) -le 3) 'one DPI-scaled header and the bottom panel margin are reserved'
    Click-FirstTab -Middle
    Wait-For { @([TabNative]::Children($script:hostWindow)).Count -eq 2 } 'middle click closes one tab'
    Assert (@([TabNative]::Children($script:hostWindow) | Where-Object { [TabNative]::IsWindowVisible($_) }).Count -eq 1) 'closing active tab selects a remaining terminal'
    $shells=@(Get-CimInstance Win32_Process -Filter "ParentProcessId = $($proc.Id)" | Where-Object Name -eq 'pwsh.exe')
    Assert ($shells.Count -eq 2) 'remaining tabs have independent PowerShell processes'
    Assert (-not (Select-String -Path "$testWork\tab-test.log","$testWork\tab-test-error.log" -Pattern '\[ERROR\]|panicked at' -Quiet)) 'startup and tab operations have no logged errors'
    if(-not $KeepOpen){
        Click-Caption 0
        Assert ($proc.WaitForExit(10000)) 'closing the host exits all terminal sessions'
    }
} finally {
    if(-not $proc.HasExited -and [TabNative]::GetAncestor([TabNative]::GetForegroundWindow(),2) -eq $script:hostWindow){
        Add-Type -AssemblyName System.Drawing
        $captureRect=[TabNative+Rect]::new()
        [void][TabNative]::GetWindowRect($script:hostWindow,[ref]$captureRect)
        $bitmap=[Drawing.Bitmap]::new($captureRect.Right-$captureRect.Left,$captureRect.Bottom-$captureRect.Top)
        $graphics=[Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CopyFromScreen($captureRect.Left,$captureRect.Top,0,0,$bitmap.Size)
            $bitmap.Save("$testWork\windows-tabs-last-state.png")
        } finally {$graphics.Dispose();$bitmap.Dispose()}
    }
    if(-not $KeepOpen -and -not $proc.HasExited){
        # Only terminate this isolated test process, never an existing user terminal.
        Stop-Process -Id $proc.Id -Force
    }
}
