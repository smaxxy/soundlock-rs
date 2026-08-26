use cpal::traits::{DeviceTrait, HostTrait};

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::System::LibraryLoader::{
    FindResourceW,
    GetModuleHandleW,
    LoadResource,
    LockResource,
    SizeofResource,
};

/// app.rc:
///
/// 101 RCDATA "assets/VBCABLE_Driver_Pack45.zip"
const VBCABLE_RESOURCE_ID: usize = 101;

/// Win32 RT_RCDATA = 10
const RT_RCDATA_ID: usize = 10;


// ============================================================
// VB-Cable 检测
// ============================================================

fn is_cable_output_name(name: &str) -> bool {
    let name = name.to_lowercase();

    // 兼容：
    //
    // CABLE Output
    // CABLE Output (VB-Audio Virtual Cable)
    // CABLE Output via Line
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

    // 常见：
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


/// 检测 VB-Cable 是否已经安装。
///
/// Sound Lock 真正需要捕获的是录音端：
///
/// CABLE Output / CABLE Output via Line
///
/// 同时也检查播放端 CABLE Input，避免某些音频 API
/// 只枚举到其中一侧时发生误判。
pub fn is_vbcable_installed() -> bool {
    let host = cpal::default_host();

    // --------------------------------------------------------
    // Recording / Capture side
    // Sound Lock 输入设备需要的就是这一侧。
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
    // Playback side
    // Windows 默认播放设备要设置成这一侧。
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
// 读取 EXE 中内嵌的 ZIP
// ============================================================

fn load_vbcable_zip_resource(
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    unsafe {
        let module = GetModuleHandleW(None)?;

        // Win32 MAKEINTRESOURCE(101)
        let resource_name =
            PCWSTR(VBCABLE_RESOURCE_ID as *const u16);

        // Win32 MAKEINTRESOURCE(RT_RCDATA = 10)
        let resource_type =
            PCWSTR(RT_RCDATA_ID as *const u16);

        let resource = FindResourceW(
            Some(module),
            resource_name,
            resource_type,
        );

        if resource.is_invalid() {
            return Err(
                format!(
                    "Sound Lock.exe 中没有找到 VB-Cable 资源，Resource ID={}",
                    VBCABLE_RESOURCE_ID
                )
                .into(),
            );
        }

        let size = SizeofResource(
            Some(module),
            resource,
        );

        if size == 0 {
            return Err(
                "VB-Cable ZIP 内嵌资源大小为 0"
                    .into(),
            );
        }

        let loaded = LoadResource(
            Some(module),
            resource,
        )?;

        let ptr = LockResource(
            loaded,
        );

        if ptr.is_null() {
            return Err(
                "无法读取 Sound Lock.exe 中的 VB-Cable ZIP 数据"
                    .into(),
            );
        }

        let bytes =
            std::slice::from_raw_parts(
                ptr as *const u8,
                size as usize,
            );

        // 拷贝为 Rust 自己持有的 Vec。
        Ok(bytes.to_vec())
    }
}


// ============================================================
// TEMP
// ============================================================

fn create_temp_directory(
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let root =
        std::env::temp_dir()
            .join(
                format!(
                    "SoundLock_VBCable_{}",
                    std::process::id()
                )
            );

    // 同一个进程 PID 理论上不会冲突。
    // 如果之前异常退出留下目录，就先清掉。
    if root.exists() {
        let _ =
            fs::remove_dir_all(
                &root,
            );
    }

    fs::create_dir_all(
        &root,
    )?;

    Ok(root)
}


// ============================================================
// PowerShell Quote
// ============================================================

fn ps_quote(
    value: &str,
) -> String {
    value.replace(
        '\'',
        "''",
    )
}


// ============================================================
// 解压 ZIP
// ============================================================

fn extract_zip(
    zip_path: &Path,
    destination: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(
        destination,
    )?;

    let zip =
        ps_quote(
            &zip_path
                .to_string_lossy(),
        );

    let destination =
        ps_quote(
            &destination
                .to_string_lossy(),
        );

    let script =
        format!(
            r#"
$ErrorActionPreference = 'Stop'

Expand-Archive `
    -LiteralPath '{zip}' `
    -DestinationPath '{destination}' `
    -Force
"#
        );

    let output =
        Command::new(
            "powershell.exe",
        )
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-Command")
        .arg(script)
        .output()?;


    if output.status.success() {
        Ok(())
    } else {
        let stderr =
            String::from_utf8_lossy(
                &output.stderr,
            );

        Err(
            format!(
                "解压 VB-Cable 驱动包失败: {}",
                stderr.trim()
            )
            .into(),
        )
    }
}


// ============================================================
// 递归寻找安装程序
// ============================================================

fn find_file_recursive(
    directory: &Path,
    target_name: &str,
) -> std::io::Result<Option<PathBuf>> {
    if !directory.is_dir() {
        return Ok(None);
    }

    for entry in fs::read_dir(
        directory,
    )? {
        let entry =
            entry?;

        let path =
            entry.path();

        if path.is_dir() {
            if let Some(found) =
                find_file_recursive(
                    &path,
                    target_name,
                )?
            {
                return Ok(
                    Some(found),
                );
            }

            continue;
        }

        let Some(name) =
            path.file_name()
                .and_then(|v| v.to_str())
        else {
            continue;
        };

        if name.eq_ignore_ascii_case(
            target_name,
        ) {
            return Ok(
                Some(path),
            );
        }
    }

    Ok(None)
}


// ============================================================
// 等待设备注册
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

        elapsed +=
            interval;
    }

    false
}


// ============================================================
// 安装
// ============================================================

/// 从 Sound Lock.exe 中：
///
/// 1. 提取完整 VBCABLE_Driver_Pack45.zip
/// 2. 解压所有文件
/// 3. 递归寻找 VBCABLE_Setup_x64.exe
/// 4. 管理员权限运行安装程序
/// 5. 等待设备注册
/// 6. 删除临时文件
pub fn install_vbcable(
) -> Result<(), Box<dyn std::error::Error>> {
    let root =
        create_temp_directory()?;

    let result =
        install_vbcable_inner(
            &root,
        );

    // 安装程序已经结束以后即可清理。
    //
    // 清理失败不应该覆盖真正的安装结果。
    if let Err(e) =
        fs::remove_dir_all(
            &root,
        )
    {
        log::warn!(
            "清理 VB-Cable 临时目录失败 {}: {}",
            root.display(),
            e
        );
    }

    result
}


fn install_vbcable_inner(
    root: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // --------------------------------------------------------
    // 提取内嵌 ZIP
    // --------------------------------------------------------

    let zip_bytes =
        load_vbcable_zip_resource()?;

    let zip_path =
        root.join(
            "VBCABLE_Driver_Pack45.zip",
        );

    fs::write(
        &zip_path,
        zip_bytes,
    )?;


    // --------------------------------------------------------
    // 完整解压
    // --------------------------------------------------------

    let extracted =
        root.join(
            "package",
        );

    extract_zip(
        &zip_path,
        &extracted,
    )?;


    // --------------------------------------------------------
    // 找 VBCABLE_Setup_x64.exe
    //
    // 不假设 ZIP 根目录结构。
    // --------------------------------------------------------

    let installer =
        find_file_recursive(
            &extracted,
            "VBCABLE_Setup_x64.exe",
        )?
        .ok_or_else(
            || {
                format!(
                    "VB-Cable 驱动包中没有找到 VBCABLE_Setup_x64.exe，解压目录：{}",
                    extracted.display()
                )
            },
        )?;


    log::info!(
        "找到 VB-Cable 安装程序: {}",
        installer.display()
    );


    // --------------------------------------------------------
    // 管理员安装
    // --------------------------------------------------------

    let installer_ps =
        ps_quote(
            &installer
                .to_string_lossy(),
        );


    // 保留你原来使用的 /S 参数。
    //
    // 注意：
    // VB-Audio 官方文档主要描述的是 GUI 安装流程，
    // 并没有在公开安装说明中明确保证 /S。
    //
    // 如果你实际使用的 Pack45 安装程序不接受 /S，
    // 这里删除 -ArgumentList '/S' 即可。
    let script =
        format!(
            r#"
$ErrorActionPreference = 'Stop'

$process = Start-Process `
    -FilePath '{installer_ps}' `
    -ArgumentList '/S' `
    -Verb RunAs `
    -Wait `
    -PassThru

if ($process.ExitCode -ne 0)
{{
    exit $process.ExitCode
}}

exit 0
"#
        );


    let output =
        Command::new(
            "powershell.exe",
        )
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-Command")
        .arg(script)
        .output()?;


    if !output.status.success() {
        let stderr =
            String::from_utf8_lossy(
                &output.stderr,
            );

        return Err(
            format!(
                "VB-Cable 安装程序执行失败，ExitCode={:?}，错误：{}",
                output.status.code(),
                stderr.trim()
            )
            .into(),
        );
    }


    // --------------------------------------------------------
    // 等待 Windows 注册音频端点
    // --------------------------------------------------------

    if wait_for_vbcable(
        Duration::from_secs(10),
    ) {
        log::info!(
            "VB-Cable 已安装并检测到音频端点"
        );
    } else {
        // 官方明确建议安装后重启 Windows。
        //
        // 所以这里不把它当成“安装器失败”。
        log::warn!(
            "VB-Cable 安装程序已完成，但当前尚未检测到 CABLE Input / CABLE Output；可能需要重启 Windows"
        );
    }

    Ok(())
}


// ============================================================
// 设置 Windows 默认播放设备
// ============================================================

/// Windows/PUBG 默认播放设备应设置为：
///
/// CABLE Input
///
/// Sound Lock 自己捕获的是另一侧：
///
/// CABLE Output / CABLE Output via Line
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

$found =
    $false

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

        $found =
            $true

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
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-Command")
        .arg(ps_script)
        .output()?;


    if output.status.success() {
        Ok(())
    } else {
        let stderr =
            String::from_utf8_lossy(
                &output.stderr,
            );

        Err(
            format!(
                "设置默认播放设备失败: {}",
                stderr.trim()
            )
            .into(),
        )
    }
}