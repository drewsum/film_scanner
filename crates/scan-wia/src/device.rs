use windows::Win32::Devices::ImageAcquisition::*;
use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, CoCreateInstance};
use windows_core::{BSTR, Interface};

use scan_core::{
    BitDepth, Capabilities, ColorMode, DeviceId, DeviceInfo, ProgressSink, ScanArea, ScanError,
    ScanPass, ScanRequest, ScanResult, ScanResultExt, ScanSource, ScannerBackend, ScannerDevice,
    SourceCapabilities,
};

use crate::com::ComGuard;
use crate::convert::{self, ValidValues};
use crate::dib;
use crate::props::Props;
use crate::transfer;

pub const WIA_BACKEND: &str = "wia";

/// STI device types (high word of `WIA_DIP_DEV_TYPE`) that are never scanners. Drivers do not
/// reliably report `StiDeviceTypeScanner`, so we exclude cameras and video devices instead
/// and let `open` decide by looking for a scannable item.
const STI_DEVICE_TYPE_CAMERA: u32 = 2;
const STI_DEVICE_TYPE_VIDEO: u32 = 3;

/// Backend for scanners exposed through Windows Image Acquisition.
///
/// Uses the WIA 1.0 interfaces (`IWiaDevMgr`), which reach both legacy WIA 1.0 drivers (such
/// as the Epson Perfection V500's) and WIA 2.0 drivers through the system compatibility layer;
/// `IWiaDevMgr2` only lists WIA 2.0 drivers.
#[derive(Default)]
pub struct WiaBackend;

fn device_manager() -> Result<IWiaDevMgr, ScanError> {
    // SAFETY: COM is initialised by the caller's ComGuard.
    unsafe { CoCreateInstance(&WiaDevMgr, None, CLSCTX_LOCAL_SERVER) }
        .backend_context("creating WIA device manager (is the WIA service running?)")
}

fn open_root(device: &DeviceInfo) -> Result<IWiaItem, ScanError> {
    let mgr = device_manager()?;
    // SAFETY: plain COM call.
    unsafe { mgr.CreateDevice(&BSTR::from(device.id.0.as_str())) }
        .map_err(|e| ScanError::Backend(format!("opening {}: {e}", device.model)))
}

fn props_of(item: &impl Interface) -> Result<Props, ScanError> {
    Ok(Props(
        item.cast::<IWiaPropertyStorage>()
            .backend_context("property storage")?,
    ))
}

/// The first child item of the root, which WIA scanner drivers use for acquisition.
fn scan_item(root: &IWiaItem) -> Result<IWiaItem, ScanError> {
    // SAFETY: COM enumeration; the fetched item is owned by the Option.
    let children = unsafe { root.EnumChildItems() }.backend_context("enumerating items")?;
    let mut item = None;
    let mut fetched = 0u32;
    unsafe { children.Next(1, &mut item, &mut fetched) }.backend_context("next item")?;
    item.filter(|_| fetched == 1)
        .ok_or_else(|| ScanError::Unsupported("device has no scan item".into()))
}

/// Sources offered by the root's `WIA_DPS_DOCUMENT_HANDLING_CAPABILITIES`. Drivers that do
/// not report the property are assumed to be flatbed-only.
fn available_sources(root_props: &Props) -> Vec<ScanSource> {
    let caps = root_props
        .read_i32(WIA_DPS_DOCUMENT_HANDLING_CAPABILITIES)
        .map(|v| v as u32)
        .unwrap_or(FLATBED);
    let mut out = Vec::new();
    if caps & FLATBED != 0 {
        out.push(ScanSource::Flatbed);
    }
    if caps & FILM_TPA != 0 {
        out.push(ScanSource::Transparency);
    }
    out
}

fn select_source(root_props: &Props, source: ScanSource) -> Result<(), ScanError> {
    let value = match source {
        ScanSource::Flatbed => FLATBED,
        ScanSource::Transparency => FILM_TPA,
    };
    match root_props.write_i32(WIA_DPS_DOCUMENT_HANDLING_SELECT, value as i32) {
        // Flatbed-only drivers often lack the property entirely.
        Err(_) if source == ScanSource::Flatbed => Ok(()),
        other => other,
    }
}

fn source_capabilities(source: ScanSource, props: &Props) -> Result<SourceCapabilities, ScanError> {
    let mut color_modes = Vec::new();
    match props.valid_values(WIA_IPA_DATATYPE)? {
        ValidValues::Other => color_modes.push(ColorMode::Color),
        valid => {
            if valid.contains(WIA_DATA_COLOR as i32) {
                color_modes.push(ColorMode::Color);
            }
            if valid.contains(WIA_DATA_GRAYSCALE as i32) {
                color_modes.push(ColorMode::Gray);
            }
        }
    }

    // Depth constraints depend on the data type, so query them in colour mode. Drivers with a
    // fixed depth report no constraint; fall back to the current value then.
    if color_modes.contains(&ColorMode::Color) {
        let _ = props.write_i32(WIA_IPA_DATATYPE, WIA_DATA_COLOR as i32);
    }
    let mut bit_depths = convert::color_bit_depths(&props.valid_values(WIA_IPA_DEPTH)?);
    if bit_depths.is_empty() {
        bit_depths.push(match props.read_i32(WIA_IPA_DEPTH) {
            Ok(48) => BitDepth::Sixteen,
            _ => BitDepth::Eight,
        });
    }

    let resolutions = convert::resolutions(&props.valid_values(WIA_IPS_XRES)?);
    if resolutions.is_empty() {
        return Err(ScanError::Backend(format!(
            "{source:?}: driver reported no resolutions"
        )));
    }

    // Extents are in pixels at the current resolution; their maximum gives the bed size for
    // the selected source.
    let max_extent = |id| match props.valid_values(id) {
        Ok(ValidValues::Range { max, .. }) => Ok(max),
        _ => props.read_i32(id),
    };
    let xres = props.read_i32(WIA_IPS_XRES)?.max(1) as f32;
    let yres = props.read_i32(WIA_IPS_YRES)?.max(1) as f32;
    let width_mm = max_extent(WIA_IPS_XEXTENT)? as f32 / xres * scan_core::MM_PER_INCH;
    let height_mm = max_extent(WIA_IPS_YEXTENT)? as f32 / yres * scan_core::MM_PER_INCH;

    Ok(SourceCapabilities {
        source,
        resolutions,
        bit_depths,
        color_modes,
        infrared: false,
        max_exposure_passes: 1,
        max_area: ScanArea::full(width_mm, height_mm),
    })
}

fn read_device_info(props: &Props) -> Result<Option<DeviceInfo>, ScanError> {
    let device_type = (props.read_i32(WIA_DIP_DEV_TYPE).unwrap_or(0) as u32) >> 16;
    if device_type == STI_DEVICE_TYPE_CAMERA || device_type == STI_DEVICE_TYPE_VIDEO {
        return Ok(None);
    }
    let id = props.read_string(WIA_DIP_DEV_ID)?;
    let model = props
        .read_string(WIA_DIP_DEV_DESC)
        .or_else(|_| props.read_string(WIA_DIP_DEV_NAME))
        .unwrap_or_else(|_| id.clone());
    let manufacturer = props.read_string(WIA_DIP_VEND_DESC).unwrap_or_default();
    Ok(Some(DeviceInfo {
        id: DeviceId(id),
        manufacturer,
        model,
        backend: WIA_BACKEND,
    }))
}

impl ScannerBackend for WiaBackend {
    fn name(&self) -> &'static str {
        WIA_BACKEND
    }

    fn list_devices(&self) -> Result<Vec<DeviceInfo>, ScanError> {
        let _com = ComGuard::new()?;
        let mgr = device_manager()?;
        // SAFETY: COM enumeration of device property sets.
        let devices = unsafe { mgr.EnumDeviceInfo(WIA_DEVINFO_ENUM_LOCAL as i32) }
            .backend_context("enumerating WIA devices")?;
        let mut out = Vec::new();
        loop {
            let mut storage = None;
            let mut fetched = 0u32;
            unsafe { devices.Next(1, &mut storage, &mut fetched) }
                .backend_context("next device")?;
            let Some(storage) = storage.filter(|_| fetched == 1) else {
                break;
            };
            match read_device_info(&Props(storage)) {
                Ok(Some(info)) => out.push(info),
                Ok(None) => {}
                Err(e) => log::warn!("skipping WIA device: {e}"),
            }
        }
        Ok(out)
    }

    fn open(&self, device: &DeviceInfo) -> Result<Box<dyn ScannerDevice>, ScanError> {
        let _com = ComGuard::new()?;
        let root = open_root(device)?;
        let root_props = props_of(&root)?;
        let item_props = props_of(&scan_item(&root)?)?;
        let mut caps = Capabilities::default();
        for source in available_sources(&root_props) {
            let result = select_source(&root_props, source)
                .and_then(|_| source_capabilities(source, &item_props));
            match result {
                Ok(c) => caps.sources.push(c),
                Err(e) => log::warn!("{}: ignoring {source:?}: {e}", device.model),
            }
        }
        if caps.sources.is_empty() {
            return Err(ScanError::Unsupported(format!(
                "{} exposes no usable scan source",
                device.model
            )));
        }
        Ok(Box::new(WiaDevice {
            info: device.clone(),
            caps,
        }))
    }
}

/// An opened WIA scanner. Holds no COM objects, so it can move between threads; each scan
/// reconnects to the device on the calling thread.
pub struct WiaDevice {
    info: DeviceInfo,
    caps: Capabilities,
}

fn configure(
    props: &Props,
    request: &ScanRequest,
    area: ScanArea,
    max: ScanArea,
) -> Result<(), ScanError> {
    let color = request.color_mode == ColorMode::Color;
    let data_type = if color {
        WIA_DATA_COLOR
    } else {
        WIA_DATA_GRAYSCALE
    };
    props.write_i32(WIA_IPA_DATATYPE, data_type as i32)?;
    let depth = convert::wia_depth(request.bit_depth, color);
    if props.read_i32(WIA_IPA_DEPTH).ok() != Some(depth) {
        props.write_i32(WIA_IPA_DEPTH, depth)?;
    }
    props.write_i32(WIA_IPS_XRES, request.dpi as i32)?;
    props.write_i32(WIA_IPS_YRES, request.dpi as i32)?;
    // Position and extent constrain each other: reset the origin, then take the bed size in
    // pixels from the extent range the driver reports for the new resolution (falling back
    // to our own computation), size the window, move it into place, and re-apply the size
    // in case the driver adjusted it.
    props.write_i32(WIA_IPS_XPOS, 0)?;
    props.write_i32(WIA_IPS_YPOS, 0)?;
    let (bed_w, bed_h) = convert::bed_pixels(max, request.dpi);
    let limit = |id, computed: i32| match props.valid_values(id) {
        Ok(ValidValues::Range { max, .. }) if max > 0 => max,
        _ => computed,
    };
    let bed = (limit(WIA_IPS_XEXTENT, bed_w), limit(WIA_IPS_YEXTENT, bed_h));
    let (x, y, w, h) = convert::area_to_pixels(area, bed, request.dpi);
    log::debug!("WIA window {x},{y} {w}x{h} px (bed {bed:?})");
    props.write_i32(WIA_IPS_XEXTENT, w)?;
    props.write_i32(WIA_IPS_YEXTENT, h)?;
    props.write_i32(WIA_IPS_XPOS, x)?;
    props.write_i32(WIA_IPS_YPOS, y)?;
    props.write_i32(WIA_IPS_XEXTENT, w)?;
    props.write_i32(WIA_IPS_YEXTENT, h)?;
    props.write_i32(WIA_IPA_TYMED, TYMED_CALLBACK as i32)?;
    props.write_guid(WIA_IPA_FORMAT, &WiaImgFmt_MEMORYBMP)
}

impl ScannerDevice for WiaDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn scan(
        &mut self,
        request: &ScanRequest,
        progress: &mut dyn ProgressSink,
    ) -> Result<ScanResult, ScanError> {
        let (src, area) = request.validate(&self.caps)?;
        let _com = ComGuard::new()?;
        log::debug!("scan: opening root");
        let root = open_root(&self.info)?;
        select_source(&props_of(&root)?, request.source)?;
        let item = scan_item(&root)?;
        let props = props_of(&item)?;
        configure(&props, request, area, src.max_area)?;
        log::debug!("WIA configured for {request:?}");
        let buffer_size = props.read_i32(WIA_IPA_BUFFER_SIZE).unwrap_or(0).max(0) as u32;

        progress.progress(0.0);
        let transfer: IWiaDataTransfer = item.cast().backend_context("IWiaDataTransfer")?;
        log::debug!("starting banded transfer, buffer {buffer_size}");
        let bytes = transfer::download(&transfer, buffer_size, progress)?;
        log::debug!("transfer complete: {} bytes", bytes.len());
        let image = dib::decode_memory_bmp(&bytes)?;
        progress.progress(1.0);

        Ok(ScanResult {
            source: request.source,
            dpi: request.dpi,
            area,
            passes: vec![ScanPass {
                exposure: Default::default(),
                image,
            }],
            infrared: None,
            // WIA drivers deliver display-ready (roughly sRGB) data.
            gamma: 2.2,
        })
    }
}
