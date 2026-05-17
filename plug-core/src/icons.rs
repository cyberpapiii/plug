use std::collections::BTreeSet;

use rmcp::model::Icon;

const MAX_ICONS: usize = 4;
const MAX_ICON_SRC_BYTES: usize = 32 * 1024;

const MIME_PNG: &str = "image/png";
const MIME_JPEG: &str = "image/jpeg";
const MIME_JPG: &str = "image/jpg";
const MIME_WEBP: &str = "image/webp";

/// Normalize untrusted MCP icon metadata before forwarding it downstream.
///
/// Plug does not fetch or transcode icon bytes here. It keeps the spec-shaped
/// metadata safe to forward by rejecting unsafe URI schemes, oversized inline
/// data URIs, executable SVGs, unknown declared formats, and invalid size tokens.
pub fn normalize_icons(icons: Option<&[Icon]>) -> Option<Vec<Icon>> {
    let mut normalized = Vec::new();

    for icon in icons.into_iter().flatten() {
        if normalized.len() >= MAX_ICONS {
            break;
        }
        if let Some(icon) = normalize_icon(icon) {
            normalized.push(icon);
        }
    }

    (!normalized.is_empty()).then_some(normalized)
}

pub fn normalize_icon(icon: &Icon) -> Option<Icon> {
    let src = icon.src.trim();
    if !is_safe_icon_src(src) {
        return None;
    }

    let mime_type = match icon
        .mime_type
        .as_deref()
        .map(str::to_string)
        .or_else(|| infer_mime_type_from_src(src))
    {
        Some(mime_type) => Some(normalize_mime_type(&mime_type)?),
        None => None,
    };
    let sizes = normalize_sizes(icon.sizes.as_deref());

    let mut normalized = Icon::new(src.to_string());
    if let Some(mime_type) = mime_type {
        normalized = normalized.with_mime_type(mime_type);
    }
    if let Some(sizes) = sizes {
        normalized = normalized.with_sizes(sizes);
    }
    if let Some(theme) = icon.theme {
        normalized = normalized.with_theme(theme);
    }

    Some(normalized)
}

fn is_safe_icon_src(src: &str) -> bool {
    if src.is_empty() || src.len() > MAX_ICON_SRC_BYTES {
        return false;
    }

    let lower = src.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return true;
    }

    lower.starts_with("data:")
        && lower.len() <= MAX_ICON_SRC_BYTES
        && lower.contains(";base64,")
        && infer_mime_type_from_data_uri(&lower)
            .and_then(normalize_mime_type)
            .is_some()
}

fn normalize_mime_type(mime_type: &str) -> Option<String> {
    let mime = mime_type.trim().to_ascii_lowercase();
    let normalized = match mime.as_str() {
        MIME_PNG => MIME_PNG,
        MIME_JPEG | MIME_JPG => MIME_JPEG,
        MIME_WEBP => MIME_WEBP,
        _ => return None,
    };
    Some(normalized.to_string())
}

fn infer_mime_type_from_src(src: &str) -> Option<String> {
    let lower = src.to_ascii_lowercase();
    if lower.starts_with("data:") {
        return infer_mime_type_from_data_uri(&lower).map(str::to_string);
    }

    let without_query = lower
        .split('?')
        .next()
        .unwrap_or(lower.as_str())
        .split('#')
        .next()
        .unwrap_or(lower.as_str());

    if without_query.ends_with(".png") {
        Some(MIME_PNG.to_string())
    } else if without_query.ends_with(".jpg") || without_query.ends_with(".jpeg") {
        Some(MIME_JPEG.to_string())
    } else if without_query.ends_with(".webp") {
        Some(MIME_WEBP.to_string())
    } else {
        None
    }
}

fn infer_mime_type_from_data_uri(src: &str) -> Option<&str> {
    src.strip_prefix("data:")
        .and_then(|rest| rest.split(';').next())
}

fn normalize_sizes(sizes: Option<&[String]>) -> Option<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::new();

    for size in sizes.into_iter().flatten() {
        let trimmed = size.trim().to_ascii_lowercase();
        if !is_valid_size(&trimmed) || !seen.insert(trimmed.clone()) {
            continue;
        }
        normalized.push(trimmed);
    }

    (!normalized.is_empty()).then_some(normalized)
}

fn is_valid_size(size: &str) -> bool {
    if size == "any" {
        return true;
    }

    let Some((width, height)) = size.split_once('x') else {
        return false;
    };
    is_positive_dimension(width) && is_positive_dimension(height)
}

fn is_positive_dimension(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|ch| ch.is_ascii_digit())
        && value.parse::<u32>().is_ok_and(|value| value > 0)
}

#[cfg(test)]
mod tests {
    use rmcp::model::{Icon, IconTheme};

    use super::*;

    #[test]
    fn https_png_with_size_survives() {
        let icon = Icon::new("https://example.com/icon.png")
            .with_mime_type("image/png")
            .with_sizes(vec!["64x64".to_string()])
            .with_theme(IconTheme::Light);

        let normalized = normalize_icon(&icon).expect("valid icon");

        assert_eq!(normalized.src, "https://example.com/icon.png");
        assert_eq!(normalized.mime_type.as_deref(), Some("image/png"));
        assert_eq!(
            normalized.sizes.as_deref(),
            Some(&["64x64".to_string()][..])
        );
        assert_eq!(normalized.theme, Some(IconTheme::Light));
    }

    #[test]
    fn jpg_alias_normalizes_to_jpeg() {
        let icon = Icon::new("https://example.com/icon.jpg").with_mime_type("image/jpg");

        let normalized = normalize_icon(&icon).expect("valid icon");

        assert_eq!(normalized.mime_type.as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn svg_icons_are_rejected_for_untrusted_upstreams() {
        let icon = Icon::new("https://example.com/icon.svg").with_mime_type("image/svg+xml");

        assert!(normalize_icon(&icon).is_none());
    }

    #[test]
    fn https_icon_without_inferable_mime_type_survives_without_mime_type() {
        let icon = Icon::new("https://cdn.example.com/icon?id=abc");

        let normalized = normalize_icon(&icon).expect("safe https icon");

        assert_eq!(normalized.src, "https://cdn.example.com/icon?id=abc");
        assert_eq!(normalized.mime_type, None);
    }

    #[test]
    fn unsafe_uri_schemes_are_rejected() {
        for src in [
            "http://example.com/icon.png",
            "file:///tmp/icon.png",
            "javascript:alert(1)",
            "ftp://example.com/icon.png",
            "ws://example.com/icon.png",
            "",
        ] {
            assert!(normalize_icon(&Icon::new(src).with_mime_type("image/png")).is_none());
        }
    }

    #[test]
    fn oversized_data_uri_is_rejected() {
        let src = format!("data:image/png;base64,{}", "a".repeat(MAX_ICON_SRC_BYTES));

        assert!(normalize_icon(&Icon::new(src).with_mime_type("image/png")).is_none());
    }

    #[test]
    fn invalid_sizes_are_dropped() {
        let icon = Icon::new("https://example.com/icon.png").with_sizes(vec![
            "64x64".to_string(),
            "0x64".to_string(),
            "64".to_string(),
            "ANY".to_string(),
            "64x64".to_string(),
        ]);

        let normalized = normalize_icon(&icon).expect("valid icon");

        assert_eq!(
            normalized.sizes.as_deref(),
            Some(&["64x64".to_string(), "any".to_string()][..])
        );
    }

    #[test]
    fn data_uri_mime_type_is_inferred() {
        let icon = Icon::new("data:image/png;base64,aGVsbG8=");

        let normalized = normalize_icon(&icon).expect("valid icon");

        assert_eq!(normalized.mime_type.as_deref(), Some("image/png"));
    }

    #[test]
    fn valid_icons_are_capped_in_original_order() {
        let icons = (0..MAX_ICONS + 1)
            .map(|index| Icon::new(format!("https://example.com/icon-{index}.png")))
            .collect::<Vec<_>>();

        let normalized = normalize_icons(Some(&icons)).expect("valid icons");

        assert_eq!(normalized.len(), MAX_ICONS);
        assert_eq!(normalized[0].src, "https://example.com/icon-0.png");
        assert_eq!(
            normalized[MAX_ICONS - 1].src,
            format!("https://example.com/icon-{}.png", MAX_ICONS - 1)
        );
    }
}
