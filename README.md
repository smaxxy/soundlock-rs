# Sound Lock Rust

This program basically does the same thing as
Sound Lock, but it's open-source and with some
more features which may fix the disadvantages of
the original program.

### Lock Mode

- Windows API Mode  
  This mode uses the Windows API to detect the
  sound level of a certain program at a certain frequency.
  If the sound level is higher than a certain
  threshold, the program will automatically
  reduce the volume of the certain program.

  This mode requires per-app volume control
  (after windows 10 I think) and windows
  api.

  This mode may be laggy in some devices due to
  limits of the Windows API.  
  This mode may not reduce the volume immediately.

- Cable Mode
  This mode detects the sound level of a audio input
  device, does the same thing as the Windows API Mode,
  and then sends the output to a certain audio output.

  This mode requires VB-Cable or similar program to
  reroute the output of your default audio device
  to this program.

  In this mode the volume reduction is immediate.

  Notes:
  VB-Cable's max sample affects the performance largely.
  512 samples doesn't work for me, but 4096 is fine.
  You can adjust the sample rate in the VB-Cable settings.

  The default device's volume setting can affect VB-cable's input. For example, you set the audio output volume
  to 50% and then set the output device to VB-Cable's
  cable-input, the 50% will still apply even if your
  volume set for cable-input is 100%. So if your volume is
  too small, you may need to adjust the volume of the
  default device.

### Warning
This program counts as third party software and may
lead to bans or other legal consequences if used
under some circumstances (e.g. FPS games).
Use at your own risk.