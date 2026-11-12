//! Requests through WinHTTP: Windows' own HTTP stack, so the PC's proxy settings (and a company's) apply, and there's
//! no TLS library to ship. Redirects (GitHub's release downloads use them) are followed. Besides downloads, a plain
//! request for talking to a server on this PC (llama.cpp's).

use crate::wide;
use std::io::Write;
use windows::Win32::Networking::WinHttp::{
    URL_COMPONENTS, WINHTTP_ACCESS_TYPE, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_OPEN_REQUEST_FLAGS, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_CONTENT_LENGTH, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle,
    WinHttpConnect, WinHttpCrackUrl, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts, WINHTTP_INTERNET_SCHEME_HTTPS,
};
use windows::core::PCWSTR;

struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a WinHTTP handle we opened.
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn last_error() -> String {
    windows::core::Error::from_thread().message().to_string()
}

/// A request that's been sent, and the handles it needs while its answer is read.
struct Answer {
    _session: Handle,
    _connection: Handle,
    request: Handle,
}

impl Answer {
    fn number(&self, query: u32) -> u64 {
        let mut value = 0u32;
        let mut size = 4u32;
        // SAFETY: a four-byte number, written into `value`, from our own request.
        let ok = unsafe {
            WinHttpQueryHeaders(self.request.0, query | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut value as *mut u32 as *mut _),
                                &mut size, std::ptr::null_mut()).is_ok()
        };
        if ok { u64::from(value) } else { 0 }
    }

    fn status(&self) -> u64 {
        self.number(WINHTTP_QUERY_STATUS_CODE)
    }

    /// The next piece of the body into `buffer`; 0 at its end.
    fn read(&self, buffer: &mut [u8]) -> Result<usize, String> {
        let mut available = 0u32;
        // SAFETY: our own request; the buffer outlives the call and `want` fits it.
        unsafe {
            WinHttpQueryDataAvailable(self.request.0, &mut available).map_err(|error| error.message().to_string())?;
            if available == 0 {
                return Ok(0);
            }
            let mut read = 0u32;
            let want = (available as usize).min(buffer.len());
            WinHttpReadData(self.request.0, buffer.as_mut_ptr() as *mut _, want as u32, &mut read).map_err(|error| error.message().to_string())?;
            Ok(read as usize)
        }
    }
}

fn send(url: &str, verb: &str, headers: &str, body: &[u8], access: WINHTTP_ACCESS_TYPE, receive_ms: i32) -> Result<Answer, String> {
    let url_wide = wide(url);
    let mut host = vec![0u16; 256];
    let mut path = vec![0u16; 4096];
    let mut parts = URL_COMPONENTS {
        dwStructSize: std::mem::size_of::<URL_COMPONENTS>() as u32,
        lpszHostName: windows::core::PWSTR(host.as_mut_ptr()),
        dwHostNameLength: host.len() as u32,
        lpszUrlPath: windows::core::PWSTR(path.as_mut_ptr()),
        dwUrlPathLength: path.len() as u32,
        ..Default::default()
    };
    // SAFETY: WinHTTP handles opened here and closed by `Handle`; every buffer outlives the call that uses it.
    unsafe {
        WinHttpCrackUrl(&url_wide[..url_wide.len() - 1], 0, &mut parts).map_err(|error| error.message().to_string())?;
        let agent = wide("Excavate");
        let session = Handle(WinHttpOpen(PCWSTR(agent.as_ptr()), access, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return Err(last_error());
        }
        let _ = WinHttpSetTimeouts(session.0, 0, 30_000, 30_000, receive_ms);
        let connection = Handle(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), parts.nPort, 0));
        if connection.0.is_null() {
            return Err(last_error());
        }
        let flags = if parts.nScheme == WINHTTP_INTERNET_SCHEME_HTTPS { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let verb = wide(verb);
        let request = Handle(WinHttpOpenRequest(connection.0, PCWSTR(verb.as_ptr()), PCWSTR(path.as_ptr()), PCWSTR::null(),
                                                PCWSTR::null(), std::ptr::null(), flags));
        if request.0.is_null() {
            return Err(last_error());
        }
        let headers: Vec<u16> = headers.encode_utf16().collect();
        let headers = (!headers.is_empty()).then_some(headers.as_slice());
        let body_pointer = (!body.is_empty()).then_some(body.as_ptr() as *const core::ffi::c_void);
        WinHttpSendRequest(request.0, headers, body_pointer, body.len() as u32, body.len() as u32, 0).map_err(|error| error.message().to_string())?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|error| error.message().to_string())?;
        Ok(Answer { _session: session, _connection: connection, request })
    }
}

/// Downloads `url` into `out`, calling `progress(received, total)` as it goes (total 0 when unknown). Returns the
/// bytes written. `cancelled` is checked between reads.
pub fn download(url: &str, out: &mut impl Write, progress: &mut dyn FnMut(u64, u64), cancelled: &dyn Fn() -> bool) -> Result<u64, String> {
    download_from(url, 0, out, progress, cancelled)
}

/// What `download_from` answers when the server can't continue from there: start over from 0.
pub const CANT_RESUME: &str = "the server can't resume this download";

/// Downloads `url` from byte `offset` on (what's before it is already in `out`); `progress` counts from the beginning
/// of the file.
pub fn download_from(url: &str, offset: u64, out: &mut impl Write, progress: &mut dyn FnMut(u64, u64), cancelled: &dyn Fn() -> bool) -> Result<u64, String> {
    let range = if offset > 0 { format!("Range: bytes={offset}-") } else { String::new() };
    let answer = send(url, "GET", &range, &[], WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, 60_000)?;
    let status = answer.status();
    if offset > 0 && (status == 200 || status == 416) {
        return Err(CANT_RESUME.into());
    }
    if status != 200 && status != 206 {
        // (Hugging Face answers 401 for a repository that isn't there.)
        return Err(match status {
            401 | 403 | 404 => format!("it isn't where Excavate looks for it (the server answered {status})"),
            _ => format!("the server answered {status}"),
        });
    }
    let length = answer.number(WINHTTP_QUERY_CONTENT_LENGTH);
    let total = if length > 0 { length + offset } else { 0 };
    let mut received = offset;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        if cancelled() {
            return Err("cancelled".into());
        }
        let read = answer.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        out.write_all(&buffer[..read]).map_err(|error| error.to_string())?;
        received += read as u64;
        progress(received, total);
    }
    if total != 0 && received != total {
        return Err(format!("the download stopped at {received} of {total} bytes"));
    }
    Ok(received - offset)
}

/// A request to a server on this PC (no proxy): its status and body. `body` empty makes it a GET.
pub fn local(url: &str, body: &[u8], timeout: std::time::Duration) -> Result<(u64, Vec<u8>), String> {
    local_with_key(url, body, timeout, None)
}

/// `local`, with the key the server asks for (`Authorization: Bearer <key>`, as llama.cpp's `--api-key` wants it).
pub fn local_with_key(url: &str, body: &[u8], timeout: std::time::Duration, key: Option<&str>) -> Result<(u64, Vec<u8>), String> {
    let mut headers = key.map(|key| format!("Authorization: Bearer {key}")).unwrap_or_default();
    if !body.is_empty() {
        headers += if headers.is_empty() { "Content-Type: application/json" } else { "
Content-Type: application/json" };
    }
    let verb = if body.is_empty() { "GET" } else { "POST" };
    let answer = send(url, verb, &headers, body, WINHTTP_ACCESS_TYPE_NO_PROXY, timeout.as_millis().min(i32::MAX as u128) as i32)?;
    let status = answer.status();
    let mut out = Vec::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = answer.read(&mut buffer)?;
        if read == 0 {
            return Ok((status, out));
        }
        out.extend_from_slice(&buffer[..read]);
    }
}

#[cfg(test)]
mod key_tests {
    use std::io::{BufRead, BufReader, Write};

    /// The key and the body's type both reach the server, as two headers.
    #[test]
    fn the_key_goes_with_the_request() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
                    break;
                }
                headers.push(line.trim().to_ascii_lowercase());
            }
            let mut stream = stream;
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}");
            headers
        });
        let answer = super::local_with_key(&format!("http://127.0.0.1:{port}/x"), b"{}", std::time::Duration::from_secs(5), Some("k3y"));
        let headers = server.join().unwrap();
        assert_eq!(answer.map(|(status, _)| status), Ok(200));
        assert!(headers.contains(&"authorization: bearer k3y".to_string()), "{headers:?}");
        assert!(headers.contains(&"content-type: application/json".to_string()), "{headers:?}");
    }
}
