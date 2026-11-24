use std::path::PathBuf;
use std::sync::OnceLock;

/// Icon names; also the basenames of the installed SVGs.
pub const IDLE: &str = "meeting-recorder";
pub const RECORDING: &str = "meeting-recorder-recording";

const SVGS: &[(&str, &str)] = &[
    (IDLE, include_str!("../assets/icons/meeting-recorder.svg")),
    (
        RECORDING,
        include_str!("../assets/icons/meeting-recorder-recording.svg"),
    ),
];

/// Pre-rendered from the SVGs with
/// `rsvg-convert -w N -h N assets/icons/X.svg -o assets/icons/png/X-N.png`.
const IDLE_PNGS: &[&[u8]] = &[
    include_bytes!("../assets/icons/png/meeting-recorder-22.png"),
    include_bytes!("../assets/icons/png/meeting-recorder-32.png"),
    include_bytes!("../assets/icons/png/meeting-recorder-48.png"),
    include_bytes!("../assets/icons/png/meeting-recorder-64.png"),
];
const RECORDING_PNGS: &[&[u8]] = &[
    include_bytes!("../assets/icons/png/meeting-recorder-recording-22.png"),
    include_bytes!("../assets/icons/png/meeting-recorder-recording-32.png"),
    include_bytes!("../assets/icons/png/meeting-recorder-recording-48.png"),
    include_bytes!("../assets/icons/png/meeting-recorder-recording-64.png"),
];

/// Tray icon as raw ARGB pixmaps. Sent directly over D-Bus, so it shows on any
/// StatusNotifierItem host without icon-theme lookup. (Plasma's tray loader ignores icons in
/// `~/.local/share/icons/hicolor` that lack an `index.theme`, and caches lookups.)
pub fn tray_pixmaps(recording: bool) -> Vec<ksni::Icon> {
    static IDLE_PIX: OnceLock<Vec<ksni::Icon>> = OnceLock::new();
    static REC_PIX: OnceLock<Vec<ksni::Icon>> = OnceLock::new();
    let (cell, pngs) = if recording {
        (&REC_PIX, RECORDING_PNGS)
    } else {
        (&IDLE_PIX, IDLE_PNGS)
    };
    cell.get_or_init(|| pngs.iter().filter_map(|b| decode(b)).collect())
        .clone()
}

fn decode(bytes: &[u8]) -> Option<ksni::Icon> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|e| tracing::error!("bundled icon PNG unreadable: {e}"))
        .ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| tracing::error!("bundled icon PNG decode failed: {e}"))
        .ok()?;
    let px = &buf[..info.buffer_size()];
    // SNI wants ARGB32 in network byte order: A, R, G, B per pixel.
    let data: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => px
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|&[r, g, b, a]| [a, r, g, b])
            .collect(),
        png::ColorType::Rgb => px
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [255, r, g, b])
            .collect(),
        other => {
            tracing::error!("bundled icon has unsupported color type {other:?}");
            return None;
        }
    };
    Some(ksni::Icon {
        width: info.width as i32,
        height: info.height as i32,
        data,
    })
}

fn svg_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| {
        d.join("icons")
            .join("hicolor")
            .join("scalable")
            .join("apps")
    })
}

/// Writes the bundled SVGs to `~/.local/share/icons/hicolor/scalable/apps/` (used by the
/// notification icon and the `.desktop` file). Only rewrites files whose content changed.
/// Failures are logged and non-fatal.
pub fn install() {
    let Some(dir) = svg_dir() else {
        tracing::warn!("no XDG data dir; app icons not installed");
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("could not create icon dir {:?}: {e}", dir);
        return;
    }
    for (name, svg) in SVGS {
        let path = dir.join(format!("{name}.svg"));
        if std::fs::read_to_string(&path).is_ok_and(|cur| cur == *svg) {
            continue;
        }
        match std::fs::write(&path, svg) {
            Ok(()) => tracing::info!("installed icon {:?}", path),
            Err(e) => tracing::warn!("could not write icon {:?}: {e}", path),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bundled_pngs_decode_to_argb() {
        for recording in [false, true] {
            let icons = super::tray_pixmaps(recording);
            assert_eq!(icons.len(), 4);
            for i in &icons {
                assert_eq!(i.data.len(), (i.width * i.height * 4) as usize);
            }
            let sizes: Vec<i32> = icons.iter().map(|i| i.width).collect();
            assert_eq!(sizes, [22, 32, 48, 64]);
        }
    }
}
