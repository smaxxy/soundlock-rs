use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use std::fs::{self, File};
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle, Thread};
use std::time::Duration;

const RECORDER_BUFFER_SECONDS: usize = 2;
pub const MAX_RECORDING_SECONDS: u64 = 120;

const STATE_IDLE: u8 = 0;
const STATE_STARTING: u8 = 1;
const STATE_RECORDING: u8 = 2;
const STATE_SAVING: u8 = 3;
const STATE_SAVED: u8 = 4;
const STATE_ERROR: u8 = 5;

static SESSION_AVAILABLE: AtomicBool = AtomicBool::new(false);
static RECORDING_REQUESTED: AtomicBool = AtomicBool::new(false);
static ACTIVE_INPUT_CALLBACKS: AtomicU32 = AtomicU32::new(0);
static RECORDER_STATE: AtomicU8 = AtomicU8::new(STATE_IDLE);
static SAMPLE_RATE: AtomicU32 = AtomicU32::new(0);
static WRITTEN_FRAMES: AtomicU64 = AtomicU64::new(0);
static DROPPED_FRAMES: AtomicU64 = AtomicU64::new(0);

static LAST_RECORDING_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);
static RECORDER_WORKER: Mutex<Option<Thread>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingState {
    Idle,
    Starting,
    Recording,
    Saving,
    Saved,
    Error,
}

#[derive(Clone, Debug)]
pub struct RecordingSnapshot {
    pub session_available: bool,
    pub requested: bool,
    pub state: RecordingState,
    pub elapsed_seconds: f32,
    pub dropped_frames: u64,
    pub last_path: Option<PathBuf>,
    pub last_error: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct RecordingStatus {
    pub session_available: bool,
    pub requested: bool,
    pub state: RecordingState,
}

/// Input callback owns this producer. It only pushes fixed-size stereo frames
/// into a preallocated SPSC ring buffer; it never locks or performs file I/O.
pub struct RealtimeRecorder {
    producer: HeapProd<(f32, f32)>,
}

impl RealtimeRecorder {
    #[inline]
    pub fn try_push(&mut self, left: f32, right: f32) -> bool {
        self.producer.try_push((left, right)).is_ok()
    }
}

/// The audio supervisor owns this guard. Dropping it stops the recorder worker,
/// drains pending frames, finalizes the WAV header, and joins the worker thread.
pub struct RecorderSession {
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl RecorderSession {
    pub fn mark_available(&self) {
        SESSION_AVAILABLE.store(true, Ordering::SeqCst);
    }
}

impl Drop for RecorderSession {
    fn drop(&mut self) {
        SESSION_AVAILABLE.store(false, Ordering::SeqCst);
        RECORDING_REQUESTED.store(false, Ordering::SeqCst);
        self.shutdown.store(true, Ordering::Release);
        wake_worker();

        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                set_error("原始录音后台线程异常退出".to_owned());
            }
        }
        set_worker(None);

        // request_start() may have raced with teardown. Never leave a stale
        // request or a non-terminal UI state after the only writer has exited.
        RECORDING_REQUESTED.store(false, Ordering::SeqCst);
        if matches!(
            RECORDER_STATE.load(Ordering::Acquire),
            STATE_STARTING | STATE_RECORDING | STATE_SAVING
        ) {
            RECORDER_STATE.store(STATE_IDLE, Ordering::Release);
        }
    }
}

pub fn start_session(sample_rate: u32) -> Option<(RealtimeRecorder, RecorderSession)> {
    SESSION_AVAILABLE.store(false, Ordering::SeqCst);
    RECORDING_REQUESTED.store(false, Ordering::SeqCst);
    RECORDER_STATE.store(STATE_IDLE, Ordering::Release);
    SAMPLE_RATE.store(sample_rate, Ordering::Release);
    ACTIVE_INPUT_CALLBACKS.store(0, Ordering::SeqCst);
    clear_error();

    let capacity = (sample_rate as usize)
        .saturating_mul(RECORDER_BUFFER_SECONDS)
        .max(1);
    let ring = HeapRb::<(f32, f32)>::new(capacity);
    let (producer, consumer) = ring.split();

    let shutdown = Arc::new(AtomicBool::new(false));
    let worker_shutdown = Arc::clone(&shutdown);

    let worker = match thread::Builder::new()
        .name("sound-lock-raw-recorder".to_owned())
        .spawn(move || writer_loop(consumer, sample_rate, worker_shutdown))
    {
        Ok(worker) => worker,
        Err(error) => {
            set_error(format!("无法启动原始录音线程：{error}"));
            return None;
        }
    };
    set_worker(Some(worker.thread().clone()));

    Some((
        RealtimeRecorder { producer },
        RecorderSession {
            shutdown,
            worker: Some(worker),
        },
    ))
}

/// RAII callback token. If the callback observed an active recording, Drop
/// releases the active-callback count even if the callback later gains an
/// early return. No allocation or lock is involved.
pub struct InputCallbackCapture {
    recording: bool,
}

impl InputCallbackCapture {
    #[inline]
    pub fn is_recording(&self) -> bool {
        self.recording
    }
}

impl Drop for InputCallbackCapture {
    #[inline]
    fn drop(&mut self) {
        if self.recording {
            ACTIVE_INPUT_CALLBACKS.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// The active-callback count prevents finalization while a callback which
/// observed an old "recording=true" value may still append frames.
#[inline]
pub fn begin_input_callback() -> InputCallbackCapture {
    if !RECORDING_REQUESTED.load(Ordering::SeqCst) {
        return InputCallbackCapture { recording: false };
    }

    ACTIVE_INPUT_CALLBACKS.fetch_add(1, Ordering::SeqCst);

    // Recheck after joining the active set. If stop raced with the first load,
    // this callback cannot append after the writer has finalized the file.
    if !RECORDING_REQUESTED.load(Ordering::SeqCst) {
        ACTIVE_INPUT_CALLBACKS.fetch_sub(1, Ordering::SeqCst);
        return InputCallbackCapture { recording: false };
    }

    InputCallbackCapture { recording: true }
}

#[inline]
pub fn add_dropped_frames(count: u64) {
    if count != 0 {
        DROPPED_FRAMES.fetch_add(count, Ordering::Relaxed);
    }
}

pub fn request_start() -> Result<(), &'static str> {
    if !SESSION_AVAILABLE.load(Ordering::SeqCst) {
        return Err("请先启动限制，等待状态显示为运行中后再录制");
    }

    if RECORDING_REQUESTED.load(Ordering::SeqCst) {
        return Ok(());
    }

    WRITTEN_FRAMES.store(0, Ordering::Release);
    DROPPED_FRAMES.store(0, Ordering::Release);
    clear_error();
    RECORDER_STATE.store(STATE_STARTING, Ordering::Release);

    if RECORDING_REQUESTED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(());
    }

    // Session teardown may race after the first availability check. Roll back
    // instead of leaving REQUESTED=true after the writer has already exited.
    if !SESSION_AVAILABLE.load(Ordering::SeqCst) {
        RECORDING_REQUESTED.store(false, Ordering::SeqCst);
        RECORDER_STATE.store(STATE_IDLE, Ordering::Release);
        return Err("音频设备正在重连，请稍后再点一次录制");
    }

    wake_worker();

    Ok(())
}

pub fn request_stop() {
    let state = RECORDER_STATE.load(Ordering::Acquire);
    if state == STATE_STARTING || state == STATE_RECORDING {
        RECORDER_STATE.store(STATE_SAVING, Ordering::Release);
    }

    RECORDING_REQUESTED.store(false, Ordering::SeqCst);
    wake_worker();
}

pub fn status() -> RecordingStatus {
    RecordingStatus {
        session_available: SESSION_AVAILABLE.load(Ordering::SeqCst),
        requested: RECORDING_REQUESTED.load(Ordering::SeqCst),
        state: decode_state(RECORDER_STATE.load(Ordering::Acquire)),
    }
}

pub fn snapshot() -> RecordingSnapshot {
    let status = status();
    let sample_rate = SAMPLE_RATE.load(Ordering::Acquire);
    let written_frames = WRITTEN_FRAMES.load(Ordering::Acquire);
    let elapsed_seconds = if sample_rate == 0 {
        0.0
    } else {
        written_frames as f32 / sample_rate as f32
    };

    let last_path = match LAST_RECORDING_PATH.lock() {
        Ok(path) => path.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    let last_error = match LAST_ERROR.lock() {
        Ok(error) => error.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };

    RecordingSnapshot {
        session_available: status.session_available,
        requested: status.requested,
        state: status.state,
        elapsed_seconds,
        dropped_frames: DROPPED_FRAMES.load(Ordering::Acquire),
        last_path,
        last_error,
    }
}

pub fn recordings_dir() -> PathBuf {
    dirs::desktop_dir()
        .or_else(dirs::document_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("SoundLock原始录音")
}

fn writer_loop(mut consumer: HeapCons<(f32, f32)>, sample_rate: u32, shutdown: Arc<AtomicBool>) {
    let maximum_frames = (sample_rate as u64).saturating_mul(MAX_RECORDING_SECONDS);
    let mut active_file: Option<(FloatWavFile, PathBuf)> = None;
    let mut frames_written = 0u64;

    loop {
        let shutting_down = shutdown.load(Ordering::Acquire);
        let requested = RECORDING_REQUESTED.load(Ordering::SeqCst) && !shutting_down;

        let state = RECORDER_STATE.load(Ordering::Acquire);
        let has_pending_frames = !consumer.is_empty();
        let should_create_file = active_file.is_none()
            && (requested
                || (has_pending_frames && matches!(state, STATE_STARTING | STATE_SAVING)));

        if should_create_file {
            match create_recording_file(sample_rate) {
                Ok((writer, path)) => {
                    set_last_path(Some(path.clone()));
                    frames_written = 0;
                    WRITTEN_FRAMES.store(0, Ordering::Release);
                    RECORDER_STATE.store(STATE_RECORDING, Ordering::Release);
                    active_file = Some((writer, path));
                }
                Err(error) => {
                    RECORDING_REQUESTED.store(false, Ordering::SeqCst);
                    SESSION_AVAILABLE.store(false, Ordering::SeqCst);
                    set_error(format!(
                        "无法创建原始录音文件：{error}。请停止并重新启动限制后重试"
                    ));
                }
            }
        }

        let mut write_failed = None;
        if active_file.is_some() {
            while let Some((left, right)) = consumer.try_pop() {
                if let Some((writer, _)) = active_file.as_mut() {
                    if frames_written < maximum_frames {
                        if let Err(error) = writer.write_frame(left, right) {
                            write_failed = Some(error);
                            break;
                        }

                        frames_written = frames_written.saturating_add(1);
                    }
                }
            }
        } else if shutting_down || !SESSION_AVAILABLE.load(Ordering::SeqCst) {
            // No file exists (for example after an I/O error). At teardown the
            // producer is stopped, so pending unusable frames can be discarded
            // without racing a new recording request.
            while consumer.try_pop().is_some() {}
        }
        WRITTEN_FRAMES.store(frames_written, Ordering::Release);

        if let Some(error) = write_failed {
            RECORDING_REQUESTED.store(false, Ordering::SeqCst);
            SESSION_AVAILABLE.store(false, Ordering::SeqCst);

            if let Some((writer, path)) = active_file.take() {
                let _ = writer.finalize();
                let _ = fs::remove_file(path);
            }

            set_error(format!(
                "写入原始录音失败：{error}。请停止并重新启动限制后重试"
            ));
        }

        if frames_written >= maximum_frames && RECORDING_REQUESTED.swap(false, Ordering::SeqCst) {
            RECORDER_STATE.store(STATE_SAVING, Ordering::Release);
        }

        let should_finalize = active_file.is_some()
            && !RECORDING_REQUESTED.load(Ordering::SeqCst)
            && ACTIVE_INPUT_CALLBACKS.load(Ordering::SeqCst) == 0
            && consumer.is_empty();

        if should_finalize {
            if let Some((writer, path)) = active_file.take() {
                match writer.finalize() {
                    Ok(()) => {
                        set_last_path(Some(path));
                        RECORDER_STATE.store(STATE_SAVED, Ordering::Release);
                    }
                    Err(error) => {
                        SESSION_AVAILABLE.store(false, Ordering::SeqCst);
                        let _ = fs::remove_file(path);
                        set_error(format!(
                            "保存原始录音失败：{error}。请停止并重新启动限制后重试"
                        ));
                    }
                }
            }
        } else if active_file.is_none()
            && !RECORDING_REQUESTED.load(Ordering::SeqCst)
            && ACTIVE_INPUT_CALLBACKS.load(Ordering::SeqCst) == 0
            && consumer.is_empty()
            && matches!(
                RECORDER_STATE.load(Ordering::Acquire),
                STATE_STARTING | STATE_SAVING
            )
        {
            // The user may stop during the few milliseconds before the worker
            // creates a file. Return to Idle instead of leaving the UI stuck.
            RECORDER_STATE.store(STATE_IDLE, Ordering::Release);
        }

        if shutting_down
            && active_file.is_none()
            && ACTIVE_INPUT_CALLBACKS.load(Ordering::SeqCst) == 0
            && consumer.is_empty()
        {
            break;
        }

        let idle = active_file.is_none()
            && !RECORDING_REQUESTED.load(Ordering::SeqCst)
            && ACTIVE_INPUT_CALLBACKS.load(Ordering::SeqCst) == 0
            && consumer.is_empty();
        thread::park_timeout(if idle {
            Duration::from_secs(1)
        } else {
            Duration::from_millis(2)
        });
    }
}

fn set_worker(worker: Option<Thread>) {
    match RECORDER_WORKER.lock() {
        Ok(mut current) => *current = worker,
        Err(poisoned) => *poisoned.into_inner() = worker,
    }
}

fn wake_worker() {
    let worker = match RECORDER_WORKER.lock() {
        Ok(current) => current.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };

    if let Some(worker) = worker {
        worker.unpark();
    }
}

fn create_recording_file(sample_rate: u32) -> io::Result<(FloatWavFile, PathBuf)> {
    let directory = recordings_dir();
    fs::create_dir_all(&directory)?;
    let path = next_recording_path(&directory);
    match FloatWavFile::create(&path, sample_rate) {
        Ok(writer) => Ok((writer, path)),
        Err(error) => {
            let _ = fs::remove_file(path);
            Err(error)
        }
    }
}

fn next_recording_path(directory: &Path) -> PathBuf {
    for index in 1..=9999u32 {
        let candidate = directory.join(format!("SoundLock原始录音_{index:04}.wav"));
        if !candidate.exists() {
            return candidate;
        }
    }

    let fallback = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    directory.join(format!("SoundLock原始录音_{fallback}.wav"))
}

fn decode_state(value: u8) -> RecordingState {
    match value {
        STATE_STARTING => RecordingState::Starting,
        STATE_RECORDING => RecordingState::Recording,
        STATE_SAVING => RecordingState::Saving,
        STATE_SAVED => RecordingState::Saved,
        STATE_ERROR => RecordingState::Error,
        _ => RecordingState::Idle,
    }
}

fn set_last_path(path: Option<PathBuf>) {
    match LAST_RECORDING_PATH.lock() {
        Ok(mut last_path) => *last_path = path,
        Err(poisoned) => *poisoned.into_inner() = path,
    }
}

fn clear_error() {
    match LAST_ERROR.lock() {
        Ok(mut error) => *error = None,
        Err(poisoned) => *poisoned.into_inner() = None,
    }
}

fn set_error(message: String) {
    match LAST_ERROR.lock() {
        Ok(mut error) => *error = Some(message),
        Err(poisoned) => *poisoned.into_inner() = Some(message),
    }
    RECORDER_STATE.store(STATE_ERROR, Ordering::Release);
}

/// Minimal stereo IEEE-float WAV writer. It is deliberately kept on the
/// recorder worker thread so neither header work nor disk access enters an
/// audio callback.
struct FloatWavFile {
    writer: BufWriter<File>,
    data_bytes: u64,
}

impl FloatWavFile {
    fn create(path: &Path, sample_rate: u32) -> io::Result<Self> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);

        writer.write_all(b"RIFF")?;
        writer.write_all(&0u32.to_le_bytes())?;
        writer.write_all(b"WAVE")?;
        writer.write_all(b"fmt ")?;
        writer.write_all(&16u32.to_le_bytes())?;
        writer.write_all(&3u16.to_le_bytes())?; // WAVE_FORMAT_IEEE_FLOAT
        writer.write_all(&2u16.to_le_bytes())?;
        writer.write_all(&sample_rate.to_le_bytes())?;
        writer.write_all(&sample_rate.saturating_mul(8).to_le_bytes())?;
        writer.write_all(&8u16.to_le_bytes())?;
        writer.write_all(&32u16.to_le_bytes())?;
        writer.write_all(b"data")?;
        writer.write_all(&0u32.to_le_bytes())?;

        Ok(Self {
            writer,
            data_bytes: 0,
        })
    }

    fn write_frame(&mut self, left: f32, right: f32) -> io::Result<()> {
        let left = if left.is_finite() { left } else { 0.0 };
        let right = if right.is_finite() { right } else { 0.0 };
        self.writer.write_all(&left.to_le_bytes())?;
        self.writer.write_all(&right.to_le_bytes())?;
        self.data_bytes = self.data_bytes.saturating_add(8);
        Ok(())
    }

    fn finalize(mut self) -> io::Result<()> {
        let data_bytes = u32::try_from(self.data_bytes).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "WAV 文件超过 RIFF 4GB 限制")
        })?;
        let riff_size = data_bytes
            .checked_add(36)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "WAV RIFF 长度溢出"))?;

        self.writer.flush()?;
        self.writer.seek(SeekFrom::Start(4))?;
        self.writer.write_all(&riff_size.to_le_bytes())?;
        self.writer.seek(SeekFrom::Start(40))?;
        self.writer.write_all(&data_bytes.to_le_bytes())?;
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_wav_header_and_stereo_payload_are_valid() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "sound_lock_raw_recorder_test_{}_{}.wav",
            std::process::id(),
            unique
        ));

        let mut wav = FloatWavFile::create(&path, 48_000).expect("create test WAV");
        wav.write_frame(0.25, -0.5).expect("write stereo frame");
        wav.finalize().expect("finalize test WAV");

        let bytes = fs::read(&path).expect("read test WAV");
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 44);
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 3);
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 2);
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            48_000
        );
        assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 32);
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
        assert_eq!(f32::from_le_bytes(bytes[44..48].try_into().unwrap()), 0.25);
        assert_eq!(f32::from_le_bytes(bytes[48..52].try_into().unwrap()), -0.5);

        fs::remove_file(path).expect("remove test WAV");
    }
}
