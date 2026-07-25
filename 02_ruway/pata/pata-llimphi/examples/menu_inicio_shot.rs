//! Volcado headless del **menú de inicio** de pata a PNG, en sus tres estilos
//! (Classic / XP / GNOME), usando las apps reales descubiertas por app-bus.
//! Sirve para ver el estado actual del menú sin bootear el DM.
//!
//! `cargo run -p pata-llimphi --example menu_inicio_shot -- [prefijo]`
//! produce  <prefijo>-classic.png  <prefijo>-xp.png  <prefijo>-gnome.png

use std::fs::File;
use std::io::BufWriter;

use llimphi_ui::llimphi_compositor::{measure_text_node, mount, paint};
use llimphi_ui::llimphi_hal::{wgpu, Hal};
use llimphi_ui::llimphi_layout::taffy;
use llimphi_ui::llimphi_layout::taffy::prelude::{percent, FlexDirection, Size, Style};
use llimphi_ui::llimphi_layout::LayoutTree;
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::llimphi_raster::{vello, Renderer};
use llimphi_ui::llimphi_text::Typesetter;
use llimphi_ui::View;

use pata_llimphi::{render, Msg, MenuStyle};

const W: u32 = 1280;
const H: u32 = 900;
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn main() {
    let prefijo = std::env::args().nth(1).unwrap_or_else(|| "menu_inicio".to_string());
    let theme = llimphi_theme::Theme::default();

    let registry = app_bus::AppRegistry::with_defaults();
    let apps = registry.all();
    eprintln!("menu_inicio_shot: {} apps descubiertas", apps.len());

    let bar_h = 40.0_f32;
    let screen = (W as f32, H as f32);

    // Cada estilo se pinta con el theme de SU vista, para verificar que el menú
    // sigue el acento de la paleta (XP=azul, CDE=Solaris) y no un azul fijo.
    let tema_xp = llimphi_theme::Theme::by_name("WinXP").unwrap_or_else(|| theme.clone());
    let tema_cde = llimphi_theme::Theme::by_name("CDE").unwrap_or_else(|| theme.clone());
    let estilos: [(&str, MenuStyle, View<Msg>, llimphi_theme::Theme); 3] = [
        (
            "classic",
            MenuStyle::Classic,
            render::start_menu_overlay(apps, "", 0.0, bar_h, screen.1, &theme),
            theme.clone(),
        ),
        (
            "xp",
            MenuStyle::Xp,
            render::start_menu_xp_overlay(apps, "", 0.0, bar_h, screen, &tema_xp),
            tema_xp.clone(),
        ),
        (
            "gnome",
            MenuStyle::Gnome,
            render::start_menu_gnome_overlay(apps, "", bar_h, screen, &tema_cde),
            tema_cde.clone(),
        ),
    ];

    let hal = pollster::block_on(Hal::new(None)).expect("hal");
    let mut renderer = Renderer::new(&hal).expect("renderer");

    for (nombre, _, overlay, tema) in estilos {
        let root = View::new(Style {
            flex_direction: FlexDirection::Column,
            size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
            ..Default::default()
        })
        .fill(tema.bg_app)
        .children(vec![overlay]);

        let mut layout = LayoutTree::new();
        let mounted = mount(&mut layout, root);
        let mut ts = Typesetter::new();
        let computed = {
            let tmap = &mounted.text_measures;
            layout
                .compute_with_measure(mounted.root, (W as f32, H as f32), |nid, known, avail| {
                    match tmap.get(&nid) {
                        Some(tm) => measure_text_node(&mut ts, tm, known, avail),
                        None => taffy::Size::ZERO,
                    }
                })
                .expect("layout")
        };
        let mut scene = vello::Scene::new();
        paint(&mut scene, &mounted, &computed, &mut ts, None, None);

        let target = hal.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("menu-shot"),
            size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FMT,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        renderer
            .render_to_view(&hal, &scene, &view, W, H, Color::from_rgba8(24, 24, 32, 255))
            .expect("render_to_view");
        let out = format!("{prefijo}-{nombre}.png");
        write_png(&hal, &target, &out);
        eprintln!("menu_inicio_shot: {out} ({W}x{H})");
    }
}

fn write_png(hal: &Hal, target: &wgpu::Texture, path: &str) {
    let unpadded = (W * 4) as usize;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let padded = unpadded.div_ceil(align) * align;
    let buf = hal.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded * H as usize) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = hal
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded as u32),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
    );
    hal.queue.submit(std::iter::once(enc.finish()));
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    hal.device.poll(wgpu::PollType::wait_indefinitely());
    rx.recv().unwrap().unwrap();
    let data = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((W * H * 4) as usize);
    for row in 0..H as usize {
        let s = row * padded;
        pixels.extend_from_slice(&data[s..s + unpadded]);
    }
    drop(data);
    buf.unmap();
    let file = File::create(path).expect("png");
    let mut enc = png::Encoder::new(BufWriter::new(file), W, H);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().unwrap();
    w.write_image_data(&pixels).unwrap();
}
