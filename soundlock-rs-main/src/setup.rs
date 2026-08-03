use cpal::traits::{DeviceTrait, HostTrait};
use std::path::PathBuf;
use std::process::Command;

/// 检测 VB-Cable 是否已安装（使用 description 代替弃用的 name）
pub fn is_vbcable_installed() -> bool {
    let host = cpal::default_host();
    if let Ok(devices) = host.output_devices() {
        for device in devices {
            if let Ok(desc) = device.description() {
                if desc.name().to_lowercase().contains("cable") {
                    return true;
                }
            }
        }
    }
    false
}

/// 获取安装程序路径
fn get_installer_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.push("assets");
    path.push("VBCABLE_Setup_x64.exe");
    path
}

/// 以管理员权限静默安装 VB-Cable，并重启音频服务
pub fn install_vbcable() -> Result<(), Box<dyn std::error::Error>> {
    let installer = get_installer_path();
    if !installer.exists() {
        return Err(format!("找不到安装文件：{:?}", installer).into());
    }

    // 安装完成后立即重启音频服务（因为已有管理员权限）
    let output = Command::new("powershell")
        .arg("-Command")
        .arg(format!(
            "Start-Process -FilePath '{}' -ArgumentList '/S' -Verb RunAs -Wait; \
             Restart-Service Audiosrv",
            installer.display()
        ))
        .output()?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!("安装程序异常退出: {:?}", output).into())
    }
}

/// 设置默认播放设备为名称中包含 keyword 的设备
/// 通过 PowerShell 调用 COM 接口，完全避开 Rust 的 windows API 版本差异
pub fn set_default_playback_device(keyword: &str) -> Result<(), Box<dyn std::error::Error>> {
    let ps_script = format!(
        r#"
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

[ComImport, Guid("870af99c-171d-4f9e-af0d-e63df40c2bc9")]
public class _CPolicyConfigClient {{ }}

[Guid("f8679f50-850a-41cf-9c72-430f290290c8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IPolicyConfig {{
    [PreserveSig]
    int GetMixFormat(IntPtr pDevice, IntPtr ppFormat);
    [PreserveSig]
    int GetDeviceFormat(IntPtr pDevice, IntPtr pFormat);
    [PreserveSig]
    int SetDeviceFormat(IntPtr pDevice, IntPtr pFormat);
    [PreserveSig]
    int GetProcessingPeriod(IntPtr pDevice, IntPtr pDefaultPeriod, IntPtr pMinimumPeriod);
    [PreserveSig]
    int SetProcessingPeriod(IntPtr pDevice, IntPtr pPeriod);
    [PreserveSig]
    int GetShareMode(IntPtr pDevice, IntPtr pMode);
    [PreserveSig]
    int SetShareMode(IntPtr pDevice, IntPtr pMode);
    [PreserveSig]
    int GetPropertyValue(IntPtr pDevice, IntPtr key, out IntPtr pValue);
    [PreserveSig]
    int SetPropertyValue(IntPtr pDevice, IntPtr key, IntPtr pValue);
    [PreserveSig]
    int SetDefaultEndpoint(string wszDeviceId, int eRole);
    [PreserveSig]
    int SetEndpointVisibility(IntPtr pDevice, int visible);
}}
"@

$enumerator = New-Object -ComObject MMDeviceEnumerator
$devices = $enumerator.EnumAudioEndpoints(0, 1)  # eRender, DEVICE_STATE_ACTIVE
foreach ($dev in $devices) {{
    if ($dev.Name -like '*{keyword}*') {{
        $pc = New-Object _CPolicyConfigClient
        $client = [IPolicyConfig]$pc
        $client.SetDefaultEndpoint($dev.Id, 1)  # eMultimedia = 1
        break
    }}
}}
"#,
        keyword = keyword
    );

    let output = Command::new("powershell")
        .arg("-NoProfile")
        .arg("-Command")
        .arg(ps_script)
        .output()?;

        
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("设置默认设备失败: {}", stderr).into())
    }
}