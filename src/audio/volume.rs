use windows::{
    Win32::Media::Audio::{Endpoints::IAudioMeterInformation, *},
    core::*,
};

use crate::audio::session::enumerate_sessions;

pub struct VolumeController {
    session_control: IAudioSessionControl2,
    original_volume: f32,
}

impl VolumeController {
    pub fn for_process(pid: u32) -> Option<Self> {
        match enumerate_sessions() {
            Ok(sessions) => {
                for session in sessions {
                    if session.pid == pid {
                        let session_control = session.session_control;
                        let original_volume;

                        unsafe {
                            original_volume = session_control
                                .cast::<ISimpleAudioVolume>()
                                .unwrap()
                                .GetMasterVolume()
                                .unwrap();
                        }

                        return Some(Self {
                            session_control,
                            original_volume,
                        });
                    }
                }

                log::error!("Failed to find session for pid {}", pid);
                None
            }
            Err(e) => {
                log::error!("Failed to enumerate sessions: {}", e);
                None
            }
        }
    }

    pub fn get_current_rms(&self) -> Result<f32> {
        let meter_info: IAudioMeterInformation = self.session_control.cast().unwrap();
        unsafe { meter_info.GetPeakValue() }
    }

    pub fn get_original_volume(&self) -> f32 {
        self.original_volume
    }

    pub fn set_volume(&self, level: f32) -> Result<()> {
        unsafe {
            self.session_control
                .cast::<ISimpleAudioVolume>()
                .unwrap()
                .SetMasterVolume(level, std::ptr::null())?;
            Ok(())
        }
    }

    pub fn restore(&self) -> Result<()> {
        self.set_volume(self.original_volume)
    }
}

impl Drop for VolumeController {
    fn drop(&mut self) {
        self.restore().ok();
        log::debug!("Volume restored to {}", self.original_volume);
    }
}
