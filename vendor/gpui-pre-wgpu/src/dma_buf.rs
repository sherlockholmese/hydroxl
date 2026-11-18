//! Vulkan-only bridge for copying foreign scanouts into the GPUI atlas.
use anyhow::{Context, Result, ensure};
use ash::vk;
use gpui::{AtlasTile, DmaBufFrame};
use std::os::fd::{AsRawFd, BorrowedFd};
use wgpu::hal::{api::Vulkan, vulkan};

const FOREIGN: &std::ffi::CStr = c"VK_EXT_queue_family_foreign";
const DMA_BUF: wgpu::Features = wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF;

pub(super) fn try_device(
    adapter: &wgpu::Adapter,
    mut descriptor: wgpu::DeviceDescriptor<'_>,
) -> Option<(wgpu::Device, wgpu::Queue)> {
    if std::env::var("HYDROXL_VULKAN_SCANOUT").as_deref() != Ok("1")
        || !adapter.features().contains(DMA_BUF)
    {
        return None;
    }
    descriptor.required_features |= DMA_BUF;
    // wgpu-hal negotiates the device and its queues as usual. The only
    // customization enables the ownership transfer for a non-Vulkan producer.
    let opened = unsafe {
        let native = adapter.as_hal::<Vulkan>()?;
        if !native
            .physical_device_capabilities()
            .supports_extension(FOREIGN)
        {
            return None;
        }
        native.open_with_callback(
            descriptor.required_features,
            &descriptor.required_limits,
            &descriptor.memory_hints,
            Some(Box::new(|args| {
                if !args.extensions.contains(&FOREIGN) {
                    args.extensions.push(FOREIGN);
                }
            })),
        )
    };
    let result = opened
        .map_err(|error| error.to_string())
        .and_then(|opened| {
            unsafe { adapter.create_device_from_hal(opened, &descriptor) }
                .map_err(|error| error.to_string())
        });
    match result {
        Ok(device) => Some(device),
        Err(error) => {
            log::warn!("Vulkan DMA-BUF device initialization failed: {error}");
            None
        }
    }
}

pub(super) fn supported(device: &wgpu::Device) -> bool {
    device.features().contains(DMA_BUF)
        && unsafe { device.as_hal::<Vulkan>() }
            .is_some_and(|native| native.enabled_device_extensions().contains(&FOREIGN))
}

struct Source<'a> {
    device: &'a wgpu::Device,
    image: Option<vulkan::Texture>,
}

impl Drop for Source<'_> {
    fn drop(&mut self) {
        use wgpu::hal::Device as _;
        if let (Some(native), Some(image)) =
            (unsafe { self.device.as_hal::<Vulkan>() }, self.image.take())
        {
            unsafe { native.destroy_texture(image) };
        }
    }
}

pub(super) unsafe fn copy(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    dest: &wgpu::Texture,
    tile: AtlasTile,
    frame: DmaBufFrame,
    initialize: bool,
) -> Result<()> {
    let width = u32::try_from(frame.extent.width.0)?;
    let height = u32::try_from(frame.extent.height.0)?;
    ensure!(width > 0 && height > 0 && frame.stride >= width * 4);
    ensure!(
        frame.modifier != (1 << 56) - 1 && frame.modifier != u64::MAX,
        "Unspecified DMA-BUF modifier"
    );
    let (format, vk_format) = match frame.fourcc.to_le_bytes() {
        [b'X' | b'A', b'R', b'2', b'4'] => {
            (wgpu::TextureFormat::Bgra8Unorm, vk::Format::B8G8R8A8_UNORM)
        }
        [b'X' | b'A', b'B', b'2', b'4'] => {
            (wgpu::TextureFormat::Rgba8Unorm, vk::Format::R8G8B8A8_UNORM)
        }
        _ => anyhow::bail!("Unsupported DMA-BUF pixel format"),
    };
    let desc = wgpu::hal::TextureDescriptor {
        label: Some("foreign scanout"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUses::COPY_SRC,
        memory_flags: wgpu::hal::MemoryFlags::empty(),
        view_formats: Vec::new(),
    };
    // The producer registers GPU writes on the DMA-BUF. POLLIN waits for writers
    // before the FOREIGN acquire, so a draw message alone never substitutes for a fence.
    let fd = std::os::fd::AsFd::as_fd(&frame.fd);
    wait_for_writer(fd)?;
    let blit = !frame.top_down || format != dest.format();
    let (raw, family, image) = unsafe {
        let native = device
            .as_hal::<Vulkan>()
            .context("DMA-BUF requires Vulkan")?;
        check_layout(&native, vk_format, frame.modifier, dest.format(), blit)?;
        let image = native
            .texture_from_dmabuf_fd(
                frame.fd,
                &desc,
                frame.modifier,
                frame.stride.into(),
                frame.offset.into(),
            )
            .context("Vulkan could not import the DMA-BUF")?;
        (
            native.raw_device().clone(),
            native.queue_family_index(),
            image,
        )
    };
    let src_handle = unsafe { image.raw_handle() };
    let _source = Source {
        device,
        image: Some(image),
    };
    let dst_handle = unsafe { dest.as_hal::<Vulkan>() }
        .map(|dest| unsafe { dest.raw_handle() })
        .context("The destination is not a Vulkan texture")?;
    if initialize {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: dest,
                mip_level: 0,
                aspect: wgpu::TextureAspect::All,
                origin: wgpu::Origin3d {
                    x: tile.bounds.origin.x.0 as u32,
                    y: tile.bounds.origin.y.0 as u32,
                    z: 0,
                },
            },
            &[0; 4],
            wgpu::TexelCopyBufferLayout::default(),
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
    }
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("copy foreign scanout"),
    });
    encoder.transition_resources(
        std::iter::empty(),
        std::iter::once(wgpu::TextureTransition {
            texture: dest,
            selector: None,
            state: wgpu::TextureUses::COPY_DST,
        }),
    );
    let dst_origin = [tile.bounds.origin.x.0, tile.bounds.origin.y.0];
    unsafe {
        encoder.as_hal_mut::<Vulkan, _, _>(|encoder| -> Result<()> {
            let commands = encoder
                .context("The Vulkan upload encoder is unavailable")?
                .raw_handle();
            record_copy(
                &raw,
                commands,
                src_handle,
                dst_handle,
                family,
                dst_origin,
                [frame.extent.width.0, frame.extent.height.0],
                blit,
                frame.top_down,
            );
            Ok(())
        })?;
    }
    let submission = queue.submit([encoder.finish()]);
    // The source stays alive through the GPU copy and FOREIGN release. The calling
    // transport acknowledges the frame only after this submission completes.
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })
        .context("Vulkan DMA-BUF copy did not complete")?;
    Ok(())
}

fn wait_for_writer(fd: BorrowedFd<'_>) -> Result<()> {
    let mut descriptor = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, 100) };
    ensure!(
        result == 1
            && descriptor.revents & (libc::POLLIN | libc::POLLERR | libc::POLLNVAL) == libc::POLLIN,
        "The DMA-BUF producer's fence did not signal"
    );
    Ok(())
}

fn check_layout(
    device: &vulkan::Device,
    source: vk::Format,
    modifier: u64,
    dest: wgpu::TextureFormat,
    blit: bool,
) -> Result<()> {
    let instance = device.shared_instance().raw_instance();
    let physical = device.raw_physical_device();
    let dest = match dest {
        wgpu::TextureFormat::Bgra8Unorm => vk::Format::B8G8R8A8_UNORM,
        wgpu::TextureFormat::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
        _ => anyhow::bail!("Unsupported atlas pixel format"),
    };
    let dst = unsafe { instance.get_physical_device_format_properties(physical, dest) };
    ensure!(
        !blit
            || dst
                .optimal_tiling_features
                .contains(vk::FormatFeatureFlags::BLIT_DST),
        "The atlas cannot receive blits"
    );
    let required = if blit {
        vk::FormatFeatureFlags::TRANSFER_SRC | vk::FormatFeatureFlags::BLIT_SRC
    } else {
        vk::FormatFeatureFlags::TRANSFER_SRC
    };
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
    let mut props = vk::FormatProperties2::default().push_next(&mut list);
    unsafe { instance.get_physical_device_format_properties2(physical, source, &mut props) };
    ensure!(
        list.drm_format_modifier_count <= 4096,
        "Invalid Vulkan modifier count"
    );
    let mut modifiers = vec![
        vk::DrmFormatModifierPropertiesEXT::default();
        list.drm_format_modifier_count as usize
    ];
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default()
        .drm_format_modifier_properties(&mut modifiers);
    let mut props = vk::FormatProperties2::default().push_next(&mut list);
    unsafe { instance.get_physical_device_format_properties2(physical, source, &mut props) };
    ensure!(
        modifiers.iter().any(|properties| {
            properties.drm_format_modifier == modifier
                && properties.drm_format_modifier_plane_count == 1
                && properties
                    .drm_format_modifier_tiling_features
                    .contains(required)
        }),
        "The Vulkan device cannot copy this DMA-BUF modifier"
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
unsafe fn record_copy(
    device: &ash::Device,
    commands: vk::CommandBuffer,
    src: vk::Image,
    dst: vk::Image,
    family: u32,
    [x, y]: [i32; 2],
    [w, h]: [i32; 2],
    blit: bool,
    top_down: bool,
) {
    let layers = vk::ImageSubresourceLayers::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .layer_count(1);
    let range = vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1);
    let acquire = vk::ImageMemoryBarrier::default()
        .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
        .old_layout(vk::ImageLayout::GENERAL)
        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
        .dst_queue_family_index(family)
        .image(src)
        .subresource_range(range);
    let release = vk::ImageMemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::TRANSFER_READ)
        .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
        .new_layout(vk::ImageLayout::GENERAL)
        .src_queue_family_index(family)
        .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
        .image(src)
        .subresource_range(range);
    unsafe {
        device.cmd_pipeline_barrier(
            commands,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[acquire],
        );
        if blit {
            let (top, bottom) = if top_down { (0, h) } else { (h, 0) };
            device.cmd_blit_image(
                commands,
                src,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::ImageBlit::default()
                    .src_subresource(layers)
                    .dst_subresource(layers)
                    .src_offsets([
                        vk::Offset3D { x: 0, y: top, z: 0 },
                        vk::Offset3D {
                            x: w,
                            y: bottom,
                            z: 1,
                        },
                    ])
                    .dst_offsets([
                        vk::Offset3D { x, y, z: 0 },
                        vk::Offset3D {
                            x: x + w,
                            y: y + h,
                            z: 1,
                        },
                    ])],
                vk::Filter::NEAREST,
            );
        } else {
            device.cmd_copy_image(
                commands,
                src,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::ImageCopy::default()
                    .src_subresource(layers)
                    .dst_subresource(layers)
                    .dst_offset(vk::Offset3D { x, y, z: 0 })
                    .extent(vk::Extent3D {
                        width: w as u32,
                        height: h as u32,
                        depth: 1,
                    })],
            );
        }
        device.cmd_pipeline_barrier(
            commands,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[release],
        );
    }
}
