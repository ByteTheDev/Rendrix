//! Renderer tests for the paint-gap display items: image-texel filtering, the ellipse
//! clip kind, and the batch-preserving fast path. GPU tests skip when no adapter exists.

use rendrix_css::{Color, FilterOp};
use rendrix_gfx::{DisplayList, Fill, GpuContext, ImageItem, Item, RectItem, Renderer, TextSystem};

fn render(list: &DisplayList, w: u32, h: u32) -> Option<(Vec<u8>, RendererStats)> {
    let ctx = GpuContext::headless().ok()?;
    let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
    let mut t = TextSystem::new();
    let img = r.render_to_rgba(list, &mut t, (w, h), 1.0);
    Some((img, RendererStats { instances: r.last_instances, batches: r.last_batches }))
}

struct RendererStats {
    instances: usize,
    batches: usize,
}

fn px(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2], img[i + 3]]
}

fn solid(x: f32, y: f32, w: f32, h: f32, c: Color) -> Item {
    Item::Rect(RectItem { rect: [x, y, w, h], radii: [0.0; 4], fill: Fill::Solid(c), border: None })
}

#[test]
fn image_texel_filter_grayscale_and_brightness() {
    let Ok(ctx) = GpuContext::headless() else { eprintln!("no GPU adapter, skipping"); return };
    let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
    // Opaque red 8x8 upload.
    let mut red = Vec::with_capacity(8 * 8 * 4);
    for _ in 0..64 {
        red.extend_from_slice(&[255, 0, 0, 255]);
    }
    let id = r.upload_image(&red, 8, 8);
    let mut l = DisplayList::new(32.0, 32.0, Color::WHITE);
    l.push(Item::PushFilter(vec![FilterOp::Grayscale(1.0), FilterOp::Brightness(2.0)]));
    l.push(Item::Image(ImageItem { image: id, rect: [0.0, 0.0, 32.0, 32.0], radii: [0.0; 4], uv: [0.0, 0.0, 1.0, 1.0] }));
    l.push(Item::PopFilter);
    let mut t = TextSystem::new();
    let img = r.render_to_rgba(&l, &mut t, (32, 32), 1.0);
    let c = px(&img, 32, 16, 16);
    // luma(1,0,0) ≈ 0.2126, × brightness 2 → ~108 in every channel.
    assert!((c[0] as i32 - 108).abs() < 8 && (c[1] as i32 - c[0] as i32).abs() < 4 && (c[2] as i32 - c[0] as i32).abs() < 4, "filtered texel: {c:?}");
}

#[test]
fn ellipse_clip_masks_corners() {
    let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
    l.push(solid(4.0, 4.0, 56.0, 56.0, Color::from_u8(255, 0, 0, 1.0)));
    l.push(Item::PushEllipseClip { rect: [12.0, 12.0, 40.0, 40.0] });
    l.push(solid(4.0, 4.0, 56.0, 56.0, Color::from_u8(0, 0, 255, 1.0)));
    l.push(Item::PopClip);
    let Some((img, _)) = render(&l, 64, 64) else { eprintln!("no GPU adapter, skipping"); return };
    assert_eq!(px(&img, 64, 32, 32), [0, 0, 255, 255], "ellipse centre paints");
    assert_eq!(px(&img, 64, 6, 6), [255, 0, 0, 255], "ellipse corner keeps the red below");
    assert_eq!(px(&img, 64, 2, 2), [255, 255, 255, 255], "outside both rects stays background");
}

#[test]
fn filter_markers_preserve_batching() {
    // Grayscale/brightness add ALU but no instances or batches (same texture, same draw).
    let mut l = DisplayList::new(200.0, 200.0, Color::WHITE);
    l.push(Item::PushFilter(vec![FilterOp::Grayscale(0.5)]));
    for i in 0..10 {
        l.push(solid(i as f32 * 10.0, 10.0, 8.0, 8.0, Color::BLACK));
    }
    l.push(Item::PopFilter);
    let Some((_, st)) = render(&l, 200, 200) else { eprintln!("no GPU adapter, skipping"); return };
    assert_eq!(st.instances, 10, "pointwise filters add no instances");
    assert_eq!(st.batches, 1, "one batch for one texture");
    // Blur expands per instance but still merges into the one batch.
    let mut l = DisplayList::new(200.0, 200.0, Color::WHITE);
    l.push(Item::PushFilter(vec![FilterOp::Blur(4.0)]));
    for i in 0..10 {
        l.push(solid(i as f32 * 10.0, 10.0, 8.0, 8.0, Color::BLACK));
    }
    l.push(Item::PopFilter);
    let Some((_, st)) = render(&l, 200, 200) else { eprintln!("no GPU adapter, skipping"); return };
    assert_eq!(st.instances, 50, "blur expands 10 rects to centre + 4 offsets each");
    assert_eq!(st.batches, 1, "copies share the texture: still one draw call");
}
