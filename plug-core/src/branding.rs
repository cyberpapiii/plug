use std::sync::OnceLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use rmcp::model::{Icon, Implementation};

pub const PLUG_TITLE: &str = "Plug";
pub const PLUG_DESCRIPTION: &str = "MCP multiplexer";
pub const PLUG_WEBSITE_URL: &str = "https://github.com/cyberpapiii/plug";

static PLUG_ICONS: OnceLock<Vec<Icon>> = OnceLock::new();

pub fn plug_icons() -> Vec<Icon> {
    PLUG_ICONS.get_or_init(build_plug_icons).clone()
}

fn build_plug_icons() -> Vec<Icon> {
    let mut icons = vec![
        png_icon(16, include_bytes!("../../docs/assets/plug-icon-16.png")),
        png_icon(32, include_bytes!("../../docs/assets/plug-icon-32.png")),
        png_icon(64, include_bytes!("../../docs/assets/plug-icon-64.png")),
        png_icon(128, include_bytes!("../../docs/assets/plug-icon-128.png")),
        png_icon(256, include_bytes!("../../docs/assets/plug-icon-256.png")),
        png_icon(512, include_bytes!("../../docs/assets/plug-icon-512.png")),
    ];

    icons.push(svg_icon(include_bytes!("../../docs/assets/plug-icon.svg")));

    icons
}

fn png_icon(size: u32, bytes: &[u8]) -> Icon {
    Icon::new(format!(
        "data:image/png;base64,{}",
        BASE64_STANDARD.encode(bytes)
    ))
    .with_mime_type("image/png")
    .with_sizes(vec![format!("{size}x{size}")])
}

fn svg_icon(bytes: &[u8]) -> Icon {
    Icon::new(format!(
        "data:image/svg+xml;base64,{}",
        BASE64_STANDARD.encode(bytes)
    ))
    .with_mime_type("image/svg+xml")
    .with_sizes(vec!["any".to_string()])
}

pub fn plug_implementation(version: &str) -> Implementation {
    Implementation::new("plug", version)
        .with_title(PLUG_TITLE)
        .with_description(PLUG_DESCRIPTION)
        .with_website_url(PLUG_WEBSITE_URL)
        .with_icons(plug_icons())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plug_icons_are_png_first_with_svg_fallback() {
        let icons = plug_icons();

        assert_eq!(icons.len(), 7);
        assert!(icons[0].src.starts_with("data:image/png;base64,"));
        assert_eq!(icons[0].mime_type.as_deref(), Some("image/png"));
        assert_eq!(icons[0].sizes.as_deref(), Some(&["16x16".to_string()][..]));

        let last = icons.last().expect("svg fallback icon");
        assert!(last.src.starts_with("data:image/svg+xml;base64,"));
        assert_eq!(last.mime_type.as_deref(), Some("image/svg+xml"));
        assert_eq!(last.sizes.as_deref(), Some(&["any".to_string()][..]));
    }

    #[test]
    fn committed_png_icons_match_advertised_dimensions() {
        assert_png_dimensions(include_bytes!("../../docs/assets/plug-icon-16.png"), 16);
        assert_png_dimensions(include_bytes!("../../docs/assets/plug-icon-32.png"), 32);
        assert_png_dimensions(include_bytes!("../../docs/assets/plug-icon-64.png"), 64);
        assert_png_dimensions(include_bytes!("../../docs/assets/plug-icon-128.png"), 128);
        assert_png_dimensions(include_bytes!("../../docs/assets/plug-icon-256.png"), 256);
        assert_png_dimensions(include_bytes!("../../docs/assets/plug-icon-512.png"), 512);
    }

    fn assert_png_dimensions(bytes: &[u8], expected_size: u32) {
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "not a PNG file");
        let width = u32::from_be_bytes(bytes[16..20].try_into().expect("png width"));
        let height = u32::from_be_bytes(bytes[20..24].try_into().expect("png height"));

        assert_eq!(width, expected_size);
        assert_eq!(height, expected_size);
    }
}
