use std::{borrow::Cow, collections::BTreeSet};

use anyhow::Result;
use gpui_kit::assets::Assets as ComponentAssets;
use gpui_kit::{AssetSource, SharedString, TextSystem};

gpui_kit::assets::icon_assets!(
    ControlIcons,
    [
        Square,
        Keyboard,
        Pencil,
        TriangleAlert,
        Circle,
        Link,
        ListFilter
    ]
);

pub const MAPLE_MONO_NF_REGULAR: &[u8] = include_bytes!("../assets/fonts/MapleMono-NF-Regular.ttf");
pub const MAPLE_MONO_VARIABLE: &[u8] = include_bytes!("../assets/fonts/MapleMono-wght.ttf");
pub const MAPLE_MONO_LICENSE: &[u8] = include_bytes!("../assets/fonts/OFL.txt");

pub const LILEX_REGULAR: &[u8] = include_bytes!("../assets/fonts/lilex/Lilex-Regular.ttf");
pub const LILEX_BOLD: &[u8] = include_bytes!("../assets/fonts/lilex/Lilex-Bold.ttf");
pub const LILEX_ITALIC: &[u8] = include_bytes!("../assets/fonts/lilex/Lilex-Italic.ttf");
pub const LILEX_BOLD_ITALIC: &[u8] = include_bytes!("../assets/fonts/lilex/Lilex-BoldItalic.ttf");
pub const LILEX_LICENSE: &[u8] = include_bytes!("../assets/fonts/lilex/OFL.txt");

pub const IBM_PLEX_SANS_REGULAR: &[u8] =
    include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf");
pub const IBM_PLEX_SANS_SEMIBOLD: &[u8] =
    include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBold.ttf");
pub const IBM_PLEX_SANS_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-Italic.ttf");
pub const IBM_PLEX_SANS_SEMIBOLD_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBoldItalic.ttf");
pub const IBM_PLEX_SANS_LICENSE: &[u8] =
    include_bytes!("../assets/fonts/ibm-plex-sans/license.txt");

const BOOTTY_ASSETS: &[(&str, &[u8])] = &[
    (
        "icons/bootty.png",
        include_bytes!("../assets/bootty-mascot.png"),
    ),
    ("fonts/lilex/Lilex-Regular.ttf", LILEX_REGULAR),
    ("fonts/lilex/Lilex-Bold.ttf", LILEX_BOLD),
    ("fonts/lilex/Lilex-Italic.ttf", LILEX_ITALIC),
    ("fonts/lilex/Lilex-BoldItalic.ttf", LILEX_BOLD_ITALIC),
    ("fonts/lilex/OFL.txt", LILEX_LICENSE),
    (
        "fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf",
        IBM_PLEX_SANS_REGULAR,
    ),
    (
        "fonts/ibm-plex-sans/IBMPlexSans-SemiBold.ttf",
        IBM_PLEX_SANS_SEMIBOLD,
    ),
    (
        "fonts/ibm-plex-sans/IBMPlexSans-Italic.ttf",
        IBM_PLEX_SANS_ITALIC,
    ),
    (
        "fonts/ibm-plex-sans/IBMPlexSans-SemiBoldItalic.ttf",
        IBM_PLEX_SANS_SEMIBOLD_ITALIC,
    ),
    ("fonts/ibm-plex-sans/license.txt", IBM_PLEX_SANS_LICENSE),
    ("fonts/MapleMono-NF-Regular.ttf", MAPLE_MONO_NF_REGULAR),
    ("fonts/MapleMono-wght.ttf", MAPLE_MONO_VARIABLE),
    ("fonts/OFL.txt", MAPLE_MONO_LICENSE),
];

#[derive(Clone, Copy, Debug, Default)]
pub struct BoottyAssets;

impl BoottyAssets {
    /// Register complete families so bundled faces do not hide installed styles.
    ///
    /// # Errors
    /// Returns an error when the text system rejects font data.
    pub fn load_fonts(&self, text_system: &TextSystem) -> Result<()> {
        let database = crate::font_database::system_font_database();
        let families: BTreeSet<_> = database
            .faces()
            .filter(|face| matches!(face.source, fontdb::Source::Binary(_)))
            .flat_map(|face| face.families.iter().map(|(name, _)| name.as_str()))
            .collect();
        let mut names = BTreeSet::new();
        // GPUI prefers an in-memory family over the system family as a whole.
        // Include installed siblings, keeping bundled versions first for shaping parity.
        let fonts = database
            .faces()
            .filter(|face| {
                face.families
                    .iter()
                    .any(|(name, _)| families.contains(name.as_str()))
            })
            .filter(|face| names.insert(face.post_script_name.as_str()))
            .filter_map(|face| {
                database.with_face_data(face.id, |bytes, _| Cow::Owned(bytes.to_vec()))
            })
            .collect();
        text_system.add_fonts(fonts)
    }
}

impl AssetSource for BoottyAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, data)) = BOOTTY_ASSETS.iter().find(|(asset, _)| *asset == path) {
            return Ok(Some(Cow::Borrowed(*data)));
        }
        if let Ok(Some(data)) = ComponentAssets.load(path) {
            return Ok(Some(data));
        }
        ControlIcons.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut assets = BOOTTY_ASSETS
            .iter()
            .map(|(asset, _)| *asset)
            .filter(|asset| asset.starts_with(path))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        assets.extend(
            ComponentAssets
                .list(path)?
                .into_iter()
                .map(|asset| asset.to_string()),
        );
        assets.extend(
            ControlIcons
                .list(path)?
                .into_iter()
                .map(|asset| asset.to_string()),
        );
        Ok(assets.into_iter().map(Into::into).collect())
    }
}
