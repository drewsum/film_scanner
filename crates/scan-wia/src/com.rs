use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

use scan_core::ScanError;

/// Initialises COM on the current thread for the guard's lifetime.
///
/// WIA objects are apartment-bound, so every public entry point creates its own guard and
/// its own WIA objects instead of sharing them across threads.
pub struct ComGuard {
    owns_init: bool,
}

impl ComGuard {
    pub fn new() -> Result<Self, ScanError> {
        // SAFETY: plain COM initialisation; balanced by CoUninitialize in Drop when it succeeds.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            // The thread already joined a multithreaded apartment; WIA works from there too.
            return Ok(Self { owns_init: false });
        }
        hr.ok()
            .map_err(|e| ScanError::Backend(format!("CoInitializeEx: {e}")))?;
        Ok(Self { owns_init: true })
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.owns_init {
            // SAFETY: matches the successful CoInitializeEx in `new` on this same thread.
            unsafe { CoUninitialize() };
        }
    }
}
