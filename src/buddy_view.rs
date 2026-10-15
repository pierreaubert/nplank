//! Bounded decoding of original buddy PNGs for native and browser rendering.

// Rust guideline compliant 2026-02-21
use std::{io::Cursor, sync::Arc};

pub(crate) fn cache_key(frame: &crate::buddyframe::Frame) -> Option<[u8; 32]> {
    use sha2::Digest as _;
    // A remote server can restart its revision counter within the same UI session.
    frame
        .is_valid()
        .then(|| sha2::Sha256::digest(&frame.png).into())
}

pub(crate) fn decode(frame: &crate::buddyframe::Frame) -> anyhow::Result<Arc<gpui::RenderImage>> {
    Ok(Arc::new(gpui::RenderImage::new(vec![image::Frame::new(
        pixels(frame)?,
    )])))
}

fn pixels(frame: &crate::buddyframe::Frame) -> anyhow::Result<image::RgbaImage> {
    anyhow::ensure!(frame.is_valid(), "Invalid buddy frame");
    let mut reader =
        image::ImageReader::with_format(Cursor::new(&frame.png), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(crate::buddyframe::MAX_EDGE);
    limits.max_image_height = Some(crate::buddyframe::MAX_EDGE);
    // Allow decoder scratch space while bounding pixels to the validated dimensions.
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    let mut rgba = reader.decode()?.into_rgba8();
    for pixel in rgba.pixels_mut() {
        let alpha = u16::from(pixel[3]);
        // GPUI atlases consume premultiplied BGRA, including transparent edges.
        for channel in pixel.0.iter_mut().take(3) {
            *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
        }
        pixel.0.swap(0, 2);
    }
    Ok(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_buddy_cache_changes_when_a_restarted_server_reuses_revision() {
        let make = |pixel| {
            let mut encoded = Cursor::new(Vec::new());
            image::RgbaImage::from_pixel(1, 1, image::Rgba(pixel))
                .write_to(&mut encoded, image::ImageFormat::Png)
                .unwrap();
            crate::buddyframe::Frame {
                revision: 1,
                png: encoded.into_inner(),
            }
        };
        let before = make([255, 0, 0, 255]);
        let after = make([0, 0, 255, 255]);
        assert_ne!(cache_key(&before), cache_key(&after));
        assert_eq!(cache_key(&before), cache_key(&before.clone()));
        assert!(
            cache_key(&crate::buddyframe::Frame {
                revision: 1,
                png: Vec::new()
            })
            .is_none()
        );
    }

    #[test]
    fn graphical_buddy_decode_preserves_color_transparency_and_rejects_corruption() {
        let image =
            image::RgbaImage::from_raw(2, 1, vec![200, 100, 50, 128, 255, 0, 0, 0]).unwrap();
        let mut encoded = Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let mut frame = crate::buddyframe::Frame {
            revision: 1,
            png: encoded.into_inner(),
        };
        let decoded = pixels(&frame).unwrap();
        assert_eq!(decoded.dimensions(), (2, 1));
        assert_eq!(decoded.get_pixel(0, 0).0, [25, 50, 100, 128]);
        assert_eq!(decoded.get_pixel(1, 0).0, [0, 0, 0, 0]);
        frame.png.truncate(24);
        assert!(
            frame.is_valid(),
            "header validation precedes complete decoding"
        );
        assert!(pixels(&frame).is_err());
    }
}
