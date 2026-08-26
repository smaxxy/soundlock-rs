use cpal::traits::{
    DeviceTrait,
    HostTrait,
};


// ============================================================
// VB-Cable 检测
// ============================================================

fn is_cable_output_name(
    name: &str,
) -> bool {
    let name =
        name.to_lowercase();

    name.contains(
        "cable output",
    )
        || (
            name.contains(
                "vb-audio",
            )
                && name.contains(
                    "cable",
                )
                && name.contains(
                    "output",
                )
        )
}


fn is_cable_input_name(
    name: &str,
) -> bool {
    let name =
        name.to_lowercase();

    name.contains(
        "cable input",
    )
        || (
            name.contains(
                "vb-audio",
            )
                && name.contains(
                    "cable",
                )
                && name.contains(
                    "input",
                )
        )
}


/// 检测 VB-CABLE 是否已经安装。
///
/// Sound Lock 捕获：
///
/// CABLE Output / CABLE Output via Line
///
/// Windows / PUBG 输出：
///
/// CABLE Input
pub fn is_vbcable_installed(
) -> bool {
    let host =
        cpal::default_host();


    // --------------------------------------------------------
    // 录音端
    // --------------------------------------------------------

    if let Ok(devices) =
        host.input_devices()
    {
        for device in devices {
            let Ok(desc) =
                device.description()
            else {
                continue;
            };

            let name =
                desc.name();

            if is_cable_output_name(
                name.as_ref(),
            ) {
                log::info!(
                    "检测到 VB-CABLE 输入端: {}",
                    name
                );

                return true;
            }
        }
    }


    // --------------------------------------------------------
    // 播放端
    // --------------------------------------------------------

    if let Ok(devices) =
        host.output_devices()
    {
        for device in devices {
            let Ok(desc) =
                device.description()
            else {
                continue;
            };

            let name =
                desc.name();

            if is_cable_input_name(
                name.as_ref(),
            ) {
                log::info!(
                    "检测到 VB-CABLE 播放端: {}",
                    name
                );

                return true;
            }
        }
    }


    false
}