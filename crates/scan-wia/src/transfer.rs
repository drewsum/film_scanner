//! Banded in-memory transfer through `IWiaDataTransfer` and an `IWiaDataCallback`.

use std::cell::{Cell, RefCell};

use windows::Win32::Devices::ImageAcquisition::{
    IT_MSG_DATA, IT_MSG_STATUS, IWiaDataCallback, IWiaDataCallback_Impl, IWiaDataTransfer,
    WIA_DATA_TRANSFER_INFO,
};
use windows::Win32::Foundation::S_FALSE;
use windows_core::{ComObject, implement};

use scan_core::{ProgressSink, ScanError};

#[implement(IWiaDataCallback)]
struct Callback {
    data: RefCell<Vec<u8>>,
    /// The caller's progress sink, valid only for the duration of `download`.
    sink: Cell<Option<*mut (dyn ProgressSink + 'static)>>,
    cancelled: Cell<bool>,
}

impl IWiaDataCallback_Impl for Callback_Impl {
    fn BandedDataCallback(
        &self,
        message: i32,
        _status: i32,
        percent: i32,
        offset: i32,
        length: i32,
        _reserved: i32,
        _res_length: i32,
        buffer: *mut u8,
    ) -> windows_core::Result<()> {
        log::trace!("WIA callback msg={message} pct={percent} off={offset} len={length}");
        if message == IT_MSG_DATA as i32 && !buffer.is_null() && length > 0 && offset >= 0 {
            // SAFETY: WIA guarantees `buffer` holds `length` valid bytes during the callback.
            let band = unsafe { std::slice::from_raw_parts(buffer, length as usize) };
            let mut data = self.data.borrow_mut();
            let (start, end) = (offset as usize, offset as usize + length as usize);
            if data.len() < end {
                data.resize(end, 0);
            }
            data[start..end].copy_from_slice(band);
        }
        if let Some(sink) = self.sink.get() {
            // SAFETY: `download` sets the pointer from a live `&mut dyn ProgressSink` and clears
            // it before returning. WIA invokes this callback on the calling (STA) thread while
            // the transfer is running, so the sink is alive and not aliased.
            let sink = unsafe { &mut *sink };
            if message == IT_MSG_STATUS as i32 || message == IT_MSG_DATA as i32 {
                sink.progress(percent.clamp(0, 100) as f32 / 100.0);
            }
            if sink.is_cancelled() {
                self.cancelled.set(true);
                // Returning S_FALSE asks WIA to cancel the transfer.
                return Err(windows_core::Error::from_hresult(S_FALSE));
            }
        }
        Ok(())
    }
}

/// Run a banded transfer and return the raw bytes (a memory bitmap for `WiaImgFmt_MEMORYBMP`).
pub fn download(
    transfer: &IWiaDataTransfer,
    buffer_size: u32,
    progress: &mut dyn ProgressSink,
) -> Result<Vec<u8>, ScanError> {
    let state = ComObject::new(Callback {
        data: RefCell::new(Vec::new()),
        sink: Cell::new(None),
        cancelled: Cell::new(false),
    });
    let callback: IWiaDataCallback = state.to_interface();

    let sink: *mut (dyn ProgressSink + '_) = progress;
    // SAFETY: erases the borrow's lifetime; the pointer is cleared below before `progress`
    // can go out of scope, and is only dereferenced inside the transfer call.
    let sink: *mut (dyn ProgressSink + 'static) = unsafe { std::mem::transmute(sink) };
    state.sink.set(Some(sink));
    let mut info = WIA_DATA_TRANSFER_INFO {
        ulSize: size_of::<WIA_DATA_TRANSFER_INFO>() as u32,
        ulBufferSize: buffer_size.max(64 * 1024),
        bDoubleBuffer: true.into(),
        ..Default::default()
    };
    // SAFETY: plain COM call with our callback object and a properly sized info struct.
    let result = unsafe { transfer.idtGetBandedData(&mut info, &callback) };
    state.sink.set(None);

    if state.cancelled.get() {
        return Err(ScanError::Cancelled);
    }
    result.map_err(|e| ScanError::Backend(format!("WIA transfer failed: {e}")))?;
    let data = state.data.take();
    if data.is_empty() {
        return Err(ScanError::Backend("driver returned no image data".into()));
    }
    Ok(data)
}
