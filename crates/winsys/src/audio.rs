//! Sound out of a recording or a video with Windows' Media Foundation, as speech models take it: 16 kHz, mono, 32-bit
//! floats. Every format Windows plays (MP3, M4A, WAV, WMA, FLAC, and a video's soundtrack), resampled by Windows.

use crate::wide;
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, IMFSourceReader, MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
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
    // SAFETY: Media Foundation started and shut down here; its objects created, used and released here; buffers read
    // within their locked length.
    crate::media::check()?;
    unsafe {
        MFStartup(MF_VERSION, MFSTARTUP_LITE).map_err(error)?;
        let result = (|| -> Result<f64, String> {
            let reader = reader(path, RATE, 1)?;
            let stream = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
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
                    let floats: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|four| f32::from_le_bytes([four[0], four[1], four[2], four[3]])).collect();
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

/// A reader of the first sound in `path`, as `rate` Hz, `channels` channels of 32-bit floats: Windows converts and
/// resamples to that. `NO_SOUND` when there's none. Media Foundation must be started.
pub(crate) fn reader(path: &str, rate: u32, channels: u32) -> Result<IMFSourceReader, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAV file of `seconds` of a tone, 16-bit, at `rate` Hz in `channels` channels.
    fn wav(path: &std::path::Path, rate: u32, channels: u16, seconds: u32) {
        let frames = rate * seconds;
        let data = frames * u32::from(channels) * 2;
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend((36 + data).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(channels.to_le_bytes());
        bytes.extend(rate.to_le_bytes());
        bytes.extend((rate * u32::from(channels) * 2).to_le_bytes());
        bytes.extend((channels * 2).to_le_bytes());
        bytes.extend(16u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(data.to_le_bytes());
        for frame in 0..frames {
            let value = ((frame as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 8000.0) as i16;
            for _ in 0..channels {
                bytes.extend(value.to_le_bytes());
            }
        }
        std::fs::write(path, bytes).unwrap();
    }

    /// Any recording comes as 16 kHz mono: two seconds of 44.1 kHz stereo are 32,000 samples, the tone in them.
    #[test]
    fn hands_over_the_sound_resampled() {
        if crate::media::check().is_err() {
            return;
        }
        crate::com_mta();
        let path = std::env::temp_dir().join(format!("excavate-audio-{}.wav", std::process::id()));
        wav(&path, 44_100, 2, 2);
        let (mut count, mut loudest) = (0usize, 0f32);
        let seconds = samples(&path.to_string_lossy(), &mut |piece| {
            count += piece.len();
            loudest = piece.iter().fold(loudest, |loudest, sample| loudest.max(sample.abs()));
            true
        });
        let _ = std::fs::remove_file(&path);
        assert!((seconds.unwrap() - 2.0).abs() < 0.05);
        assert!(count.abs_diff(32_000) < 800);
        assert!(loudest > 0.1);
    }

    #[test]
    fn a_file_that_isnt_sound_is_an_error() {
        if crate::media::check().is_err() {
            return;
        }
        crate::com_mta();
        let path = std::env::temp_dir().join(format!("excavate-audio-{}.txt", std::process::id()));
        std::fs::write(&path, "not a recording").unwrap();
        let result = samples(&path.to_string_lossy(), &mut |_| true);
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err());
    }
}
