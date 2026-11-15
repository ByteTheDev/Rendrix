//! wgpu device creation, windowed and headless.

pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl GpuContext {
    /// Create a context; pass a surface when it must be presentable on it. Blocks on the adapter
    /// and device requests, so it is native-only: the web target must use [`GpuContext::new_async`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(compatible_surface: Option<&wgpu::Surface<'_>>, instance: Option<wgpu::Instance>) -> Result<GpuContext, String> {
        pollster::block_on(GpuContext::new_async(compatible_surface, instance))
    }

    /// Async form of [`GpuContext::new`]; the only option in a browser, where nothing may block.
    pub async fn new_async(compatible_surface: Option<&wgpu::Surface<'_>>, instance: Option<wgpu::Instance>) -> Result<GpuContext, String> {
        let instance = if let Some(i) = instance {
            i
        } else {
            let desc = wgpu::InstanceDescriptor { backends: default_backends(), ..Default::default() };
            // The browser may expose `navigator.gpu` without being able to create an adapter
            // (e.g. headless SwiftShader); detect real support so the WebGL2 fallback is used.
            // `Instance::new` would lock onto the WebGPU-only path from the property alone.
            #[cfg(target_arch = "wasm32")]
            {
                wgpu::util::new_instance_with_webgpu_detection(&desc).await
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                wgpu::Instance::new(&desc)
            }
        };
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| format!("no suitable GPU adapter: {e}"))?;
        // WebGL2 (the fallback where WebGPU is unavailable) cannot honour the desktop defaults.
        #[cfg(target_arch = "wasm32")]
        let required_limits = wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits());
        #[cfg(not(target_arch = "wasm32"))]
        let required_limits = wgpu::Limits::default();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("rendrix device"),
                required_features: wgpu::Features::empty(),
                required_limits,
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| format!("request device: {e}"))?;
        Ok(GpuContext { instance, adapter, device, queue })
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn headless() -> Result<GpuContext, String> {
        GpuContext::new(None, None)
    }

    pub fn adapter_name(&self) -> String {
        let i = self.adapter.get_info();
        format!("{} ({:?})", i.name, i.backend)
    }
}

/// Native: the platform's primary APIs. Web: WebGPU where the browser has it, otherwise WebGL2.
pub fn default_backends() -> wgpu::Backends {
    #[cfg(target_arch = "wasm32")]
    {
        wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        wgpu::Backends::PRIMARY
    }
}
