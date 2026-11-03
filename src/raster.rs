//! Image decoding helpers for screenshots and app icons.

pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl DecodedImage {
    pub fn to_color_image(&self) -> eframe::egui::ColorImage {
        eframe::egui::ColorImage::from_rgba_unmultiplied(
            [self.width as usize, self.height as usize],
            &self.rgba,
        )
    }
}

pub fn decode(bytes: &[u8]) -> Result<DecodedImage, String> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    Ok(DecodedImage {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    })
}
