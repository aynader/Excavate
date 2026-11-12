//! Frames out of a video with Windows' Media Foundation: every format Windows plays (MP4, MOV, MKV, AVI, WMV, WebM
//! with their codecs installed), decoded by Windows' own decoders, on the graphics card when it can.

use crate::imaging::Rgb;
use crate::wide;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device, ID3D11Multithread,
};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFDXGIDeviceManager, IMFMediaType, IMFSourceReader, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_PD_DURATION,
    MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_D3D_MANAGER,
    MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF_SOURCE_READER_MEDIASOURCE, MF_SOURCE_READERF_ENDOFSTREAM, MF_VERSION, MFCreateAttributes, MFCreateDXGIDeviceManager, MFCreateMediaType,
    MFCreateSourceReaderFromURL, MFMediaType_Video, MFSTARTUP_LITE, MFShutdown, MFStartup, MFVideoFormat_RGB32,
};
use windows::core::Interface;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Variant::VT_I8;
use windows::core::{GUID, PCWSTR};

/// A frame, and where in the video it is.
pub struct Frame {
    pub seconds: f64,
    pub picture: Rgb,
}

/// How long a video is, in seconds, without decoding it. COM must be initialized on the calling thread.
pub fn duration(path: &str) -> Result<f64, String> {
    let _started = Started::new()?;
    let reader = open(path)?;
    duration_of(&reader)
}

/// How long any file Windows plays is (a recording too), in seconds, without decoding it. COM must be initialized on
/// the calling thread.
pub fn length(path: &str) -> Result<f64, String> {
    let _started = Started::new()?;
    let file = wide(path);
    // SAFETY: a reader created here, only asked for its duration.
    let reader = unsafe {
        let mut attributes: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attributes, 1).map_err(|error| error.message().to_string())?;
        let attributes = attributes.ok_or("no attributes")?;
        MFCreateSourceReaderFromURL(PCWSTR(file.as_ptr()), &attributes).map_err(|error| error.message().to_string())?
    };
    duration_of(&reader)
}

/// How long a file Windows plays is, and a video's own size in pixels (none for a recording), from one reader that
/// decodes nothing. COM must be initialized on the calling thread.
pub fn length_and_size(path: &str) -> Result<(f64, Option<(u32, u32)>), String> {
    let _started = Started::new()?;
    let reader = open(path)?;
    let length = duration_of(&reader)?;
    // SAFETY: the reader's native type for its first video stream, read.
    let size = unsafe {
        reader.GetNativeMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, 0).ok()
            .and_then(|native| native.GetUINT64(&MF_MT_FRAME_SIZE).ok())
    };
    let size = size.map(|size| ((size >> 32) as u32, (size & 0xffff_ffff) as u32)).filter(|(width, height)| *width > 0 && *height > 0);
    Ok((length, size))
}

/// A frame every `every` seconds (the nearest Windows can seek to), the first half a step in (or the middle of a
/// shorter video: its first moments are often black or a title), shrunk so its longer side is at most `max_side`,
/// handed to `each` until it says stop. Says how long the video is. COM must be initialized on the calling thread.
///
/// Decoded on the graphics hardware when Windows can (DirectX Video Acceleration, which also shrinks the frames there),
/// else in software. `EXCAVATE_VIDEO_SOFTWARE` set chooses software, to compare.
pub fn frames(path: &str, every: f64, max_side: u32, each: &mut dyn FnMut(Frame) -> bool) -> Result<f64, String> {
    let mut video = Video::open(path, max_side)?;
    let length = video.length;
    let mut at = if length > 0.0 { every.min(length) / 2.0 } else { 0.0 };
    loop {
        if length > 0.0 && at > length {
            break;
        }
        // A frame this close to the moment will do: the keyframe a seek lands on often is, and decoding on from it
        // to the exact moment cost twice the time (an hour of 1080p video: 15.9 s instead of 32.7 s on the graphics
        // hardware). The moment kept is the frame's own.
        match video.at(at, (every / 4.0).min(1.0))? {
            At::End => break,
            At::Unseekable => break,
            At::Frame(frame) => {
                if !each(frame) {
                    break;
                }
            }
            At::Nothing => {}
        }
        at += every;
    }
    Ok(length)
}

/// An open video, for frames at moments asked for (`Video::at`).
pub struct Video {
    // (Dropped in this order: the reader before the device it decodes on, both before Media Foundation stops.)
    reader: IMFSourceReader,
    _device: Option<ID3D11Device>,
    _started: Started,
    width: usize,
    height: usize,
    max_side: u32,
    /// Its length in seconds (0 when Windows doesn't say).
    pub length: f64,
    last: f64,
}

/// What `Video::at` found.
pub enum At {
    Frame(Frame),
    /// A sample without a picture: try the next moment.
    Nothing,
    End,
    /// It can't be sought in.
    Unseekable,
}

impl Video {
    /// Opens a video to take frames from, shrunk so the longer side is at most `max_side`. COM must be initialized on
    /// the calling thread.
    pub fn open(path: &str, max_side: u32) -> Result<Video, String> {
        let started = Started::new()?;
        let hardware = std::env::var_os("EXCAVATE_VIDEO_SOFTWARE").is_none();
        let opened = if hardware { open_hardware(path, max_side).ok() } else { None };
        let (reader, device) = match opened {
            Some(opened) => opened,
            None => (open_software(path)?, None),
        };
        let error = |error: windows::core::Error| error.message().to_string();
        let length = duration_of(&reader).unwrap_or(0.0);
        // SAFETY: the media type the reader decodes to, read.
        let size = unsafe {
            let current = reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32).map_err(error)?;
            current.GetUINT64(&MF_MT_FRAME_SIZE).map_err(error)?
        };
        let (width, height) = ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
        if width == 0 || height == 0 {
            return Err("a video without a picture".into());
        }
        Ok(Video { reader, _device: device, _started: started, width, height, max_side, length, last: -1.0 })
    }

    /// The frame at `seconds`: the first one from `near` seconds before it on (a seek lands on the keyframe before
    /// the moment, and decodes forward from there).
    pub fn at(&mut self, seconds: f64, near: f64) -> Result<At, String> {
        // SAFETY: a position set on the reader owned by self, then cleared.
        unsafe {
            let mut position = PROPVARIANT::default();
            (*position.Anonymous.Anonymous).vt = VT_I8;
            (*position.Anonymous.Anonymous).Anonymous.hVal = (seconds * 10_000_000.0) as i64;
            let sought = self.reader.SetCurrentPosition(&GUID::zeroed(), &position);
            let _ = PropVariantClear(&mut position);
            if sought.is_err() && seconds > 0.0 {
                return Ok(At::Unseekable);
            }
        }
        self.forward(seconds - near)
    }

    /// The keyframe at or before `seconds`: a seek lands on it, and it alone is decoded (the cheapest frame there is).
    pub fn keyframe(&mut self, seconds: f64) -> Result<At, String> {
        self.at(seconds, f64::INFINITY)
    }

    /// The first frame from `seconds` on, decoding forward from where the reader is, without seeking (the frames
    /// passed over are decoded, as they must be, but not copied out).
    pub fn forward(&mut self, seconds: f64) -> Result<At, String> {
        let error = |error: windows::core::Error| error.message().to_string();
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        // SAFETY: Media Foundation objects owned by self; buffers read within their locked length.
        unsafe {
            let target = (seconds.max(0.0) * 10_000_000.0) as i64;
            let (mut flags, mut timestamp) = (0u32, 0i64);
            let mut sample: Option<windows::Win32::Media::MediaFoundation::IMFSample>;
            loop {
                sample = None;
                self.reader.ReadSample(stream, 0, None, Some(&mut flags), Some(&mut timestamp), Some(&mut sample)).map_err(error)?;
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    return Ok(At::End);
                }
                if sample.is_some() && timestamp >= target {
                    break;
                }
            }
            let Some(sample) = sample else { return Ok(At::Nothing) };
            let buffer = sample.ConvertToContiguousBuffer().map_err(error)?;
            let (mut data, mut length) = (std::ptr::null_mut(), 0u32);
            buffer.Lock(&mut data, None, Some(&mut length)).map_err(error)?;
            let picture = shrink(std::slice::from_raw_parts(data, length as usize), self.width, self.height, self.max_side);
            let _ = buffer.Unlock();
            self.last = timestamp as f64 / 10_000_000.0;
            Ok(match picture {
                Some(picture) => At::Frame(Frame { seconds: self.last, picture }),
                None => At::Nothing,
            })
        }
    }

    /// The moment of the last frame handed out (seconds), or below zero before the first.
    pub fn last(&self) -> f64 {
        self.last
    }
}

/// Every frame from `from` seconds on, in order, shrunk so its longer side is at most `max_side`, handed to `each` until
/// it says stop or the video ends (for playing it: the caller keeps the pace by `Frame::seconds`). Says whether it
/// reached the end. Decoded on the graphics hardware when Windows can, else in software. COM must be initialized on the
/// calling thread.
pub fn play(path: &str, from: f64, max_side: u32, each: &mut dyn FnMut(Frame) -> bool) -> Result<bool, String> {
    let _started = Started::new()?;
    let hardware = std::env::var_os("EXCAVATE_VIDEO_SOFTWARE").is_none();
    let opened = if hardware { open_hardware(path, max_side).ok() } else { None };
    let (reader, _device) = match opened {
        Some(opened) => opened,
        None => (open_software(path)?, None),
    };
    let error = |error: windows::core::Error| error.message().to_string();
    // SAFETY: Media Foundation objects created, used and released here; buffers read within their locked length.
    unsafe {
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let current = reader.GetCurrentMediaType(stream).map_err(error)?;
        let size = current.GetUINT64(&MF_MT_FRAME_SIZE).map_err(error)?;
        let (width, height) = ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
        if width == 0 || height == 0 {
            return Err("a video without a picture".into());
        }
        if from > 0.0 {
            let mut position = PROPVARIANT::default();
            (*position.Anonymous.Anonymous).vt = VT_I8;
            (*position.Anonymous.Anonymous).Anonymous.hVal = (from * 10_000_000.0) as i64;
            let _ = reader.SetCurrentPosition(&GUID::zeroed(), &position);
            let _ = PropVariantClear(&mut position);
        }
        // (A seek lands on the keyframe before the moment: what's before it is skipped.)
        let target = ((from - 0.04).max(0.0) * 10_000_000.0) as i64;
        loop {
            let (mut flags, mut timestamp) = (0u32, 0i64);
            let mut sample: Option<windows::Win32::Media::MediaFoundation::IMFSample> = None;
            reader.ReadSample(stream, 0, None, Some(&mut flags), Some(&mut timestamp), Some(&mut sample)).map_err(error)?;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Ok(true);
            }
            let Some(sample) = sample.filter(|_| timestamp >= target) else { continue };
            let buffer = sample.ConvertToContiguousBuffer().map_err(error)?;
            let (mut data, mut length) = (std::ptr::null_mut(), 0u32);
            buffer.Lock(&mut data, None, Some(&mut length)).map_err(error)?;
            let picture = shrink(std::slice::from_raw_parts(data, length as usize), width, height, max_side);
            let _ = buffer.Unlock();
            if let Some(picture) = picture {
                if !each(Frame { seconds: timestamp as f64 / 10_000_000.0, picture }) {
                    return Ok(false);
                }
            }
        }
    }
}

/// What `play_paced` saw: whether it reached the end, and for each frame shown with sound, how far the sound was from
/// it (ms, positive when the sound was ahead).
#[derive(Debug, Default)]
pub struct Paced {
    pub ended: bool,
    pub drift_ms: Vec<f64>,
    pub shown: usize,
    pub skipped: usize,
}

/// `play`, each frame handed to `each` at its moment: by the sound's `position` while it plays (`crate::sound::play`,
/// on another thread), else by the clock from the first frame. A frame more than 100 ms late is skipped. `each` says
/// whether to go on.
pub fn play_paced(path: &str, from: f64, max_side: u32, sound: Option<&crate::sound::Position>,
                  each: &mut dyn FnMut(Frame) -> bool) -> Result<Paced, String> {
    use std::time::{Duration, Instant};
    let mut paced = Paced::default();
    let mut started: Option<(Instant, f64)> = None;
    let mut last_shown: Option<Instant> = None;
    let mut waited_for_sound = Instant::now();
    let ended = play(path, from, max_side, &mut |frame| {
        // With sound: wait for it to start (a moment), then keep to it.
        if let Some(position) = sound {
            while position.get().is_none() && waited_for_sound.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(2));
            }
            if let Some(mut at) = position.get() {
                while frame.seconds - at > 0.004 {
                    std::thread::sleep(Duration::from_secs_f64((frame.seconds - at).min(0.02)));
                    match position.get() {
                        Some(now) => at = now,
                        None => break,
                    }
                }
                if at - frame.seconds > 0.1 {
                    paced.skipped += 1;
                    return true;
                }
                paced.drift_ms.push((at - frame.seconds) * 1000.0);
                paced.shown += 1;
                return each(frame);
            }
            // No sound after all (it ended, or never started): the clock from here.
            waited_for_sound = Instant::now() - Duration::from_secs(10);
        }
        let (start, first) = *started.get_or_insert((Instant::now(), frame.seconds));
        let due = start + Duration::from_secs_f64((frame.seconds - first).max(0.0));
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        } else if now - due > Duration::from_millis(100) {
            // Late: skipped, unless none has been shown for a while (decoding slower than it plays, on a slow PC or at a
            // large size): then this one is, and the clock starts again from it, rather than every frame after being late.
            if last_shown.is_some_and(|shown: Instant| shown.elapsed() < Duration::from_millis(250)) {
                paced.skipped += 1;
                return true;
            }
            started = Some((now, frame.seconds));
        }
        last_shown = Some(Instant::now());
        paced.shown += 1;
        each(frame)
    })?;
    paced.ended = ended;
    Ok(paced)
}

/// Media Foundation, started for this thread's work and shut down after.
struct Started;

impl Started {
    fn new() -> Result<Started, String> {
        // SAFETY: paired with MFShutdown in Drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE).map_err(|error| error.message().to_string())? };
        Ok(Started)
    }
}

impl Drop for Started {
    fn drop(&mut self) {
        // SAFETY: after MFStartup in new.
        unsafe {
            let _ = MFShutdown();
        }
    }
}

fn open(path: &str) -> Result<IMFSourceReader, String> {
    open_with(path, &|_| Ok(()))
}

fn open_with(path: &str, set: &dyn Fn(&IMFAttributes) -> windows::core::Result<()>) -> Result<IMFSourceReader, String> {
    let file = wide(path);
    let error = |error: windows::core::Error| error.message().to_string();
    // SAFETY: the reader and its attributes are created here; only the first video stream is read.
    unsafe {
        let mut attributes: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attributes, 3).map_err(error)?;
        let attributes = attributes.ok_or("no attributes")?;
        set(&attributes).map_err(error)?;
        let reader = MFCreateSourceReaderFromURL(PCWSTR(file.as_ptr()), &attributes).map_err(error)?;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false).map_err(error)?;
        reader.SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, true).map_err(error)?;
        Ok(reader)
    }
}

/// Decoded and converted to RGB by Windows on the processor.
fn open_software(path: &str) -> Result<IMFSourceReader, String> {
    // SAFETY: attributes set on the reader's own attribute store.
    let reader = open_with(path, &|attributes| unsafe { attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1) })?;
    set_rgb(&reader, None)?;
    Ok(reader)
}

/// Decoded, converted to RGB and shrunk on the graphics hardware, copied back small: on the card Excavate runs on
/// (`gpu::capable`), else the adapter Windows draws with (on a laptop, the processor's graphics). The device has to
/// live as long as the reader.
fn open_hardware(path: &str, max_side: u32) -> Result<(IMFSourceReader, Option<ID3D11Device>), String> {
    let error = |error: windows::core::Error| error.message().to_string();
    // SAFETY: a Direct3D device and Media Foundation's manager for it, created here and handed to the reader.
    unsafe {
        let mut device: Option<ID3D11Device> = None;
        let flags = D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        let adapter = crate::gpu::dxgi_adapter().and_then(|adapter| adapter.cast::<windows::Win32::Graphics::Dxgi::IDXGIAdapter>().ok());
        // (A given adapter takes the "unknown" driver type; without one, the default hardware adapter.)
        let made = match &adapter {
            Some(adapter) => D3D11CreateDevice(adapter, windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN, Default::default(), flags,
                                               None, D3D11_SDK_VERSION, Some(&mut device), None, None),
            None => Err(windows::core::Error::empty()),
        };
        if made.is_err() {
            device = None;
            D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, Default::default(), flags, None, D3D11_SDK_VERSION, Some(&mut device), None, None).map_err(error)?;
        }
        let device = device.ok_or("no Direct3D device")?;
        // Media Foundation uses it from its own threads.
        let _previous = device.cast::<ID3D11Multithread>().map_err(error)?.SetMultithreadProtected(true);
        let mut token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(error)?;
        let manager = manager.ok_or("no device manager")?;
        manager.ResetDevice(&device, token).map_err(error)?;
        let reader = open_with(path, &|attributes| {
            attributes.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &manager)?;
            attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)
        })?;
        // Shrunk there to what's wanted, keeping its shape; full size if the hardware won't.
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let native = reader.GetNativeMediaType(stream, 0).map_err(error)?;
        let size = native.GetUINT64(&MF_MT_FRAME_SIZE).map_err(error)?;
        let (width, height) = ((size >> 32) as u32, (size & 0xffff_ffff) as u32);
        let scale = (f64::from(max_side) / f64::from(width.max(height).max(1))).min(1.0);
        let small = ((f64::from(width) * scale / 2.0).round() as u32 * 2, (f64::from(height) * scale / 2.0).round() as u32 * 2);
        if set_rgb(&reader, Some(small)).is_err() {
            set_rgb(&reader, None)?;
        }
        Ok((reader, Some(device)))
    }
}

/// The reader's output: RGB, at a size when given.
fn set_rgb(reader: &IMFSourceReader, size: Option<(u32, u32)>) -> Result<(), String> {
    let error = |error: windows::core::Error| error.message().to_string();
    // SAFETY: a media type created and set here.
    unsafe {
        let wanted: IMFMediaType = MFCreateMediaType().map_err(error)?;
        wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(error)?;
        wanted.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(error)?;
        if let Some((width, height)) = size {
            wanted.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(width) << 32) | u64::from(height)).map_err(error)?;
        }
        reader.SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &wanted).map_err(error)
    }
}

fn duration_of(reader: &IMFSourceReader) -> Result<f64, String> {
    // SAFETY: a value read as the 64-bit number Media Foundation keeps durations in, then cleared.
    unsafe {
        let mut value = reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION).map_err(|error| error.message().to_string())?;
        let hundreds = (*value.Anonymous.Anonymous).Anonymous.uhVal;
        let _ = PropVariantClear(&mut value);
        Ok(hundreds as f64 / 10_000_000.0)
    }
}

/// BGRX rows, top to bottom, to RGB shrunk by whole steps (averaging each block) to fit `max_side`.
fn shrink(bytes: &[u8], width: usize, height: usize, max_side: u32) -> Option<Rgb> {
    if bytes.len() < width * height * 4 {
        return None;
    }
    let step = width.max(height).div_ceil(max_side as usize).max(1);
    let (out_width, out_height) = (width / step, height / step);
    let mut pixels = Vec::with_capacity(out_width * out_height * 3);
    for y in 0..out_height {
        for x in 0..out_width {
            let mut sum = [0u32; 3];
            for dy in 0..step {
                let row = (y * step + dy) * width * 4;
                for dx in 0..step {
                    let at = row + (x * step + dx) * 4;
                    sum[0] += u32::from(bytes[at + 2]);
                    sum[1] += u32::from(bytes[at + 1]);
                    sum[2] += u32::from(bytes[at]);
                }
            }
            let count = (step * step) as u32;
            pixels.extend(sum.iter().map(|value| (value / count) as u8));
        }
    }
    Some(Rgb { width: out_width as u32, height: out_height as u32, pixels, original: (width as u32, height as u32) })
}
