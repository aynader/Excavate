//! Frames out of a video with Windows' Media Foundation: every format Windows plays (MP4, MOV, MKV, AVI, WMV, WebM
//! with their codecs installed), decoded by Windows' own decoders, on the graphics card when it can.

use crate::imaging::Rgb;
use crate::wide;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_FEATURE_D3D11_OPTIONS2, D3D11_FEATURE_DATA_D3D11_OPTIONS2,
    D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device, ID3D11Multithread,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_GPU_PREFERENCE_MINIMUM_POWER, IDXGIAdapter, IDXGIAdapter1, IDXGIFactory6,
};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFDXGIDeviceManager, IMFMediaType, IMFSourceReader, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_PD_DURATION,
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

/// Where a video's frames are decoded: by Windows' own decoders on a graphics adapter (DirectX Video Acceleration,
/// which also converts and shrinks them there), or on the processor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoder {
    /// The processor's own graphics (a laptop's Intel or AMD graphics), when a graphics card of its own beside them
    /// runs the models: the card is left to the models.
    Integrated,
    /// The card Excavate runs its models on (`gpu::capable`), else the adapter Windows draws with.
    Card,
    /// On the processor.
    Software,
}

impl Decoder {
    pub fn name(self) -> &'static str {
        match self {
            Decoder::Integrated => "integrated",
            Decoder::Card => "card",
            Decoder::Software => "software",
        }
    }

    fn parse(text: &str) -> Option<Decoder> {
        match text.trim().to_ascii_lowercase().as_str() {
            "integrated" | "intel" | "igpu" => Some(Decoder::Integrated),
            "card" | "nvidia" | "gpu" => Some(Decoder::Card),
            "software" | "cpu" => Some(Decoder::Software),
            _ => None,
        }
    }

    /// The ways tried, in order, each one after it if it fails. `integrated_first`: the processor's graphics first
    /// when there's a card beside them (`integrated_device` says no on a PC with one adapter, which goes on as
    /// before), then the card, then software; else the card, then software. To compare: `EXCAVATE_VIDEO_DECODER`
    /// (integrated, card or software) first, with software after it; `EXCAVATE_VIDEO_SOFTWARE` set, software alone.
    pub fn order(integrated_first: bool) -> Vec<Decoder> {
        if let Some(chosen) = std::env::var("EXCAVATE_VIDEO_DECODER").ok().and_then(|chosen| Decoder::parse(&chosen)) {
            return if chosen == Decoder::Software { vec![chosen] } else { vec![chosen, Decoder::Software] };
        }
        if std::env::var_os("EXCAVATE_VIDEO_SOFTWARE").is_some() {
            return vec![Decoder::Software];
        }
        if integrated_first { vec![Decoder::Integrated, Decoder::Card, Decoder::Software] } else { vec![Decoder::Card, Decoder::Software] }
    }
}

/// How near its moment a frame will do, as a share of the step between moments (`frames`), and at most how many
/// seconds before it. A quarter step, at most 1 s: the keyframe a seek lands on often is that near, and decoding on
/// from it to the exact moment cost twice the time (an hour of 1080p video: 15.9 s instead of 32.7 s on the graphics
/// hardware). Half a step (2.5 s) decoded 27 to 30% sooner again but cost MSR-VTT 1.2 points of R@1 and 2.2 of R@10:
/// not kept. `EXCAVATE_VIDEO_NEAR` (a share of the step, without the limit in seconds) tries another, to compare.
const NEAR_SHARE: f64 = 0.25;
const NEAR_MOST: f64 = 1.0;

fn near(every: f64) -> f64 {
    match std::env::var("EXCAVATE_VIDEO_NEAR").ok().and_then(|share| share.trim().parse::<f64>().ok()) {
        Some(share) => every * share.clamp(0.0, 1.0),
        None => (every * NEAR_SHARE).min(NEAR_MOST),
    }
}

/// What `frames` did: how long the video is, where it was decoded (the last way, if one failed partway), and where
/// decoding stalled if it did (`Video`: it went on in software).
#[derive(Clone, Copy, Debug)]
pub struct Decoded {
    pub length: f64,
    pub decoder: Decoder,
    pub stalled: Option<Decoder>,
}

/// A frame every `every` seconds (the nearest Windows can seek to), the first half a step in (or the middle of a
/// shorter video: its first moments are often black or a title), shrunk so its longer side is at most `max_side`,
/// handed to `each` until it says stop. COM must be initialized on the calling thread.
///
/// Decoded on the processor's graphics when there's a graphics card beside them, which is then left to the model
/// looking at the frames: end to end that was 10 to 55% sooner, for the same or less processor time, with the same
/// moments and MSR-VTT within a point. Else as before. The moment kept is the frame's own (at most `near` before the
/// one asked for).
pub fn frames(path: &str, every: f64, max_side: u32, each: &mut dyn FnMut(Frame) -> bool) -> Result<Decoded, String> {
    let mut video = Video::open_with(path, max_side, Decoder::order(true))?;
    let length = video.length;
    let near = near(every);
    let mut at = if length > 0.0 { every.min(length) / 2.0 } else { 0.0 };
    loop {
        if length > 0.0 && at > length {
            break;
        }
        match video.at(at, near)? {
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
    Ok(Decoded { length, decoder: video.decoder, stalled: video.stalled })
}

/// How long a decoder may take over one answer before it's let go: twice the graphics card's decoder never returned
/// (one run with both video lanes on the card; a frame takes well under a second, a long seek a few).
pub const STALL: Duration = Duration::from_secs(60);

/// An open video, for frames at moments asked for (`Video::at`), decoded on a thread of its own: a decoder that
/// doesn't answer within `STALL` is let go (its thread ends if its call ever returns) and the video opened again in
/// software, at the same place. Media Foundation's reads have no time limit of their own.
pub struct Video {
    asks: Sender<Ask>,
    answers: Receiver<Answer>,
    path: String,
    max_side: u32,
    stall: Duration,
    /// Its length in seconds (0 when Windows doesn't say).
    pub length: f64,
    /// Where it's decoded now.
    pub decoder: Decoder,
    /// Where decoding stalled, if it did: decoded in software since.
    pub stalled: Option<Decoder>,
    last: f64,
}

#[derive(Clone, Copy, Debug)]
enum Ask {
    At(f64, f64),
    Forward(f64),
    Seek(f64),
}

struct Answer {
    found: Result<At, String>,
    decoder: Decoder,
    last: f64,
}

/// A decoding thread's ends: asks in, answers out, and what it opened (length, decoder).
type Ends = (Sender<Ask>, Receiver<Answer>, f64, Decoder);

impl Video {
    /// Opens a video to take frames from, shrunk so the longer side is at most `max_side`: on the graphics card, else
    /// in software (`Decoder::order(false)`). Reading text in videos decodes this way: on the processor's graphics it
    /// was sooner, but took more processor time, which is what that lane is measured by.
    pub fn open(path: &str, max_side: u32) -> Result<Video, String> {
        Video::open_with(path, max_side, Decoder::order(false))
    }

    /// The same, decoded the first of `ways` that opens it, the others after it if it fails.
    pub fn open_with(path: &str, max_side: u32, ways: Vec<Decoder>) -> Result<Video, String> {
        Video::watched(path, max_side, ways, STALL)
    }

    fn watched(path: &str, max_side: u32, ways: Vec<Decoder>, stall: Duration) -> Result<Video, String> {
        let (asks, answers, length, decoder) = match Video::start(path, max_side, ways, stall) {
            Err(Stalled(Some(error))) => return Err(error),
            // (Opening stalled: in software, if it wasn't that already.)
            Err(Stalled(None)) => Video::start(path, max_side, vec![Decoder::Software], stall)
                .map_err(|failed| failed.0.unwrap_or_else(|| "opening it stalled".into()))?,
            Ok(started) => started,
        };
        Ok(Video { asks, answers, path: path.to_string(), max_side, stall, length, decoder, stalled: None, last: -1.0 })
    }

    /// A thread that opens the video and answers asks. `Stalled(None)`: it didn't open within `stall`.
    fn start(path: &str, max_side: u32, ways: Vec<Decoder>, stall: Duration) -> Result<Ends, Stalled> {
        let (asks, asked) = std::sync::mpsc::channel::<Ask>();
        let (answer, answers) = std::sync::mpsc::channel::<Answer>();
        let (opened_send, opened) = std::sync::mpsc::channel::<Result<(f64, Decoder), String>>();
        let path_ = path.to_string();
        std::thread::Builder::new().name("video's decoder".into()).spawn(move || {
            crate::com_mta();
            let mut video = match Decoding::open_with(&path_, max_side, ways) {
                Ok(video) => video,
                Err(error) => {
                    let _ = opened_send.send(Err(error));
                    return;
                }
            };
            if opened_send.send(Ok((video.length, video.decoder))).is_err() {
                return;
            }
            for ask in asked {
                let found = match ask {
                    Ask::At(seconds, near) => video.at(seconds, near),
                    Ask::Forward(seconds) => video.forward(seconds),
                    Ask::Seek(seconds) => {
                        video.seek(seconds);
                        Ok(At::Nothing)
                    }
                };
                if answer.send(Answer { found, decoder: video.decoder, last: video.last }).is_err() {
                    break;
                }
            }
        }).map_err(|error| Stalled(Some(error.to_string())))?;
        match opened.recv_timeout(stall) {
            Ok(Ok((length, decoder))) => Ok((asks, answers, length, decoder)),
            Ok(Err(error)) => Err(Stalled(Some(error))),
            Err(RecvTimeoutError::Disconnected) => Err(Stalled(Some("the video's decoder stopped".into()))),
            Err(RecvTimeoutError::Timeout) => Err(Stalled(None)),
        }
    }

    fn ask(&mut self, ask: Ask) -> Result<At, String> {
        loop {
            if self.asks.send(ask).is_err() {
                return Err("the video's decoder stopped".into());
            }
            match self.answers.recv_timeout(self.stall) {
                Ok(answer) => {
                    (self.decoder, self.last) = (answer.decoder, answer.last);
                    return answer.found;
                }
                Err(RecvTimeoutError::Disconnected) => return Err("the video's decoder stopped".into()),
                Err(RecvTimeoutError::Timeout) if self.decoder == Decoder::Software => {
                    return Err(format!("decoding stalled ({} s without a frame, in software)", self.stall.as_secs()));
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Let go (it ends if its call returns: its answer finds no one); again in software.
                    self.stalled = Some(self.decoder);
                    let (asks, answers, _, decoder) = Video::start(&self.path, self.max_side, vec![Decoder::Software], self.stall)
                        .map_err(|failed| failed.0.unwrap_or_else(|| "opening it again stalled".into()))?;
                    (self.asks, self.answers, self.decoder) = (asks, answers, decoder);
                    // (Decoding forward goes on from the keyframe before the last frame handed out; `at` seeks itself.)
                    if matches!(ask, Ask::Forward(_)) && self.last > 0.0 {
                        let _ = self.asks.send(Ask::Seek(self.last));
                        let _ = self.answers.recv_timeout(self.stall);
                    }
                }
            }
        }
    }

    /// The frame at `seconds`: the first one from `near` seconds before it on (a seek lands on the keyframe before
    /// the moment, and decodes forward from there). On an error, again the next way.
    pub fn at(&mut self, seconds: f64, near: f64) -> Result<At, String> {
        self.ask(Ask::At(seconds, near))
    }

    /// The keyframe at or before `seconds`: a seek lands on it, and it alone is decoded (the cheapest frame there is).
    pub fn keyframe(&mut self, seconds: f64) -> Result<At, String> {
        self.at(seconds, f64::INFINITY)
    }

    /// The first frame from `seconds` on, decoding forward from where the reader is, without seeking. On an error, the
    /// next way, from where this one was.
    pub fn forward(&mut self, seconds: f64) -> Result<At, String> {
        self.ask(Ask::Forward(seconds))
    }

    /// The moment of the last frame handed out (seconds), or below zero before the first.
    pub fn last(&self) -> f64 {
        self.last
    }
}

/// For the tests: decoding on graphics hardware hangs, as it did twice on a card.
#[cfg(test)]
static HANG_ON_HARDWARE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Why a decoding thread didn't start: an error, or none within the time (`None`).
struct Stalled(Option<String>);

/// An open video, decoded where it's asked: `Video`'s thread owns one.
struct Decoding {
    // (Dropped in this order: the reader before the device it decodes on, both before Media Foundation stops.)
    reader: IMFSourceReader,
    _device: Option<ID3D11Device>,
    _started: Started,
    width: usize,
    height: usize,
    /// Its pixels' shape, width over height (`pixel_aspect`), when they come out as stored.
    aspect: f64,
    path: String,
    max_side: u32,
    /// Its length in seconds (0 when Windows doesn't say).
    pub length: f64,
    last: f64,
    /// Where it's decoded now.
    pub decoder: Decoder,
    /// The ways left to try if this one fails.
    rest: Vec<Decoder>,
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

impl Decoding {
    /// Decoded the first of `ways` that opens it, the others after it if it fails. COM must be initialized on the
    /// calling thread.
    fn open_with(path: &str, max_side: u32, ways: Vec<Decoder>) -> Result<Decoding, String> {
        let started = Started::new()?;
        let mut rest = ways;
        let (reader, device, decoder, (width, height), aspect) = open_first(path, max_side, &mut rest)?;
        let length = duration_of(&reader).unwrap_or(0.0);
        Ok(Decoding { reader, _device: device, _started: started, width, height, aspect, path: path.to_string(), max_side, length, last: -1.0, decoder, rest })
    }

    /// The next way to decode it, after this one failed: false when there's none left.
    fn fall_back(&mut self) -> bool {
        match open_first(&self.path, self.max_side, &mut self.rest) {
            Ok((reader, device, decoder, (width, height), aspect)) => {
                self.reader = reader;
                self._device = device;
                (self.decoder, self.width, self.height, self.aspect) = (decoder, width, height, aspect);
                true
            }
            Err(_) => false,
        }
    }

    /// The frame at `seconds` (`Video::at`). On an error, again the next way (`fall_back`).
    fn at(&mut self, seconds: f64, near: f64) -> Result<At, String> {
        #[cfg(test)]
        if HANG_ON_HARDWARE.load(std::sync::atomic::Ordering::Relaxed) && self.decoder != Decoder::Software {
            std::thread::sleep(Duration::from_secs(3600));
        }
        loop {
            let found = if self.seek(seconds) { self.read_from(seconds - near) } else { Ok(At::Unseekable) };
            match found {
                Err(error) if !self.fall_back() => return Err(error),
                Err(_) => {}
                found => return found,
            }
        }
    }

    /// Sets the reader at `seconds` (it lands on the keyframe at or before it). False when it can't be sought in.
    fn seek(&mut self, seconds: f64) -> bool {
        // SAFETY: a position set on the reader owned by self, then cleared.
        unsafe {
            let mut position = PROPVARIANT::default();
            (*position.Anonymous.Anonymous).vt = VT_I8;
            (*position.Anonymous.Anonymous).Anonymous.hVal = (seconds * 10_000_000.0) as i64;
            let sought = self.reader.SetCurrentPosition(&GUID::zeroed(), &position);
            let _ = PropVariantClear(&mut position);
            sought.is_ok() || seconds <= 0.0
        }
    }

    /// The first frame from `seconds` on, decoding forward from where the reader is, without seeking (the frames
    /// passed over are decoded, as they must be, but not copied out). On an error, the next way, from where this one
    /// was (`fall_back`).
    fn forward(&mut self, seconds: f64) -> Result<At, String> {
        loop {
            match self.read_from(seconds) {
                Err(error) if !self.fall_back() => return Err(error),
                // (A new reader starts at the beginning: it's set at the keyframe before the last frame handed out.)
                Err(_) => {
                    if self.last > 0.0 {
                        self.seek(self.last);
                    }
                }
                found => return found,
            }
        }
    }

    fn read_from(&mut self, seconds: f64) -> Result<At, String> {
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
            let picture = shrink(std::slice::from_raw_parts(data, length as usize), self.width, self.height, self.max_side, self.aspect);
            let _ = buffer.Unlock();
            self.last = timestamp as f64 / 10_000_000.0;
            Ok(match picture {
                Some(picture) => At::Frame(Frame { seconds: self.last, picture }),
                None => At::Nothing,
            })
        }
    }
}

/// A reader, the device it decodes on, where that is, the size it hands frames out at and their pixels' shape.
type Opened = (IMFSourceReader, Option<ID3D11Device>, Decoder, (usize, usize), f64);

/// The first way in `rest` that opens `path` (taken off it, as are those that failed before it).
fn open_first(path: &str, max_side: u32, rest: &mut Vec<Decoder>) -> Result<Opened, String> {
    let mut failed = String::from("no way to decode it");
    while !rest.is_empty() {
        let decoder = rest.remove(0);
        let opened = match decoder {
            Decoder::Software => open_software(path).map(|reader| {
                let aspect = pixel_aspect(&reader);
                (reader, None, aspect)
            }),
            Decoder::Card => card_device().and_then(|device| open_hardware(path, max_side, device)),
            Decoder::Integrated => integrated_device().and_then(|device| open_hardware(path, max_side, device)),
        };
        match opened.and_then(|(reader, device, aspect)| frame_size(&reader).map(|size| (reader, device, decoder, size, aspect))) {
            Ok(opened) => return Ok(opened),
            Err(error) => failed = error,
        }
    }
    Err(failed)
}

/// The size the reader hands frames out at.
fn frame_size(reader: &IMFSourceReader) -> Result<(usize, usize), String> {
    let error = |error: windows::core::Error| error.message().to_string();
    // SAFETY: the media type the reader decodes to, read.
    let size = unsafe {
        let current = reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32).map_err(error)?;
        current.GetUINT64(&MF_MT_FRAME_SIZE).map_err(error)?
    };
    let (width, height) = ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
    if width == 0 || height == 0 {
        return Err("a video without a picture".into());
    }
    Ok((width, height))
}

/// Every frame from `from` seconds on, in order, shrunk so its longer side is at most `max_side`, handed to `each` until
/// it says stop or the video ends (for playing it: the caller keeps the pace by `Frame::seconds`). Says whether it
/// reached the end. Decoded on the graphics card when Windows can (`Decoder::Card`: where the person's screen is drawn
/// on a laptop, not measured on the processor's graphics), else in software. COM must be initialized on the calling
/// thread.
pub fn play(path: &str, from: f64, max_side: u32, each: &mut dyn FnMut(Frame) -> bool) -> Result<bool, String> {
    let _started = Started::new()?;
    let hardware = std::env::var_os("EXCAVATE_VIDEO_SOFTWARE").is_none();
    let opened = if hardware { card_device().and_then(|device| open_hardware(path, max_side, device)).ok() } else { None };
    let (reader, _device, aspect) = match opened {
        Some(opened) => opened,
        None => {
            let reader = open_software(path)?;
            let aspect = pixel_aspect(&reader);
            (reader, None, aspect)
        }
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
            let picture = shrink(std::slice::from_raw_parts(data, length as usize), width, height, max_side, aspect);
            let _ = buffer.Unlock();
            if let Some(picture) = picture
                && !each(Frame { seconds: timestamp as f64 / 10_000_000.0, picture }) {
                    return Ok(false);
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
        crate::media::check()?;
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

/// A Direct3D device for decoding on `adapter`, or on Windows' default hardware adapter without one.
fn device_on(adapter: Option<&IDXGIAdapter>) -> Result<ID3D11Device, String> {
    let flags = D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT;
    let mut device: Option<ID3D11Device> = None;
    // SAFETY: a device created here, handed back.
    unsafe {
        // (A given adapter takes the "unknown" driver type; without one, the default hardware adapter.)
        let driver = if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE };
        D3D11CreateDevice(adapter, driver, Default::default(), flags, None, D3D11_SDK_VERSION, Some(&mut device), None, None)
            .map_err(|error| error.message().to_string())?;
    }
    device.ok_or_else(|| "no Direct3D device".to_string())
}

/// On the card Excavate runs its models on (`gpu::capable`), else the adapter Windows draws with.
fn card_device() -> Result<ID3D11Device, String> {
    let adapter = crate::gpu::dxgi_adapter().and_then(|adapter| adapter.cast::<IDXGIAdapter>().ok());
    adapter.and_then(|adapter| device_on(Some(&adapter)).ok()).map_or_else(|| device_on(None), Ok)
}

/// On the processor's own graphics, when the models have a card of their own beside them: Windows' power-saving
/// adapter, when it isn't that card and shares the processor's memory (Direct3D's "unified memory": an integrated
/// one, not a second card). An error otherwise, and on a PC with one adapter.
fn integrated_device() -> Result<ID3D11Device, String> {
    let error = |error: windows::core::Error| error.message().to_string();
    let card = crate::gpu::capable().ok_or("no graphics card of its own for the models")?;
    // SAFETY: a DXGI factory and adapter, asked and released here; a feature check into a struct of its size.
    unsafe {
        let factory = CreateDXGIFactory1::<IDXGIFactory6>().map_err(error)?;
        let adapter: IDXGIAdapter1 = factory.EnumAdapterByGpuPreference(0, DXGI_GPU_PREFERENCE_MINIMUM_POWER).map_err(error)?;
        let description = adapter.GetDesc1().map_err(error)?;
        if description.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 || crate::gpu::luid_name(description.AdapterLuid) == card.luid {
            return Err("no graphics beside the card".into());
        }
        let device = device_on(Some(&adapter.cast::<IDXGIAdapter>().map_err(error)?))?;
        let mut options = D3D11_FEATURE_DATA_D3D11_OPTIONS2::default();
        device.CheckFeatureSupport(D3D11_FEATURE_D3D11_OPTIONS2, (&raw mut options).cast(), size_of::<D3D11_FEATURE_DATA_D3D11_OPTIONS2>() as u32)
            .map_err(error)?;
        if !options.UnifiedMemoryArchitecture.as_bool() {
            return Err("the power-saving adapter is a card of its own".into());
        }
        Ok(device)
    }
}

/// Decoded, converted to RGB and shrunk on the graphics hardware of `device`, copied back small. The device has to
/// live as long as the reader.
fn open_hardware(path: &str, max_side: u32, device: ID3D11Device) -> Result<(IMFSourceReader, Option<ID3D11Device>, f64), String> {
    let error = |error: windows::core::Error| error.message().to_string();
    // SAFETY: Media Foundation's manager for the device, created here and handed to the reader.
    unsafe {
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
        // Shrunk there to what's wanted, keeping its shape as shown (a video of wide pixels, 16:9 stored as 320 by
        // 240, is 427 by 240: asked for its stored shape, the hardware letterboxed it, black above and below); full
        // size if the hardware won't.
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let native = reader.GetNativeMediaType(stream, 0).map_err(error)?;
        let size = native.GetUINT64(&MF_MT_FRAME_SIZE).map_err(error)?;
        let aspect = pixel_aspect(&reader);
        let (width, height) = (((size >> 32) as f64 * aspect).round() as u32, (size & 0xffff_ffff) as u32);
        let scale = (f64::from(max_side) / f64::from(width.max(height).max(1))).min(1.0);
        let small = ((f64::from(width) * scale / 2.0).round() as u32 * 2, (f64::from(height) * scale / 2.0).round() as u32 * 2);
        if set_rgb(&reader, Some(small)).is_ok() {
            return Ok((reader, Some(device), 1.0));
        }
        set_rgb(&reader, None)?;
        Ok((reader, Some(device), aspect))
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
            wanted.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1).map_err(error)?;
        }
        reader.SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &wanted).map_err(error)
    }
}

/// The shape of a video's stored pixels, width over height: 1 but in videos stored narrower or wider than shown
/// (MSR-VTT has 16:9 clips stored as 4:3). Out of 1/4 to 4, or not said: 1.
fn pixel_aspect(reader: &IMFSourceReader) -> f64 {
    // SAFETY: the reader's native type for its first video stream, read.
    let packed = unsafe {
        reader.GetNativeMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, 0).ok()
            .and_then(|native| native.GetUINT64(&MF_MT_PIXEL_ASPECT_RATIO).ok())
    };
    let aspect = packed.map_or(1.0, |packed| {
        let (across, down) = ((packed >> 32) as u32, (packed & 0xffff_ffff) as u32);
        if down == 0 { 1.0 } else { f64::from(across) / f64::from(down) }
    });
    if (0.25..=4.0).contains(&aspect) { aspect } else { 1.0 }
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

/// BGRX rows, top to bottom, to RGB shrunk by whole steps (averaging each block) to fit `max_side`, at the shape it's
/// shown at: pixels `aspect` times as wide as high (`pixel_aspect`) come out square, stored columns repeated or
/// averaged.
fn shrink(bytes: &[u8], width: usize, height: usize, max_side: u32, aspect: f64) -> Option<Rgb> {
    if width == 0 || bytes.len() < width * height * 4 {
        return None;
    }
    let shown = ((width as f64 * aspect).round() as usize).max(1);
    let step = shown.max(height).div_ceil(max_side as usize).max(1);
    let (out_width, out_height) = (shown / step, height / step);
    // The stored columns an output column averages: `step` of them when the pixels are square.
    let columns = |x: usize| {
        let first = x * step * width / shown;
        first..((x + 1) * step * width / shown).clamp(first + 1, width)
    };
    let mut pixels = Vec::with_capacity(out_width * out_height * 3);
    for y in 0..out_height {
        for x in 0..out_width {
            let mut sum = [0u32; 3];
            let across = columns(x);
            for dy in 0..step {
                let row = (y * step + dy) * width * 4;
                for column in across.clone() {
                    let at = row + column * 4;
                    sum[0] += u32::from(bytes[at + 2]);
                    sum[1] += u32::from(bytes[at + 1]);
                    sum[2] += u32::from(bytes[at]);
                }
            }
            let count = (step * across.len()) as u32;
            pixels.extend(sum.iter().map(|value| (value / count) as u8));
        }
    }
    Some(Rgb { width: out_width as u32, height: out_height as u32, pixels, original: (shown as u32, height as u32) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_within_a_quarter_step_at_most_a_second() {
        if std::env::var_os("EXCAVATE_VIDEO_NEAR").is_some() {
            return;
        }
        assert_eq!(near(2.0), 0.5);
        assert_eq!(near(5.0), 1.0);
        assert_eq!(near(30.0), 1.0);
    }

    #[test]
    fn ways_to_decode_by_name() {
        assert_eq!(Decoder::parse(" Intel "), Some(Decoder::Integrated));
        assert_eq!(Decoder::parse("card"), Some(Decoder::Card));
        assert_eq!(Decoder::parse("software"), Some(Decoder::Software));
        assert_eq!(Decoder::parse("fastest"), None);
        for decoder in [Decoder::Integrated, Decoder::Card, Decoder::Software] {
            assert_eq!(Decoder::parse(decoder.name()), Some(decoder));
        }
        // Without a choice made for measuring: the processor's graphics first, the card, then software, in that order;
        // or the card, then software.
        if std::env::var_os("EXCAVATE_VIDEO_DECODER").is_none() && std::env::var_os("EXCAVATE_VIDEO_SOFTWARE").is_none() {
            assert_eq!(Decoder::order(true), [Decoder::Integrated, Decoder::Card, Decoder::Software]);
            assert_eq!(Decoder::order(false), [Decoder::Card, Decoder::Software]);
        }
    }

    /// A decoder that never answers is let go, and the frame comes in software (on a PC without a graphics card to
    /// decode on, it's software from the start). Needs a video of a few seconds, `EXCAVATE_TEST_VIDEO`: skipped
    /// without one.
    #[test]
    fn a_stalled_decoder_is_left_for_software() {
        let Some(path) = std::env::var("EXCAVATE_TEST_VIDEO").ok().filter(|path| std::path::Path::new(path).exists()) else {
            return;
        };
        let path = path.as_str();
        HANG_ON_HARDWARE.store(true, std::sync::atomic::Ordering::Relaxed);
        let started = std::time::Instant::now();
        let mut video = Video::watched(path, 160, vec![Decoder::Card, Decoder::Software], Duration::from_secs(2)).expect("opened");
        let found = video.at(2.0, 0.25).expect("a frame");
        assert!(matches!(found, At::Frame(_)));
        assert_eq!(video.decoder, Decoder::Software);
        assert!(video.stalled.is_none() || video.stalled == Some(Decoder::Card));
        assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
        // And on from there, in software.
        assert!(matches!(video.at(4.0, 0.25).expect("a frame"), At::Frame(_)));
    }

    #[test]
    fn shrunk_by_averaging_blocks() {
        // 4 x 2 BGRX: shrunk to fit 2, two pixels, each the average of a 2 x 2 block, in RGB.
        let bgrx = [10, 20, 30, 0, 30, 40, 50, 0, 0, 0, 200, 0, 0, 0, 100, 0, 10, 20, 30, 0, 30, 40, 50, 0, 0, 0, 200, 0, 0, 0, 100, 0];
        let small = shrink(&bgrx, 4, 2, 2, 1.0).expect("a picture");
        assert_eq!((small.width, small.height, small.original), (2, 1, (4, 2)));
        assert_eq!(small.pixels, [40, 30, 20, 150, 0, 0]);
        assert!(shrink(&bgrx[..8], 4, 2, 2, 1.0).is_none());
        // Wide pixels come out square, each stored column twice; narrow ones, two stored columns averaged into one.
        let wide = shrink(&bgrx, 4, 2, 8, 2.0).expect("a picture");
        assert_eq!((wide.width, wide.height, wide.original), (8, 2, (8, 2)));
        assert_eq!(wide.pixels[..3], wide.pixels[3..6]);
        let narrow = shrink(&bgrx, 4, 2, 8, 0.5).expect("a picture");
        assert_eq!((narrow.width, narrow.height, &narrow.pixels[..3]), (2, 2, &[40u8, 30, 20][..]));
    }
}
