use windows::{
    Win32::Foundation::MAX_PATH, Win32::Media::Audio::*, Win32::System::Com::*,
    Win32::System::Threading::*, core::*,
};

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub name: String,
    pub pid: u32,
    pub session_control: IAudioSessionControl2,
}

pub fn enumerate_sessions() -> Result<Vec<SessionInfo>> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).unwrap();

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
        let session_enum = manager.GetSessionEnumerator()?;
        let count = session_enum.GetCount()?;

        let mut sessions = Vec::new();

        for i in 0..count {
            let session_ctrl: IAudioSessionControl = session_enum.GetSession(i)?;
            let session_ctrl2: IAudioSessionControl2 = session_ctrl.cast()?;

            let pid = session_ctrl2.GetProcessId().unwrap_or(0);
            let name = get_process_name(pid).unwrap_or_else(|_| "Unknown".into());

            sessions.push(SessionInfo {
                name,
                pid,
                session_control: session_ctrl2,
            });
        }

        Ok(sessions)
    }
}

fn get_process_name(pid: u32) -> Result<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)?;

        let mut name = [0u16; MAX_PATH as usize];
        let mut len = name.len() as u32;

        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(name.as_mut_ptr()),
            &mut len,
        )?;

        let path = String::from_utf16_lossy(&name[..len as usize]);

        Ok(path
            .split('\\')
            .next_back()
            .unwrap_or("Unknown")
            .to_string())
    }
}
