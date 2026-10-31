use crate::{Capabilities, DeviceInfo, ProgressSink, ScanError, ScanRequest, ScanResult};

/// A family of scanner drivers (e.g. WIA on Windows, SANE on Linux, or the mock backend).
pub trait ScannerBackend: Send + Sync {
    /// Short, stable identifier, e.g. `"wia"` or `"mock"`.
    fn name(&self) -> &'static str;

    /// Enumerate the devices currently reachable through this backend.
    fn list_devices(&self) -> Result<Vec<DeviceInfo>, ScanError>;

    /// Open a device previously returned by [`ScannerBackend::list_devices`].
    fn open(&self, device: &DeviceInfo) -> Result<Box<dyn ScannerDevice>, ScanError>;
}

/// An opened scanner.
pub trait ScannerDevice: Send {
    fn info(&self) -> &DeviceInfo;

    fn capabilities(&self) -> &Capabilities;

    /// Perform a scan. Implementations must call `progress.progress` periodically and
    /// return [`ScanError::Cancelled`] promptly once `progress.is_cancelled()` is true.
    fn scan(
        &mut self,
        request: &ScanRequest,
        progress: &mut dyn ProgressSink,
    ) -> Result<ScanResult, ScanError>;
}
