//! Byte transports to a scanner's bulk endpoints.

use std::collections::VecDeque;

use scan_core::ScanError;

/// Bulk-pipe access to a scanner: writes go to the bulk OUT endpoint, reads come from bulk IN.
pub trait Transport: Send {
    fn write(&mut self, data: &[u8]) -> Result<(), ScanError>;

    /// Read one transfer of at most `buf.len()` bytes; returns how many arrived.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, ScanError>;

    /// Read exactly `buf.len()` bytes, across as many transfers as needed.
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ScanError> {
        let mut filled = 0;
        while filled < buf.len() {
            let n = self.read(&mut buf[filled..])?;
            if n == 0 {
                return Err(ScanError::Backend(format!(
                    "scanner sent {filled} of {} expected bytes",
                    buf.len()
                )));
            }
            filled += n;
        }
        Ok(())
    }
}

/// One expected write and the replies the device gives to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exchange {
    pub write: Vec<u8>,
    pub replies: Vec<Vec<u8>>,
}

impl Exchange {
    pub fn new(write: &[u8], replies: &[&[u8]]) -> Self {
        Self {
            write: write.to_vec(),
            replies: replies.iter().map(|r| r.to_vec()).collect(),
        }
    }
}

/// A transport that plays back a script, failing on any write it does not expect.
#[derive(Debug, Default)]
pub struct ScriptedTransport {
    script: VecDeque<Exchange>,
    pending: VecDeque<Vec<u8>>,
}

impl ScriptedTransport {
    pub fn new(script: impl IntoIterator<Item = Exchange>) -> Self {
        Self {
            script: script.into_iter().collect(),
            pending: VecDeque::new(),
        }
    }

    /// True once every scripted exchange and reply has been consumed.
    pub fn finished(&self) -> bool {
        self.script.is_empty() && self.pending.is_empty()
    }
}

impl Transport for ScriptedTransport {
    fn write(&mut self, data: &[u8]) -> Result<(), ScanError> {
        if !self.pending.is_empty() {
            return Err(ScanError::Backend(format!(
                "write {data:02x?} while {} scripted replies are unread",
                self.pending.len()
            )));
        }
        let Some(next) = self.script.pop_front() else {
            return Err(ScanError::Backend(format!("unscripted write {data:02x?}")));
        };
        if next.write != data {
            return Err(ScanError::Backend(format!(
                "expected write {:02x?}, got {data:02x?}",
                next.write
            )));
        }
        self.pending = next.replies.into();
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, ScanError> {
        let Some(mut reply) = self.pending.pop_front() else {
            return Err(ScanError::Backend("read with no scripted reply".into()));
        };
        let n = reply.len().min(buf.len());
        buf[..n].copy_from_slice(&reply[..n]);
        if n < reply.len() {
            // A short read buffer leaves the rest for the next read, as a pipe would.
            self.pending.push_front(reply.split_off(n));
        }
        Ok(n)
    }
}

#[cfg(windows)]
pub use usbscan::{UsbScan, UsbScanDevice, find_usbscan_devices};

#[cfg(windows)]
mod usbscan {
    //! Windows `usbscan.sys`: the kernel driver Windows binds to still-image USB devices.
    //! `ReadFile`/`WriteFile` on its device file move data over the bulk pipes, so we can
    //! talk to a scanner without replacing the vendor's driver.

    use windows::Win32::Foundation::{
        CloseHandle, ERROR_SUCCESS, GENERIC_READ, GENERIC_WRITE, HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        ReadFile, WriteFile,
    };
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW,
        RegGetValueW, RegOpenKeyExW,
    };
    use windows::core::{HSTRING, PCWSTR, PWSTR};

    use super::Transport;
    use scan_core::ScanError;

    /// Still-image device class; each device's key names its usbscan file in `CreateFileName`.
    const IMAGE_CLASS_KEY: &str =
        r"SYSTEM\CurrentControlSet\Control\Class\{6BDD1FC6-810F-11D0-BEC7-08002BE2092F}";
    /// `CTL_CODE(FILE_DEVICE_USB_SCAN, IOCTL_INDEX + 11, METHOD_BUFFERED, FILE_ANY_ACCESS)`.
    const IOCTL_SET_TIMEOUT: u32 = 0x8000_202c;

    /// A usbscan device found in the registry.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct UsbScanDevice {
        /// Device file, e.g. `\\.\Usbscan0`.
        pub path: String,
        /// Lower-case hardware id, e.g. `usb\vid_04b8&pid_0130`.
        pub hardware_id: String,
        pub description: String,
    }

    impl UsbScanDevice {
        /// Vendor and product id parsed from the hardware id.
        pub fn usb_id(&self) -> Option<(u16, u16)> {
            let hex = |key: &str| {
                let at = self.hardware_id.find(key)? + key.len();
                u16::from_str_radix(self.hardware_id.get(at..at + 4)?, 16).ok()
            };
            Some((hex("vid_")?, hex("pid_")?))
        }
    }

    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the key was opened by `open_key` and is closed once.
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    fn open_key(parent: HKEY, path: &str) -> Option<Key> {
        let mut key = HKEY::default();
        // SAFETY: valid parent key and NUL-terminated path; `key` receives the handle.
        let err = unsafe { RegOpenKeyExW(parent, &HSTRING::from(path), None, KEY_READ, &mut key) };
        (err == ERROR_SUCCESS).then_some(Key(key))
    }

    fn string_value(key: &Key, name: &str) -> Option<String> {
        let name = HSTRING::from(name);
        let mut buf = [0u16; 512];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: `buf` holds `len` bytes; RRF_RT_REG_SZ guarantees NUL termination.
        let err = unsafe {
            RegGetValueW(
                key.0,
                PCWSTR::null(),
                &name,
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut len),
            )
        };
        if err != ERROR_SUCCESS {
            return None;
        }
        let chars = (len as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..chars]))
    }

    /// All still-image devices with a usbscan device file.
    pub fn find_usbscan_devices() -> Vec<UsbScanDevice> {
        let Some(class) = open_key(HKEY_LOCAL_MACHINE, IMAGE_CLASS_KEY) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for index in 0.. {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            // SAFETY: `name` holds `len` UTF-16 units.
            let err = unsafe {
                RegEnumKeyExW(
                    class.0,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if err != ERROR_SUCCESS {
                break;
            }
            let sub = String::from_utf16_lossy(&name[..len as usize]);
            let Some(key) = open_key(class.0, &sub) else {
                continue;
            };
            let (Some(path), Some(id)) = (
                string_value(&key, "CreateFileName"),
                string_value(&key, "MatchingDeviceId"),
            ) else {
                continue;
            };
            if !path.to_ascii_lowercase().contains("usbscan") {
                continue;
            }
            out.push(UsbScanDevice {
                path,
                hardware_id: id.to_ascii_lowercase(),
                description: string_value(&key, "DriverDesc").unwrap_or_default(),
            });
        }
        out
    }

    /// An open usbscan device file.
    pub struct UsbScan {
        handle: HANDLE,
    }

    // SAFETY: a file handle may be used from any thread; we never share it concurrently.
    unsafe impl Send for UsbScan {}

    impl UsbScan {
        pub fn open(path: &str) -> Result<Self, ScanError> {
            // SAFETY: plain CreateFileW on a device path; the handle is owned by `UsbScan`.
            let handle = unsafe {
                CreateFileW(
                    &HSTRING::from(path),
                    (GENERIC_READ | GENERIC_WRITE).0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                )
            }
            .map_err(|e| ScanError::Backend(format!("open {path}: {e}")))?;
            Ok(Self { handle })
        }

        /// Read and write timeouts, in seconds. usbscan's default is two minutes.
        pub fn set_timeouts(&mut self, read_s: u32, write_s: u32) -> Result<(), ScanError> {
            let timeouts = [read_s, write_s, read_s];
            let mut returned = 0u32;
            // SAFETY: input is a USBSCAN_TIMEOUT (three ULONGs) living for the call.
            unsafe {
                DeviceIoControl(
                    self.handle,
                    IOCTL_SET_TIMEOUT,
                    Some(timeouts.as_ptr().cast()),
                    size_of_val(&timeouts) as u32,
                    None,
                    0,
                    Some(&mut returned),
                    None,
                )
            }
            .map_err(|e| ScanError::Backend(format!("set usbscan timeout: {e}")))
        }
    }

    impl Drop for UsbScan {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateFileW and is closed once.
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }

    impl Transport for UsbScan {
        fn write(&mut self, data: &[u8]) -> Result<(), ScanError> {
            let mut written = 0u32;
            // SAFETY: synchronous write from a live slice.
            unsafe { WriteFile(self.handle, Some(data), Some(&mut written), None) }
                .map_err(|e| ScanError::Backend(format!("USB write: {e}")))?;
            if written as usize != data.len() {
                return Err(ScanError::Backend(format!(
                    "USB write sent {written} of {} bytes",
                    data.len()
                )));
            }
            log::trace!("OUT {data:02x?}");
            Ok(())
        }

        fn read(&mut self, buf: &mut [u8]) -> Result<usize, ScanError> {
            let mut read = 0u32;
            // SAFETY: synchronous read into a live, writable slice.
            unsafe { ReadFile(self.handle, Some(buf), Some(&mut read), None) }
                .map_err(|e| ScanError::Backend(format!("USB read: {e}")))?;
            let n = read as usize;
            log::trace!(" IN {:02x?}", &buf[..n.min(64)]);
            Ok(n)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_transport_plays_back_and_checks_writes() {
        let mut t = ScriptedTransport::new([
            Exchange::new(&[0x1b, 0x40], &[&[0x06]]),
            Exchange::new(&[0x1b, 0x03], &[&[0x81, 0x00, 0x11, 0x22]]),
        ]);
        let mut b = [0u8; 8];
        t.write(&[0x1b, 0x40]).unwrap();
        assert_eq!(t.read(&mut b).unwrap(), 1);
        assert_eq!(b[0], 0x06);
        t.write(&[0x1b, 0x03]).unwrap();
        let mut two = [0u8; 2];
        t.read_exact(&mut two).unwrap();
        assert_eq!(two, [0x81, 0x00]);
        t.read_exact(&mut two).unwrap();
        assert_eq!(two, [0x11, 0x22]);
        assert!(t.finished());
        assert!(t.write(&[0]).is_err());
    }

    #[test]
    fn scripted_transport_rejects_unexpected_write() {
        let mut t = ScriptedTransport::new([Exchange::new(&[1], &[])]);
        assert!(t.write(&[2]).is_err());
        let mut b = [0u8; 1];
        assert!(t.read(&mut b).is_err());
    }
}
