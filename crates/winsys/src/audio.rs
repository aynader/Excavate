//! Sound out of a recording or a video with Windows' Media Foundation, as speech models take it: 16 kHz, mono, 32-bit
//! floats. Every format Windows plays (MP3, M4A, WAV, WMA, FLAC, and a video's soundtrack), resampled by Windows.

use crate::wide;
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_FIRST_AUDIO_STREAM, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_VERSION, MFAudioFormat_Float, MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Audio,
    MFSTARTUP_LITE, MFShutdown, MFStartup,
};
use windows::core::PCWSTR;

pub const RATE: u32 = 16_000;

/// `samples`' error for a file with no sound in it (a video without a soundtrack).
pub const NO_SOUND: &str = "no sound in it";

/// Hands `each` the sound a piece at a time (16 kHz mono samples), until it says stop or the sound ends. Says how many
/// seconds it handed over. COM must be initialized on the calling thread.
pub fn samples(path: &str, each: &mut dyn FnMut(&[f32]) -> bool) -> Result<f64, String> {
    let error = |error: windows::core::Error| error.message().to_string();
    let file = wide(path);
    // SAFETY: Media Foundation started and shut down here; its objects created, used and released here; buffers read
    // within their locked length.
    unsafe {
        MFStartup(MF_VERSION, MFSTARTUP_LITE).map_err(error)?;
        let result = (|| -> Result<f64, String> {
            let mut attributes: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attributes, 1).map_err(error)?;
            let attributes = attributes.ok_or("no attributes")?;
            let reader = MFCreateSourceReaderFromURL(PCWSTR(file.as_ptr()), &attributes).map_err(error)?;
            let stream = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false).map_err(error)?;
            reader.SetStreamSelection(stream, true).map_err(|_| NO_SOUND.to_string())?;
            // Windows converts and resamples to what's asked.
            let wanted: IMFMediaType = MFCreateMediaType().map_err(error)?;
            wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(error)?;
            wanted.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float).map_err(error)?;
            wanted.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE).map_err(error)?;
            wanted.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 1).map_err(error)?;
            wanted.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 32).map_err(error)?;
            reader.SetCurrentMediaType(stream, None, &wanted).map_err(error)?;
            let mut handed = 0usize;
            loop {
                let mut flags = 0u32;
                let mut sample = None;
                reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample)).map_err(error)?;
                if let Some(sample) = sample {
                    let buffer = sample.ConvertToContiguousBuffer().map_err(error)?;
                    let (mut data, mut length) = (std::ptr::null_mut(), 0u32);
                    buffer.Lock(&mut data, None, Some(&mut length)).map_err(error)?;
                    // (Copied out: Windows' buffer needn't be aligned for floats.)
                    let bytes = if data.is_null() { &[][..] } else { std::slice::from_raw_parts(data, length as usize) };
                    let floats: Vec<f32> = bytes.chunks_exact(4).map(|four| f32::from_le_bytes([four[0], four[1], four[2], four[3]])).collect();
                    let go_on = each(&floats);
                    handed += floats.len();
                    let _ = buffer.Unlock();
                    if !go_on {
                        break;
                    }
                }
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    break;
                }
            }
            Ok(handed as f64 / f64::from(RATE))
        })();
        let _ = MFShutdown();
        result
    }
}
