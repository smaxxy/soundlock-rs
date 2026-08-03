use windows::{
    Win32::Media::Audio::{Endpoints::IAudioMeterInformation, *},
    core::*,
};


pub struct VolumeController {
    session_control: IAudioSessionControl2,
    original_volume: f32,
}

impl VolumeController {


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
