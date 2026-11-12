//! Windows Hello (face, fingerprint or PIN) before showing what Excavate hid: the system's own verification dialog,
//! over the popup.

use windows::Security::Credentials::UI::{UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::IUserConsentVerifierInterop;
use windows::core::HSTRING;
use windows_future::IAsyncOperation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verified {
    Yes,
    No,
    /// No Windows Hello on this PC (or it isn't set up): nothing to ask with.
    Unavailable,
}

/// Asks the person to verify, over `owner`. Blocks until they answer: call it off the UI thread.
pub fn verify(owner: isize, message: &str) -> Result<Verified, String> {
    let error = |error: windows::core::Error| error.message().to_string();
    let availability = UserConsentVerifier::CheckAvailabilityAsync().map_err(error)?.join().map_err(error)?;
    if availability != UserConsentVerifierAvailability::Available {
        return Ok(Verified::Unavailable);
    }
    let interop = windows::core::factory::<UserConsentVerifier, IUserConsentVerifierInterop>().map_err(error)?;
    // SAFETY: the window handle is ours; the operation is awaited before returning.
    let operation: IAsyncOperation<UserConsentVerificationResult> =
        unsafe { interop.RequestVerificationForWindowAsync(HWND(owner as *mut _), &HSTRING::from(message)) }.map_err(error)?;
    let result = operation.join().map_err(error)?;
    Ok(if result == UserConsentVerificationResult::Verified { Verified::Yes } else { Verified::No })
}
