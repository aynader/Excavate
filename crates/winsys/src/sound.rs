//! Playing a file's sound (a recording, or a video's soundtrack) from a moment, through the default output device:
//! decoded and resampled by Media Foundation (as `audio.rs` does for the speech models), played by WASAPI in shared
//! mode. The device's own clock says where the sound is (`Position`), and a playing video keeps to it.

use crate::wide;
use std::sync::atomic::{AtomicU64, Ordering};
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IAudioClock, IAudioRenderClient, ISimpleAudioVolume, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX,
};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, IMFSourceReader, MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_FIRST_AUDIO_STREAM, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_VERSION, MFAudioFormat_Float, MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Audio,
    MFSTARTUP_LITE, MFShutdown, MFStartup,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::Variant::VT_I8;
use windows::core::{GUID, PCWSTR};

/// Where the sound is, in seconds of the file, shared with whoever keeps time by it (a playing video). Not yet known
/// until the device has started.
#[derive(Default)]
pub struct Position(AtomicU64);

impl Position {
    pub fn new() -> Position {
        Position(AtomicU64::new(f64::NAN.to_bits()))
    }

    /// Seconds into the file, once playing.
    pub fn get(&self) -> Option<f64> {
        let seconds = f64::from_bits(self.0.load(Ordering::SeqCst));
        (!seconds.is_nan()).then_some(seconds)
    }

    fn set(&self, seconds: f64) {
        self.0.store(seconds.to_bits(), Ordering::SeqCst);
    }

    pub fn clear(&self) {
        self.set(f64::NAN);
    }
}

/// `play`'s error for a file with no sound in it (a video without a soundtrack).
pub const NO_SOUND: &str = "no sound in it";

/// Plays the sound of `path` from `from` seconds, until `keep_on` says stop or it ends (then after what was handed to
/// the device has been heard). `position` follows it. Says whether it reached the end. COM must be initialized on the
/// calling thread.
pub fn play(path: &str, from: f64, keep_on: &dyn Fn() -> bool, position: &Position) -> Result<bool, String> {
    let error = |error: windows::core::Error| error.message().to_string();
    // SAFETY: Media Foundation started and shut down here; COM objects created, used and released here; buffers
    // written within the length the device gave.
    unsafe {
        MFStartup(MF_VERSION, MFSTARTUP_LITE).map_err(error)?;
        let result = (|| -> Result<bool, String> {
            // The device, in the format it mixes at (its rate), as 32-bit floats in two channels: Windows converts.
            let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(error)?;
            let device = devices.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|_| "no sound device".to_string())?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(error)?;
            let mix = client.GetMixFormat().map_err(error)?;
            let rate = (*mix).nSamplesPerSec;
            CoTaskMemFree(Some(mix as *const _));
            let channels = 2u16;
            let format = WAVEFORMATEX {
                wFormatTag: 3, // IEEE float
                nChannels: channels,
                nSamplesPerSec: rate,
                nAvgBytesPerSec: rate * 4 * channels as u32,
                nBlockAlign: 4 * channels,
                wBitsPerSample: 32,
                cbSize: 0,
            };
            let reader = open(path, rate, channels as u32)?;
            let stream = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
            if from > 0.0 {
                let mut at = PROPVARIANT::default();
                (*at.Anonymous.Anonymous).vt = VT_I8;
                (*at.Anonymous.Anonymous).Anonymous.hVal = (from * 10_000_000.0) as i64;
                let _ = reader.SetCurrentPosition(&GUID::zeroed(), &at);
                let _ = PropVariantClear(&mut at);
            }
            // 100 ms of buffer, refilled each time the device asks.
            client.Initialize(AUDCLNT_SHAREMODE_SHARED,
                              AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                              1_000_000, 0, &format, None).map_err(error)?;
            let event = CreateEventW(None, false, false, PCWSTR::null()).map_err(error)?;
            let played = (|| -> Result<bool, String> {
                client.SetEventHandle(event).map_err(error)?;
                let render: IAudioRenderClient = client.GetService().map_err(error)?;
                // For tests: Excavate's own session silent (the device's clock and the work are the same).
                if std::env::var_os("EXCAVATE_SOUND_SILENT").is_some() {
                    let volume: ISimpleAudioVolume = client.GetService().map_err(error)?;
                    volume.SetMasterVolume(0.0, std::ptr::null()).map_err(error)?;
                }
                let clock: IAudioClock = client.GetService().map_err(error)?;
                let frequency = clock.GetFrequency().map_err(error)? as f64;
                let buffer_frames = client.GetBufferSize().map_err(error)?;
                // Decoded and not yet handed over (interleaved), and where the first decoded sample is in the file.
                let mut pending: std::collections::VecDeque<f32> = std::collections::VecDeque::new();
                let mut first: Option<f64> = None;
                let mut ended = false;
                let mut started = false;
                loop {
                    if !keep_on() {
                        let _ = client.Stop();
                        return Ok(false);
                    }
                    let padding = client.GetCurrentPadding().map_err(error)?;
                    let room = (buffer_frames - padding) as usize;
                    // Enough decoded for the room the device has.
                    while !ended && pending.len() < room * channels as usize {
                        let (mut flags, mut timestamp) = (0u32, 0i64);
                        let mut sample = None;
                        reader.ReadSample(stream, 0, None, Some(&mut flags), Some(&mut timestamp), Some(&mut sample)).map_err(error)?;
                        if let Some(sample) = sample {
                            first.get_or_insert(timestamp as f64 / 10_000_000.0);
                            let buffer = sample.ConvertToContiguousBuffer().map_err(error)?;
                            let (mut data, mut length) = (std::ptr::null_mut(), 0u32);
                            buffer.Lock(&mut data, None, Some(&mut length)).map_err(error)?;
                            if !data.is_null() {
                                let bytes = std::slice::from_raw_parts(data, length as usize);
                                pending.extend(bytes.chunks_exact(4).map(|four| f32::from_le_bytes([four[0], four[1], four[2], four[3]])));
                            }
                            let _ = buffer.Unlock();
                        }
                        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                            ended = true;
                        }
                    }
                    let frames = room.min(pending.len() / channels as usize);
                    if frames > 0 {
                        let out = render.GetBuffer(frames as u32).map_err(error)?;
                        let out = std::slice::from_raw_parts_mut(out as *mut f32, frames * channels as usize);
                        for (slot, value) in out.iter_mut().zip(pending.drain(..frames * channels as usize)) {
                            *slot = value;
                        }
                        render.ReleaseBuffer(frames as u32, 0).map_err(error)?;
                    }
                    if !started {
                        client.Start().map_err(error)?;
                        started = true;
                    }
                    let mut heard = 0u64;
                    clock.GetPosition(&mut heard, None).map_err(error)?;
                    position.set(first.unwrap_or(from) + heard as f64 / frequency);
                    if ended && pending.is_empty() && client.GetCurrentPadding().map_err(error)? == 0 {
                        let _ = client.Stop();
                        return Ok(true);
                    }
                    if WaitForSingleObject(event, 200) != WAIT_OBJECT_0 && !started {
                        return Err("the sound device didn't start".into());
                    }
                }
            })();
            let _ = CloseHandle(event);
            played
        })();
        position.clear();
        let _ = MFShutdown();
        result
    }
}

fn open(path: &str, rate: u32, channels: u32) -> Result<IMFSourceReader, String> {
    let error = |error: windows::core::Error| error.message().to_string();
    let file = wide(path);
    // SAFETY: the reader and its attributes are created here; only the first audio stream is read.
    unsafe {
        let mut attributes: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attributes, 1).map_err(error)?;
        let attributes = attributes.ok_or("no attributes")?;
        let reader = MFCreateSourceReaderFromURL(PCWSTR(file.as_ptr()), &attributes).map_err(error)?;
        let stream = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false).map_err(error)?;
        reader.SetStreamSelection(stream, true).map_err(|_| NO_SOUND.to_string())?;
        let wanted: IMFMediaType = MFCreateMediaType().map_err(error)?;
        wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(error)?;
        wanted.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float).map_err(error)?;
        wanted.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate).map_err(error)?;
        wanted.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels).map_err(error)?;
        wanted.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 32).map_err(error)?;
        reader.SetCurrentMediaType(stream, None, &wanted).map_err(error)?;
        Ok(reader)
    }
}
