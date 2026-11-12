//! Now, and how far local time is from UTC (for `dm:today` and the like).

/// Seconds since 1970.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The first day of a week as Windows is set to have it (Settings → Time & language → Language & region → Regional
/// format): 0 for Monday to 6 for Sunday, as `LOCALE_IFIRSTDAYOFWEEK` numbers them. Monday when it can't be read.
pub fn first_weekday() -> i64 {
    use windows_sys::Win32::Globalization::{GetLocaleInfoEx, LOCALE_IFIRSTDAYOFWEEK};
    let mut text = [0u16; 4];
    // SAFETY: the buffer is passed with its length; null is this user's own format.
    let length = unsafe { GetLocaleInfoEx(std::ptr::null(), LOCALE_IFIRSTDAYOFWEEK, text.as_mut_ptr(), text.len() as i32) };
    match (length, text[0]) {
        (2.., digit @ 0x30..=0x36) => (digit - 0x30) as i64,
        _ => 0,
    }
}

/// Seconds local time is ahead of UTC right now (daylight saving included).
pub fn utc_offset() -> i64 {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    use windows_sys::Win32::System::Time::SystemTimeToFileTime;
    unsafe {
        let mut local: SYSTEMTIME = std::mem::zeroed();
        let mut utc: SYSTEMTIME = std::mem::zeroed();
        GetLocalTime(&mut local);
        GetSystemTime(&mut utc);
        let mut a = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let mut b = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        SystemTimeToFileTime(&local, &mut a);
        SystemTimeToFileTime(&utc, &mut b);
        let f = |t: &FILETIME| ((t.dwHighDateTime as i64) << 32 | t.dwLowDateTime as i64) / 10_000_000;
        // To the nearest quarter hour: the two readings are a moment apart.
        ((f(&a) - f(&b)) as f64 / 900.0).round() as i64 * 900
    }
}
