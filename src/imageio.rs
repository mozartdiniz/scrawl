//! Loading and saving bitmaps, and moving them through the clipboard.
//!
//! Everything is converted to a cairo ARGB32 surface (native-endian,
//! premultiplied) on the way in, which is what the renderer draws from.

use std::path::Path;

use anyhow::{bail, Context as _};
use gtk::prelude::*;
use gtk::{cairo, gdk, gdk_pixbuf, glib};

/// Load an image file, honouring its EXIF orientation like Preview does.
/// Also returns the resolution stored in the file (PNG pHYs, JPEG density).
pub fn load_file(path: &Path) -> anyhow::Result<(cairo::ImageSurface, Option<f64>)> {
    let pixbuf =
        gdk_pixbuf::Pixbuf::from_file(path).with_context(|| format!("could not read {}", path.display()))?;
    let dpi = pixbuf.option("x-dpi").and_then(|v| v.parse::<f64>().ok()).filter(|dpi| *dpi > 0.0);
    let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
    Ok((surface_from_pixbuf(&pixbuf)?, dpi))
}

fn surface_from_pixbuf(pixbuf: &gdk_pixbuf::Pixbuf) -> anyhow::Result<cairo::ImageSurface> {
    let (w, h) = (pixbuf.width(), pixbuf.height());
    let channels = pixbuf.n_channels() as usize;
    let src_stride = pixbuf.rowstride() as usize;
    let bytes = pixbuf.read_pixel_bytes();

    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h)?;
    let dst_stride = surface.stride() as usize;
    {
        let mut dst = surface.data()?;
        for y in 0..h as usize {
            for x in 0..w as usize {
                let s = &bytes[y * src_stride + x * channels..][..channels];
                let a = if channels == 4 { s[3] as u32 } else { 255 };
                let pre = |c: u8| (c as u32 * a + 127) / 255;
                let pixel = a << 24 | pre(s[0]) << 16 | pre(s[1]) << 8 | pre(s[2]);
                dst[y * dst_stride + x * 4..][..4].copy_from_slice(&pixel.to_ne_bytes());
            }
        }
    }
    Ok(surface)
}

/// Clipboard textures arrive in whatever format the source app chose;
/// `download` hands them back in cairo's layout.
pub fn surface_from_texture(texture: &gdk::Texture) -> anyhow::Result<cairo::ImageSurface> {
    let (w, h) = (texture.width(), texture.height());
    let stride = cairo::Format::ARgb32.stride_for_width(w as u32)?;
    let mut data = vec![0u8; stride as usize * h as usize];
    texture.download(&mut data, stride as usize);
    Ok(cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, w, h, stride)?)
}

pub fn texture_from_surface(surface: cairo::ImageSurface) -> anyhow::Result<gdk::Texture> {
    let (w, h, stride) = (surface.width(), surface.height(), surface.stride() as usize);
    let data = surface.take_data().map_err(|e| anyhow::anyhow!("surface busy: {e}"))?;
    let format = if cfg!(target_endian = "little") {
        gdk::MemoryFormat::B8g8r8a8Premultiplied
    } else {
        gdk::MemoryFormat::A8r8g8b8Premultiplied
    };
    let bytes = glib::Bytes::from(&data[..]);
    Ok(gdk::MemoryTexture::new(w, h, format, &bytes, stride).upcast())
}

/// Write a flattened image; the format follows the file extension. `dpi`
/// is stored in PNG and JPEG metadata when known.
pub fn save(surface: cairo::ImageSurface, path: &Path, dpi: Option<f64>) -> anyhow::Result<()> {
    let dpi = dpi.map(|d| d.round().max(1.0).to_string());
    let with_dpi = |mut options: Vec<(&'static str, String)>| {
        if let Some(dpi) = &dpi {
            options.push(("x-dpi", dpi.clone()));
            options.push(("y-dpi", dpi.clone()));
        }
        options
    };
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" => {
            let options = with_dpi(vec![("quality", "92".into())]);
            pixbuf_from_surface(surface, false)?.savev(path, "jpeg", &as_refs(&options))?
        }
        "webp" | "tiff" | "bmp" => {
            let format = if ext == "webp" { "webp" } else { ext.as_str() };
            pixbuf_from_surface(surface, true)?.savev(path, format, &[])?
        }
        // Cairo's PNG writer can't store a resolution; gdk-pixbuf's can.
        "png" | "" if dpi.is_some() => {
            let options = with_dpi(Vec::new());
            pixbuf_from_surface(surface, true)?.savev(path, "png", &as_refs(&options))?
        }
        "png" | "" => {
            let mut file = std::fs::File::create(path)?;
            surface.write_to_png(&mut file)?;
        }
        other => bail!("unsupported format “{other}”, save as .png or .jpg"),
    }
    Ok(())
}

fn as_refs<'a>(options: &'a [(&'static str, String)]) -> Vec<(&'a str, &'a str)> {
    options.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// Un-premultiply into an RGB(A) pixbuf. Without alpha (JPEG), transparent
/// areas are composited over white.
fn pixbuf_from_surface(
    mut surface: cairo::ImageSurface,
    keep_alpha: bool,
) -> anyhow::Result<gdk_pixbuf::Pixbuf> {
    let (w, h) = (surface.width(), surface.height());
    let src_stride = surface.stride() as usize;
    let channels = if keep_alpha { 4 } else { 3 };
    let dst_stride = w as usize * channels;
    let src = surface.data()?;

    let mut out = vec![0u8; dst_stride * h as usize];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let px = u32::from_ne_bytes(src[y * src_stride + x * 4..][..4].try_into().unwrap());
            let a = px >> 24;
            let channel = |shift: u32| {
                let c = (px >> shift) & 0xff;
                if keep_alpha {
                    (c * 255 + a / 2).checked_div(a).unwrap_or(0)
                } else {
                    c + (255 - a) // premultiplied over white
                }
            };
            let d = &mut out[y * dst_stride + x * channels..][..channels];
            d[0] = channel(16).min(255) as u8;
            d[1] = channel(8).min(255) as u8;
            d[2] = channel(0).min(255) as u8;
            if keep_alpha {
                d[3] = a as u8;
            }
        }
    }
    Ok(gdk_pixbuf::Pixbuf::from_mut_slice(
        out,
        gdk_pixbuf::Colorspace::Rgb,
        keep_alpha,
        8,
        w,
        h,
        dst_stride as i32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_survives_png_and_jpeg_round_trips() {
        let dir = std::env::temp_dir();
        for name in ["scrawl-dpi-test.png", "scrawl-dpi-test.jpg"] {
            let path = dir.join(name);
            let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 8, 6).unwrap();
            save(surface, &path, Some(144.0)).unwrap();
            let (loaded, dpi) = load_file(&path).unwrap();
            assert_eq!((loaded.width(), loaded.height()), (8, 6), "{name}");
            assert_eq!(dpi, Some(144.0), "{name}");
            std::fs::remove_file(path).ok();
        }
    }
}
