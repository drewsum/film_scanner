use std::sync::Arc;

use crate::{DeviceInfo, ScanError, ScannerBackend, ScannerDevice};

/// Result of enumerating every registered backend. A failing backend does not hide the
/// devices found by the others.
#[derive(Debug, Default)]
pub struct DeviceListing {
    pub devices: Vec<DeviceInfo>,
    pub errors: Vec<(&'static str, ScanError)>,
}

/// Aggregates all compiled-in scanner backends.
#[derive(Clone, Default)]
pub struct DeviceRegistry {
    backends: Vec<Arc<dyn ScannerBackend>>,
}

impl DeviceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_backend(mut self, backend: impl ScannerBackend + 'static) -> Self {
        self.register(backend);
        self
    }

    pub fn register(&mut self, backend: impl ScannerBackend + 'static) {
        self.backends.push(Arc::new(backend));
    }

    pub fn backend_names(&self) -> Vec<&'static str> {
        self.backends.iter().map(|b| b.name()).collect()
    }

    pub fn list_devices(&self) -> DeviceListing {
        let mut listing = DeviceListing::default();
        for backend in &self.backends {
            match backend.list_devices() {
                Ok(devices) => listing.devices.extend(devices),
                Err(e) => {
                    log::warn!("backend {} failed to list devices: {e}", backend.name());
                    listing.errors.push((backend.name(), e));
                }
            }
        }
        listing
    }

    pub fn open(&self, device: &DeviceInfo) -> Result<Box<dyn ScannerDevice>, ScanError> {
        self.backends
            .iter()
            .find(|b| b.name() == device.backend)
            .ok_or_else(|| {
                ScanError::DeviceNotFound(format!("no backend named {}", device.backend))
            })?
            .open(device)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockBackend;

    struct Failing;

    impl ScannerBackend for Failing {
        fn name(&self) -> &'static str {
            "failing"
        }
        fn list_devices(&self) -> Result<Vec<DeviceInfo>, ScanError> {
            Err(ScanError::Backend("driver missing".into()))
        }
        fn open(&self, _: &DeviceInfo) -> Result<Box<dyn ScannerDevice>, ScanError> {
            unreachable!()
        }
    }

    #[test]
    fn aggregates_devices_and_errors() {
        let registry = DeviceRegistry::new()
            .with_backend(Failing)
            .with_backend(MockBackend);
        assert_eq!(registry.backend_names(), ["failing", "mock"]);
        let listing = registry.list_devices();
        assert_eq!(listing.devices, vec![MockBackend::device_info()]);
        assert_eq!(listing.errors.len(), 1);
        assert_eq!(listing.errors[0].0, "failing");
    }

    #[test]
    fn opens_through_matching_backend() {
        let registry = DeviceRegistry::new().with_backend(MockBackend);
        let dev = registry.open(&MockBackend::device_info()).unwrap();
        assert_eq!(dev.info(), &MockBackend::device_info());

        let mut orphan = MockBackend::device_info();
        orphan.backend = "sane";
        assert!(matches!(
            registry.open(&orphan),
            Err(ScanError::DeviceNotFound(_))
        ));
    }
}
