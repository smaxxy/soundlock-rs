use cpal::traits::{DeviceTrait, HostTrait};

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;

/// Windows CREATE_NO_WINDOW
///
/// 防止 powershell.exe 弹出黑色控制台窗口。
const CREATE_NO_WINDOW: u32 = 0x08000000;


// ============================================================
// VB-Cable 设备名称检测
// ============================================================

fn is_cable_output_name(name: &str) -> bool {
    let name = name.to_lowercase();

    // 兼容：
    //
    // CABLE Output
    // CABLE Output via Line
    // CABLE Output (VB-Audio Virtual Cable)
    //
    name.contains("cable output")
        || (
            name.contains("vb-audio")
                && name.contains("cable")
                && name.contains("output")
        )
}


fn is_cable_input_name(name: &str) -> bool {
    let name = name.to_lowercase();

    // 兼容：
    //
    // CABLE Input
    // CABLE Input (VB-Audio Virtual Cable)
    //
    name.contains("cable input")
        || (
            name.contains("vb-audio")
                && name.contains("cable")
                && name.contains("input")
        )
}


// ============================================================
// 检测 VB-Cable
// ============================================================

/// 检测 VB-Cable 是否已经安装。
///
/// Sound Lock 捕获的是录音端：
///
/// CABLE Output
/// CABLE Output via Line
///
/// Windows / PUBG 默认播放设备则是：
///
/// CABLE Input
pub fn is_vbcable_installed() -> bool {
    let host = cpal::default_host();

    // --------------------------------------------------------
    // 先检查 Sound Lock 真正要捕获的输入端
    // --------------------------------------------------------

    if let Ok(devices) = host.input_devices() {
        for device in devices {
            let Ok(desc) = device.description() else {
                continue;
            };

            let name = desc.name();

            if is_cable_output_name(name.as_ref()) {
                log::info!(
                    "检测到 VB-Cable 输入端: {}",
                    name
                );

                return true;
            }
        }
    }

    // --------------------------------------------------------
    // 再检查 Windows 播放端
    // --------------------------------------------------------

    if let Ok(devices) = host.output_devices() {
        for device in devices {
            let Ok(desc) = device.description() else {
                continue;
            };

            let name = desc.name();

            if is_cable_input_name(name.as_ref()) {
                log::info!(
                    "检测到 VB-Cable 播放端: {}",
                    name
                );

                return true;
            }
        }
    }

    false
}


// ============================================================
// 获取 VB-Cable 安装程序路径
// ============================================================

/// 当前开发目录：
///
/// D:\soundlock\soundlock-rs\soundlock-rs-main\
/// └─ assets\
///    └─ VBCABLE\
///       ├─ VBCABLE_Setup_x64.exe
///       ├─ *.inf
///       ├─ *.cat
///       ├─ *.sys
///       └─ ...
///
/// 优先使用项目根目录 assets\VBCABLE。
///
/// 同时兼容以后把 assets 文件夹放到 exe 旁边。
fn get_installer_path(
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    // --------------------------------------------------------
    // 方案 1：
    // 项目源码根目录\assets\VBCABLE
    //
    // 你现在自己开发运行时就是走这个。
    // --------------------------------------------------------

    let project_installer =
        PathBuf::from(
            env!("CARGO_MANIFEST_DIR")
        )
        .join("assets")
        .join("VBCABLE")
        .join("VBCABLE_Setup_x64.exe");

    if project_installer.exists() {
        return Ok(project_installer);
    }


    // --------------------------------------------------------
    // 方案 2：
    // exe 所在目录\assets\VBCABLE
    //
    // 以后如果把程序复制出去，
    // 也可以把 assets 一起放到 exe 旁边。
    // --------------------------------------------------------

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let portable_installer =
                exe_dir
                    .join("assets")
                    .join("VBCABLE")
                    .join("VBCABLE_Setup_x64.exe");

            if portable_installer.exists() {
                return Ok(portable_installer);
            }
        }
    }


    Err(
        format!(
            "找不到 VB-Cable 安装程序。\n\n\
             请确认完整驱动文件夹位于：\n\
             {}",
            project_installer.display()
        )
        .into(),
    )
}


// ============================================================
// PowerShell 字符串转义
// ============================================================

fn ps_quote(value: &str) -> String {
    value.replace(
        '\'',
        "''",
    )
}


// ============================================================
// 等待 Windows 注册 VB-Cable 音频端点
// ============================================================

fn wait_for_vbcable(
    timeout: Duration,
) -> bool {
    let interval =
        Duration::from_millis(500);

    let mut elapsed =
        Duration::ZERO;

    while elapsed < timeout {
        if is_vbcable_installed() {
            return true;
        }

        thread::sleep(
            interval,
        );

        elapsed += interval;
    }

    false
}


// ============================================================
// 安装 VB-Cable
// ============================================================

/// 直接使用 assets\VBCABLE 中完整解压后的官方驱动包。
///
/// 不再：
///
/// - 读取 EXE Resource
/// - 写 ZIP
/// - Expand-Archive
/// - TEMP 解压
///
/// PowerShell 自身隐藏运行，不会弹黑色控制台。
pub fn install_vbcable(
) -> Result<(), Box<dyn std::error::Error>> {
    let installer =
        get_installer_path()?;


    log::info!(
        "找到 VB-Cable 安装程序: {}",
        installer.display()
    );


    let installer_ps =
        ps_quote(
            &installer
                .to_string_lossy(),
        );


    // --------------------------------------------------------
    // 只负责启动安装器，不再 -Wait。
    //
    // 否则 VB-Cable 安装器残留的子进程可能导致
    // PowerShell 一直等待，从而阻塞 Sound Lock UI 启动。
    // --------------------------------------------------------

    let script =
        format!(
            r#"
$ErrorActionPreference = 'Stop'

Start-Process `
    -FilePath '{installer_ps}' `
    -ArgumentList '/S' `
    -Verb RunAs

exit 0
"#
        );


    let output =
        Command::new(
            "powershell.exe",
        )
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-WindowStyle")
        .arg("Hidden")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-Command")
        .arg(script)
        .creation_flags(
            CREATE_NO_WINDOW,
        )
        .output()?;


    if !output.status.success() {
        let stdout =
            String::from_utf8_lossy(
                &output.stdout,
            );

        let stderr =
            String::from_utf8_lossy(
                &output.stderr,
            );

        return Err(
            format!(
                "无法启动 VB-Cable 安装程序。\n\
                 ExitCode: {:?}\n\
                 stdout: {}\n\
                 stderr: {}",
                output.status.code(),
                stdout.trim(),
                stderr.trim(),
            )
            .into(),
        );
    }


    // --------------------------------------------------------
    // 不等安装器进程。
    //
    // 我们自己等待 Windows 真正出现 VB-Cable 音频端点。
    // --------------------------------------------------------

    if wait_for_vbcable(
        Duration::from_secs(60),
    ) {
        log::info!(
            "VB-Cable 安装完成，并已检测到音频端点"
        );
    } else {
        // 即使 60 秒没检测到，也不要卡死 Sound Lock。
        // 某些机器安装驱动后需要重启才能出现设备。
        log::warn!(
            "60 秒内未检测到 VB-Cable 音频端点；\
             安装可能需要重新启动 Windows"
        );
    }


    // 无论有没有检测到设备，
    // 都返回 main，让 Sound Lock UI 能继续启动。
    Ok(())
}
// ============================================================
// 设置 Windows 默认播放设备
// ============================================================

/// 将 Windows 默认播放设备设置成名称包含 keyword 的设备。
///
/// 你的 main.rs 继续使用：
///
/// setup::set_default_playback_device("CABLE Input")
///
/// 注意：
///
/// Windows / PUBG 输出：CABLE Input
///
/// Sound Lock 捕获输入：CABLE Output via Line
pub fn set_default_playback_device(
    keyword: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let keyword =
        keyword.replace(
            '\'',
            "''",
        );


    let ps_script =
        format!(
            r#"
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

[ComImport, Guid("870af99c-171d-4f9e-af0d-e63df40c2bc9")]
public class _CPolicyConfigClient {{ }}

[Guid("f8679f50-850a-41cf-9c72-430f290290c8"),
 InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IPolicyConfig
{{
    [PreserveSig]
    int GetMixFormat(
        IntPtr pDevice,
        IntPtr ppFormat
    );

    [PreserveSig]
    int GetDeviceFormat(
        IntPtr pDevice,
        IntPtr pFormat
    );

    [PreserveSig]
    int SetDeviceFormat(
        IntPtr pDevice,
        IntPtr pFormat
    );

    [PreserveSig]
    int GetProcessingPeriod(
        IntPtr pDevice,
        IntPtr pDefaultPeriod,
        IntPtr pMinimumPeriod
    );

    [PreserveSig]
    int SetProcessingPeriod(
        IntPtr pDevice,
        IntPtr pPeriod
    );

    [PreserveSig]
    int GetShareMode(
        IntPtr pDevice,
        IntPtr pMode
    );

    [PreserveSig]
    int SetShareMode(
        IntPtr pDevice,
        IntPtr pMode
    );

    [PreserveSig]
    int GetPropertyValue(
        IntPtr pDevice,
        IntPtr key,
        out IntPtr pValue
    );

    [PreserveSig]
    int SetPropertyValue(
        IntPtr pDevice,
        IntPtr key,
        IntPtr pValue
    );

    [PreserveSig]
    int SetDefaultEndpoint(
        string wszDeviceId,
        int eRole
    );

    [PreserveSig]
    int SetEndpointVisibility(
        IntPtr pDevice,
        int visible
    );
}}
"@

$enumerator =
    New-Object -ComObject MMDeviceEnumerator

$devices =
    $enumerator.EnumAudioEndpoints(
        0,
        1
    )

$found = $false

foreach ($dev in $devices)
{{
    if ($dev.Name -like '*{keyword}*')
    {{
        $pc =
            New-Object _CPolicyConfigClient

        $client =
            [IPolicyConfig]$pc

        # 保持你原来的行为：
        # eMultimedia = 1
        $result =
            $client.SetDefaultEndpoint(
                $dev.Id,
                1
            )

        if ($result -ne 0)
        {{
            throw "SetDefaultEndpoint failed: $result"
        }}

        $found = $true

        break
    }}
}}

if (-not $found)
{{
    throw '未找到 CABLE Input 播放设备'
}}
"#,
            keyword = keyword
        );


    let output =
        Command::new(
            "powershell.exe",
        )
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-WindowStyle")
        .arg("Hidden")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-Command")
        .arg(ps_script)
        .creation_flags(
            CREATE_NO_WINDOW,
        )
        .output()?;


    if output.status.success() {
        Ok(())
    } else {
        let stdout =
            String::from_utf8_lossy(
                &output.stdout,
            );

        let stderr =
            String::from_utf8_lossy(
                &output.stderr,
            );

        Err(
            format!(
                "设置默认播放设备失败。\n\
                 stdout: {}\n\
                 stderr: {}",
                stdout.trim(),
                stderr.trim()
            )
            .into(),
        )
    }
}