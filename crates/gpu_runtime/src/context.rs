use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterSelection {
    Request,
    EnvOrDefault,
}

#[derive(Debug, Clone)]
pub struct GpuContextInitDescriptor {
    pub instance: wgpu::InstanceDescriptor,
    pub adapter_selection: AdapterSelection,
    pub power_preference: wgpu::PowerPreference,
    pub force_fallback_adapter: bool,
    pub required_features: wgpu::Features,
    pub required_limits: wgpu::Limits,
    pub experimental_features: wgpu::ExperimentalFeatures,
    pub memory_hints: wgpu::MemoryHints,
    pub trace: wgpu::Trace,
    pub device_label: Option<String>,
}

impl Default for GpuContextInitDescriptor {
    fn default() -> Self {
        Self {
            instance: wgpu::InstanceDescriptor::default(),
            adapter_selection: AdapterSelection::EnvOrDefault,
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::default(),
            device_label: Some(String::from("glaphica-gpu-device")),
        }
    }
}

#[derive(Debug)]
pub enum GpuContextInitError {
    AdapterRequest(wgpu::RequestAdapterError),
    UnsupportedFeatures {
        requested: wgpu::Features,
        supported: wgpu::Features,
    },
    UnsupportedLimits {
        requested: wgpu::Limits,
        supported: wgpu::Limits,
    },
    DeviceRequest(wgpu::RequestDeviceError),
}

impl Display for GpuContextInitError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AdapterRequest(err) => write!(f, "failed to request GPU adapter: {err}"),
            Self::UnsupportedFeatures {
                requested,
                supported,
            } => write!(
                f,
                "required GPU features are not supported (requested: {requested:?}, supported: {supported:?})"
            ),
            Self::UnsupportedLimits {
                requested,
                supported,
            } => write!(
                f,
                "required GPU limits are not supported (requested: {requested:?}, supported: {supported:?})"
            ),
            Self::DeviceRequest(err) => write!(f, "failed to request GPU device: {err}"),
        }
    }
}

impl Error for GpuContextInitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::AdapterRequest(err) => Some(err),
            Self::DeviceRequest(err) => Some(err),
            Self::UnsupportedFeatures { .. } | Self::UnsupportedLimits { .. } => None,
        }
    }
}

pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl GpuContext {
    pub async fn init(desc: &GpuContextInitDescriptor) -> Result<Self, GpuContextInitError> {
        Self::init_with_surface(desc, None).await
    }

    pub async fn init_with_instance_and_surface<'surface, 'window>(
        desc: &GpuContextInitDescriptor,
        instance: wgpu::Instance,
        compatible_surface: Option<&'surface wgpu::Surface<'window>>,
    ) -> Result<Self, GpuContextInitError> {
        let adapter = match desc.adapter_selection {
            AdapterSelection::Request => {
                let options = wgpu::RequestAdapterOptions {
                    power_preference: desc.power_preference,
                    force_fallback_adapter: desc.force_fallback_adapter,
                    compatible_surface,
                };
                instance
                    .request_adapter(&options)
                    .await
                    .map_err(GpuContextInitError::AdapterRequest)?
            }
            AdapterSelection::EnvOrDefault => {
                wgpu::util::initialize_adapter_from_env_or_default(&instance, compatible_surface)
                    .await
                    .map_err(GpuContextInitError::AdapterRequest)?
            }
        };

        let supported_features = adapter.features();
        let unsupported_features = desc.required_features.difference(supported_features);
        if !unsupported_features.is_empty() {
            return Err(GpuContextInitError::UnsupportedFeatures {
                requested: desc.required_features,
                supported: supported_features,
            });
        }

        let supported_limits = adapter.limits();
        if !desc.required_limits.check_limits(&supported_limits) {
            return Err(GpuContextInitError::UnsupportedLimits {
                requested: desc.required_limits.clone(),
                supported: supported_limits,
            });
        }

        let device_desc = wgpu::DeviceDescriptor {
            label: desc.device_label.as_deref(),
            required_features: desc.required_features,
            required_limits: desc.required_limits.clone(),
            experimental_features: desc.experimental_features,
            memory_hints: desc.memory_hints.clone(),
            trace: desc.trace.clone(),
        };
        let (device, queue) = adapter
            .request_device(&device_desc)
            .await
            .map_err(GpuContextInitError::DeviceRequest)?;

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    pub async fn init_with_surface<'surface, 'window>(
        desc: &GpuContextInitDescriptor,
        compatible_surface: Option<&'surface wgpu::Surface<'window>>,
    ) -> Result<Self, GpuContextInitError> {
        Self::init_with_instance_and_surface(
            desc,
            wgpu::Instance::new(&desc.instance),
            compatible_surface,
        )
        .await
    }

    #[cfg(feature = "blocking")]
    pub fn init_blocking(desc: &GpuContextInitDescriptor) -> Result<Self, GpuContextInitError> {
        pollster::block_on(Self::init(desc))
    }
}

#[cfg(test)]
mod tests {
    use super::{AdapterSelection, GpuContextInitDescriptor};

    #[test]
    fn default_init_descriptor_is_stable() {
        let desc = GpuContextInitDescriptor::default();
        assert_eq!(desc.adapter_selection, AdapterSelection::EnvOrDefault);
        assert_eq!(desc.required_features, wgpu::Features::empty());
        assert!(!desc.force_fallback_adapter);
        assert_eq!(desc.device_label.as_deref(), Some("glaphica-gpu-device"));
    }
}
