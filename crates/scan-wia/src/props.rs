//! Typed access to `IWiaPropertyStorage`.

use windows::Win32::Devices::ImageAcquisition::{
    IWiaPropertyStorage, WIA_PROP_LIST, WIA_PROP_RANGE,
};
use windows::Win32::System::Com::CoTaskMemAlloc;
use windows::Win32::System::Com::StructuredStorage::{
    PROPSPEC, PROPSPEC_0, PROPVARIANT, PRSPEC_PROPID, PropVariantClear, PropVariantGetElementCount,
    PropVariantGetInt32Elem, PropVariantToBSTR, PropVariantToInt32,
};
use windows::Win32::System::Variant::{VT_CLSID, VT_I4};
use windows_core::GUID;

use scan_core::ScanError;

use crate::convert::ValidValues;

pub struct Props(pub IWiaPropertyStorage);

fn spec(id: u32) -> PROPSPEC {
    PROPSPEC {
        ulKind: PRSPEC_PROPID,
        Anonymous: PROPSPEC_0 { propid: id },
    }
}

/// A PROPVARIANT read from the driver, cleared on drop.
struct Owned(PROPVARIANT);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the variant was filled by ReadMultiple/GetPropertyAttributes and is owned here.
        let _ = unsafe { PropVariantClear(&mut self.0) };
    }
}

fn err(op: &str, id: u32, e: windows_core::Error) -> ScanError {
    ScanError::Backend(format!("{op} property {id}: {e}"))
}

impl Props {
    fn read(&self, id: u32) -> Result<Owned, ScanError> {
        let mut pv = Owned(PROPVARIANT::default());
        // SAFETY: one spec, one output variant.
        unsafe { self.0.ReadMultiple(1, &spec(id), &mut pv.0) }.map_err(|e| err("read", id, e))?;
        Ok(pv)
    }

    pub fn read_i32(&self, id: u32) -> Result<i32, ScanError> {
        let pv = self.read(id)?;
        // SAFETY: pv is a valid, initialised PROPVARIANT.
        unsafe { PropVariantToInt32(&pv.0) }.map_err(|e| err("convert", id, e))
    }

    pub fn read_string(&self, id: u32) -> Result<String, ScanError> {
        let pv = self.read(id)?;
        // SAFETY: as above.
        let bstr = unsafe { PropVariantToBSTR(&pv.0) }.map_err(|e| err("convert", id, e))?;
        Ok(bstr.to_string())
    }

    pub fn valid_values(&self, id: u32) -> Result<ValidValues, ScanError> {
        let mut flags = 0u32;
        let mut pv = Owned(PROPVARIANT::default());
        // SAFETY: one spec, one flag, one output variant.
        unsafe {
            self.0
                .GetPropertyAttributes(1, &spec(id), &mut flags, &mut pv.0)
        }
        .map_err(|e| err("query attributes of", id, e))?;
        // SAFETY: pv is initialised; element access is bounds-checked by the API.
        let elems: Vec<i32> = unsafe {
            (0..PropVariantGetElementCount(&pv.0))
                .filter_map(|i| PropVariantGetInt32Elem(&pv.0, i).ok())
                .collect()
        };
        Ok(ValidValues::from_raw(
            flags & WIA_PROP_RANGE != 0,
            flags & WIA_PROP_LIST != 0,
            &elems,
        ))
    }

    fn write(&self, id: u32, pv: &PROPVARIANT) -> Result<(), ScanError> {
        log::trace!("WIA write property {id}");
        // SAFETY: one spec, one input variant that outlives the call.
        unsafe { self.0.WriteMultiple(1, &spec(id), pv, 2) }.map_err(|e| err("write", id, e))
    }

    pub fn write_i32(&self, id: u32, value: i32) -> Result<(), ScanError> {
        let mut pv = PROPVARIANT::default();
        // SAFETY: setting the discriminant and matching union member of a zeroed variant.
        unsafe {
            let inner = &mut *pv.Anonymous.Anonymous;
            inner.vt = VT_I4;
            inner.Anonymous.lVal = value;
        }
        self.write(id, &pv)
    }

    pub fn write_guid(&self, id: u32, value: &GUID) -> Result<(), ScanError> {
        // The WIA property proxy takes ownership of a VT_CLSID payload and releases it with the
        // COM allocator (a stack pointer here corrupts the heap), so allocate it with
        // CoTaskMemAlloc and do not free it ourselves.
        // SAFETY: allocation of a GUID-sized block that we fully initialise before use.
        let guid = unsafe { CoTaskMemAlloc(size_of::<GUID>()) } as *mut GUID;
        if guid.is_null() {
            return Err(ScanError::Backend("out of memory".into()));
        }
        let mut pv = PROPVARIANT::default();
        // SAFETY: `guid` is a valid, exclusively owned allocation; the variant is a zeroed one
        // whose discriminant and matching union member we set.
        unsafe {
            guid.write(*value);
            let inner = &mut *pv.Anonymous.Anonymous;
            inner.vt = VT_CLSID;
            inner.Anonymous.puuid = guid;
        }
        self.write(id, &pv)
    }
}
