use serde::Deserialize;

use super::load::{ConfigLoadError, ConfigResult};
use super::model::{ColorConfig, ResolvedTheme, ThemeInfo};
use super::raw::ColorPatch;
use super::resolve::apply_partial_colors;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawTheme {
    #[serde(default)]
    metadata: ThemeMetadata,
    #[serde(default)]
    colors: ColorPatch,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct ThemeMetadata {
    name: Option<String>,
    source: Option<String>,
    license: Option<String>,
}

pub(super) fn load_builtin_theme(theme: &str) -> Option<ResolvedTheme> {
    BUILTIN_THEMES
        .iter()
        .find(|builtin| theme_name_matches(builtin.name, theme))
        .map(|builtin| builtin.resolve())
}

pub(super) fn default_light_colors() -> ColorConfig {
    (ZED_ONE_LIGHT_THEME.colors)()
}

pub(super) fn default_dark_colors() -> ColorConfig {
    (ZED_ONE_DARK_THEME.colors)()
}

fn theme_name_matches(candidate: &str, requested: &str) -> bool {
    candidate.eq_ignore_ascii_case(requested)
        || requested
            .strip_prefix("iTerm2 ")
            .is_some_and(|stripped| candidate.eq_ignore_ascii_case(stripped))
}

pub fn builtin_theme_names() -> impl Iterator<Item = &'static str> {
    BUILTIN_THEMES.iter().map(|theme| theme.name)
}

///
/// # Errors
/// Returns an error for malformed TOML or invalid theme fields and colors.
pub fn parse_theme_source(source: &str, label: &str) -> ConfigResult<ResolvedTheme> {
    let raw: RawTheme = toml_edit::de::from_str(source)
        .map_err(|error| ConfigLoadError::new(format!("failed to parse theme {label}: {error}")))?;
    let mut colors = ColorConfig::default();
    apply_partial_colors(&mut colors, raw.colors);
    Ok(ResolvedTheme {
        info: ThemeInfo {
            name: raw.metadata.name.unwrap_or_else(|| label.to_owned()),
            source: raw.metadata.source.unwrap_or_default(),
            license: raw.metadata.license.unwrap_or_default(),
        },
        colors,
    })
}

struct BuiltinTheme {
    name: &'static str,
    metadata_name: &'static str,
    source: &'static str,
    license: &'static str,
    colors: fn() -> ColorConfig,
}

impl BuiltinTheme {
    fn resolve(&self) -> ResolvedTheme {
        ResolvedTheme {
            info: ThemeInfo {
                name: self.metadata_name.to_owned(),
                source: self.source.to_owned(),
                license: self.license.to_owned(),
            },
            colors: (self.colors)(),
        }
    }
}

const fn rgba(value: u32) -> crate::color::Color {
    let [r, g, b, a] = value.to_be_bytes();
    crate::color::Color { r, g, b, a }
}

pub const DEFAULT_LIGHT_THEME: &str = ZED_ONE_LIGHT_THEME.name;
pub const DEFAULT_DARK_THEME: &str = ZED_ONE_DARK_THEME.name;
const BUILTIN_THEMES: &[&BuiltinTheme] = &[
    &CATPPUCCIN_MOCHA_THEME,
    &CATPPUCCIN_LATTE_THEME,
    &CATPPUCCIN_FRAPPE_THEME,
    &CATPPUCCIN_MACCHIATO_THEME,
    &ZED_ONE_DARK_THEME,
    &ZED_ONE_LIGHT_THEME,
    &AYU_THEME,
    &AYU_LIGHT_THEME,
    &AYU_MIRAGE_THEME,
    &DRACULA_THEME,
    &EVERFOREST_DARK_HARD_THEME,
    &EVERFOREST_DARK_MED_THEME,
    &EVERFOREST_DARK_SOFT_THEME,
    &EVERFOREST_LIGHT_HARD_THEME,
    &EVERFOREST_LIGHT_MED_THEME,
    &EVERFOREST_LIGHT_SOFT_THEME,
    &FLEXOKI_DARK_THEME,
    &FLEXOKI_LIGHT_THEME,
    &KANAGAWA_DRAGON_THEME,
    &KANAGAWA_LOTUS_THEME,
    &KANAGAWA_WAVE_THEME,
    &ROSE_PINE_THEME,
    &ROSE_PINE_DAWN_THEME,
    &ROSE_PINE_MOON_THEME,
    &TOKYONIGHT_NIGHT_THEME,
    &TOKYONIGHT_DAY_THEME,
    &TOKYONIGHT_MOON_THEME,
    &TOKYONIGHT_STORM_THEME,
    &ITERM2_SOLARIZED_DARK_THEME,
    &ITERM2_SOLARIZED_LIGHT_THEME,
    &XCODE_DARK_THEME,
    &XCODE_LIGHT_THEME,
    &GRUVBOX_DARK_THEME,
];

const CATPPUCCIN_MOCHA_THEME: BuiltinTheme = BuiltinTheme {
    name: "Catppuccin Mocha",
    metadata_name: "Catppuccin Mocha",
    source: "catppuccin/ghostty and mbadolato/iTerm2-Color-Schemes ghostty/Catppuccin Mocha",
    license: "MIT",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x45_47_5a_ff),
            rgba(0xf3_8b_a8_ff),
            rgba(0xa6_e3_a1_ff),
            rgba(0xf9_e2_af_ff),
            rgba(0x89_b4_fa_ff),
            rgba(0xf5_c2_e7_ff),
            rgba(0x94_e2_d5_ff),
            rgba(0xa6_ad_c8_ff),
            rgba(0x58_5b_70_ff),
            rgba(0xf3_77_99_ff),
            rgba(0x89_d8_8b_ff),
            rgba(0xeb_d3_91_ff),
            rgba(0x74_a8_fc_ff),
            rgba(0xf2_ae_de_ff),
            rgba(0x6b_d7_ca_ff),
            rgba(0xba_c2_de_ff),
        ],
        background: Some(rgba(0x1e_1e_2e_ff)),
        foreground: Some(rgba(0xcd_d6_f4_ff)),
        cursor: Some(rgba(0xf5_e0_dc_ff)),
        cursor_text: Some(rgba(0x1e_1e_2e_ff)),
        selection_background: Some(rgba(0x58_5b_70_ff)),
        selection_foreground: Some(rgba(0xcd_d6_f4_ff)),
        ..ColorConfig::default()
    },
};

const CATPPUCCIN_LATTE_THEME: BuiltinTheme = BuiltinTheme {
    name: "Catppuccin Latte",
    metadata_name: "Catppuccin Latte",
    source: "catppuccin/ghostty and mbadolato/iTerm2-Color-Schemes ghostty/Catppuccin Latte",
    license: "MIT",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x5c_5f_77_ff),
            rgba(0xd2_0f_39_ff),
            rgba(0x40_a0_2b_ff),
            rgba(0xdf_8e_1d_ff),
            rgba(0x1e_66_f5_ff),
            rgba(0xea_76_cb_ff),
            rgba(0x17_92_99_ff),
            rgba(0xac_b0_be_ff),
            rgba(0x6c_6f_85_ff),
            rgba(0xd2_0f_39_ff),
            rgba(0x40_a0_2b_ff),
            rgba(0xdf_8e_1d_ff),
            rgba(0x1e_66_f5_ff),
            rgba(0xea_76_cb_ff),
            rgba(0x17_92_99_ff),
            rgba(0xbc_c0_cc_ff),
        ],
        background: Some(rgba(0xef_f1_f5_ff)),
        foreground: Some(rgba(0x4c_4f_69_ff)),
        cursor: Some(rgba(0xdc_8a_78_ff)),
        cursor_text: Some(rgba(0xef_f1_f5_ff)),
        selection_background: Some(rgba(0xac_b0_be_ff)),
        selection_foreground: Some(rgba(0x4c_4f_69_ff)),
        ..ColorConfig::default()
    },
};

const CATPPUCCIN_FRAPPE_THEME: BuiltinTheme = BuiltinTheme {
    name: "Catppuccin Frappe",
    metadata_name: "Catppuccin Frappe",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Catppuccin Frappe",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x51_57_6d_ff),
            rgba(0xe7_82_84_ff),
            rgba(0xa6_d1_89_ff),
            rgba(0xe5_c8_90_ff),
            rgba(0x8c_aa_ee_ff),
            rgba(0xf4_b8_e4_ff),
            rgba(0x81_c8_be_ff),
            rgba(0xb5_bf_e2_ff),
            rgba(0x62_68_80_ff),
            rgba(0xed_a0_a2_ff),
            rgba(0xb9_db_a2_ff),
            rgba(0xec_d7_ae_ff),
            rgba(0xad_c2_f3_ff),
            rgba(0xf3_8e_d8_ff),
            rgba(0x98_d2_ca_ff),
            rgba(0xa5_ad_ce_ff),
        ],
        background: Some(rgba(0x30_34_46_ff)),
        foreground: Some(rgba(0xc6_d0_f5_ff)),
        cursor: Some(rgba(0xf2_d5_cf_ff)),
        cursor_text: Some(rgba(0x30_34_46_ff)),
        selection_background: Some(rgba(0xf2_d5_cf_ff)),
        selection_foreground: Some(rgba(0x30_34_46_ff)),
        ..ColorConfig::default()
    },
};

const CATPPUCCIN_MACCHIATO_THEME: BuiltinTheme = BuiltinTheme {
    name: "Catppuccin Macchiato",
    metadata_name: "Catppuccin Macchiato",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Catppuccin Macchiato",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x49_4d_64_ff),
            rgba(0xed_87_96_ff),
            rgba(0xa6_da_95_ff),
            rgba(0xee_d4_9f_ff),
            rgba(0x8a_ad_f4_ff),
            rgba(0xf5_bd_e6_ff),
            rgba(0x8b_d5_ca_ff),
            rgba(0xb8_c0_e0_ff),
            rgba(0x5b_60_78_ff),
            rgba(0xf2_a7_b2_ff),
            rgba(0xbd_e3_b0_ff),
            rgba(0xf4_e3_c1_ff),
            rgba(0xad_c5_f7_ff),
            rgba(0xf4_93_da_ff),
            rgba(0xa5_de_d6_ff),
            rgba(0xa5_ad_cb_ff),
        ],
        background: Some(rgba(0x24_27_3a_ff)),
        foreground: Some(rgba(0xca_d3_f5_ff)),
        cursor: Some(rgba(0xf4_db_d6_ff)),
        cursor_text: Some(rgba(0x24_27_3a_ff)),
        selection_background: Some(rgba(0xf4_db_d6_ff)),
        selection_foreground: Some(rgba(0x24_27_3a_ff)),
        ..ColorConfig::default()
    },
};

const ZED_ONE_DARK_THEME: BuiltinTheme = BuiltinTheme {
    name: "One Dark",
    metadata_name: "One Dark",
    source: "zed-industries/zed assets/themes/one/one.json",
    license: "GPL-3.0-or-later",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x28_2c_34_ff),
            rgba(0xe0_6c_75_ff),
            rgba(0x98_c3_79_ff),
            rgba(0xe5_c0_7b_ff),
            rgba(0x61_af_ef_ff),
            rgba(0xc6_78_dd_ff),
            rgba(0x56_b6_c2_ff),
            rgba(0xab_b2_bf_ff),
            rgba(0x63_6d_83_ff),
            rgba(0xea_85_8b_ff),
            rgba(0xaa_d5_81_ff),
            rgba(0xff_d8_85_ff),
            rgba(0x85_c1_ff_ff),
            rgba(0xd3_98_eb_ff),
            rgba(0x6e_d5_de_ff),
            rgba(0xfa_fa_fa_ff),
        ],
        background: Some(rgba(0x28_2c_34_ff)),
        foreground: Some(rgba(0xab_b2_bf_ff)),
        cursor: Some(rgba(0x74_ad_e8_ff)),
        cursor_text: Some(rgba(0x11_11_10_ff)),
        selection_background: Some(rgba(0x74_ad_e8_3d)),
        ..ColorConfig::default()
    },
};

const ZED_ONE_LIGHT_THEME: BuiltinTheme = BuiltinTheme {
    name: "One Light",
    metadata_name: "One Light",
    source: "zed-industries/zed assets/themes/one/one.json",
    license: "GPL-3.0-or-later",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x00_00_00_ff),
            rgba(0xde_3e_35_ff),
            rgba(0x3f_95_3a_ff),
            rgba(0xd2_b6_7c_ff),
            rgba(0x2f_5a_f3_ff),
            rgba(0x95_00_95_ff),
            rgba(0x09_97_b3_ff),
            rgba(0xbb_bb_bb_ff),
            rgba(0x00_00_00_ff),
            rgba(0xde_3e_35_ff),
            rgba(0x3f_95_3a_ff),
            rgba(0xd2_b6_7c_ff),
            rgba(0x2f_5a_f3_ff),
            rgba(0xa0_00_95_ff),
            rgba(0x0b_bc_d6_ff),
            rgba(0xff_ff_ff_ff),
        ],
        background: Some(rgba(0xfa_fa_fa_ff)),
        foreground: Some(rgba(0x2a_2c_33_ff)),
        cursor: Some(rgba(0x5c_78_e2_ff)),
        cursor_text: Some(rgba(0xfd_fd_fc_ff)),
        selection_background: Some(rgba(0x5c_78_e2_3d)),
        ..ColorConfig::default()
    },
};

const AYU_THEME: BuiltinTheme = BuiltinTheme {
    name: "Ayu",
    metadata_name: "Ayu",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Ayu",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x11_15_1c_ff),
            rgba(0xea_6c_73_ff),
            rgba(0x7f_d9_62_ff),
            rgba(0xf9_af_4f_ff),
            rgba(0x53_bd_fa_ff),
            rgba(0xcd_a1_fa_ff),
            rgba(0x90_e1_c6_ff),
            rgba(0xc7_c7_c7_ff),
            rgba(0x68_68_68_ff),
            rgba(0xf0_71_78_ff),
            rgba(0xaa_d9_4c_ff),
            rgba(0xff_b4_54_ff),
            rgba(0x59_c2_ff_ff),
            rgba(0xd2_a6_ff_ff),
            rgba(0x95_e6_cb_ff),
            rgba(0xff_ff_ff_ff),
        ],
        background: Some(rgba(0x0b_0e_14_ff)),
        foreground: Some(rgba(0xbf_bd_b6_ff)),
        cursor: Some(rgba(0xe6_b4_50_ff)),
        cursor_text: Some(rgba(0x0b_0e_14_ff)),
        selection_background: Some(rgba(0x40_9f_ff_ff)),
        selection_foreground: Some(rgba(0x0b_0e_14_ff)),
        ..ColorConfig::default()
    },
};

const AYU_LIGHT_THEME: BuiltinTheme = BuiltinTheme {
    name: "Ayu Light",
    metadata_name: "Ayu Light",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Ayu Light",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x00_00_00_ff),
            rgba(0xea_6c_6d_ff),
            rgba(0x6c_bf_43_ff),
            rgba(0xec_a9_44_ff),
            rgba(0x31_99_e1_ff),
            rgba(0x9e_75_c7_ff),
            rgba(0x46_ba_94_ff),
            rgba(0xba_ba_ba_ff),
            rgba(0x68_68_68_ff),
            rgba(0xf0_71_71_ff),
            rgba(0x86_b3_00_ff),
            rgba(0xf2_ae_49_ff),
            rgba(0x39_9e_e6_ff),
            rgba(0xa3_7a_cc_ff),
            rgba(0x4c_bf_99_ff),
            rgba(0xd1_d1_d1_ff),
        ],
        background: Some(rgba(0xf8_f9_fa_ff)),
        foreground: Some(rgba(0x5c_61_66_ff)),
        cursor: Some(rgba(0xff_aa_33_ff)),
        cursor_text: Some(rgba(0xf8_f9_fa_ff)),
        selection_background: Some(rgba(0x03_5b_d6_ff)),
        selection_foreground: Some(rgba(0xf8_f9_fa_ff)),
        ..ColorConfig::default()
    },
};

const AYU_MIRAGE_THEME: BuiltinTheme = BuiltinTheme {
    name: "Ayu Mirage",
    metadata_name: "Ayu Mirage",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Ayu Mirage",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x17_1b_24_ff),
            rgba(0xed_82_74_ff),
            rgba(0x87_d9_6c_ff),
            rgba(0xfa_cc_6e_ff),
            rgba(0x6d_cb_fa_ff),
            rgba(0xda_ba_fa_ff),
            rgba(0x90_e1_c6_ff),
            rgba(0xc7_c7_c7_ff),
            rgba(0x68_68_68_ff),
            rgba(0xf2_87_79_ff),
            rgba(0xd5_ff_80_ff),
            rgba(0xff_d1_73_ff),
            rgba(0x73_d0_ff_ff),
            rgba(0xdf_bf_ff_ff),
            rgba(0x95_e6_cb_ff),
            rgba(0xff_ff_ff_ff),
        ],
        background: Some(rgba(0x1f_24_30_ff)),
        foreground: Some(rgba(0xcc_ca_c2_ff)),
        cursor: Some(rgba(0xff_cc_66_ff)),
        cursor_text: Some(rgba(0x1f_24_30_ff)),
        selection_background: Some(rgba(0x40_9f_ff_ff)),
        selection_foreground: Some(rgba(0x1f_24_30_ff)),
        ..ColorConfig::default()
    },
};

const DRACULA_THEME: BuiltinTheme = BuiltinTheme {
    name: "Dracula",
    metadata_name: "Dracula",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Dracula",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x21_22_2c_ff),
            rgba(0xff_55_55_ff),
            rgba(0x50_fa_7b_ff),
            rgba(0xf1_fa_8c_ff),
            rgba(0xbd_93_f9_ff),
            rgba(0xff_79_c6_ff),
            rgba(0x8b_e9_fd_ff),
            rgba(0xf8_f8_f2_ff),
            rgba(0x62_72_a4_ff),
            rgba(0xff_6e_6e_ff),
            rgba(0x69_ff_94_ff),
            rgba(0xff_ff_a5_ff),
            rgba(0xd6_ac_ff_ff),
            rgba(0xff_92_df_ff),
            rgba(0xa4_ff_ff_ff),
            rgba(0xff_ff_ff_ff),
        ],
        background: Some(rgba(0x28_2a_36_ff)),
        foreground: Some(rgba(0xf8_f8_f2_ff)),
        cursor: Some(rgba(0xf8_f8_f2_ff)),
        cursor_text: Some(rgba(0x28_2a_36_ff)),
        selection_background: Some(rgba(0x44_47_5a_ff)),
        selection_foreground: Some(rgba(0xff_ff_ff_ff)),
        ..ColorConfig::default()
    },
};

const EVERFOREST_DARK_HARD_THEME: BuiltinTheme = BuiltinTheme {
    name: "Everforest Dark Hard",
    metadata_name: "Everforest Dark Hard",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Everforest Dark Hard",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x7a_84_78_ff),
            rgba(0xe6_7e_80_ff),
            rgba(0xa7_c0_80_ff),
            rgba(0xdb_bc_7f_ff),
            rgba(0x7f_bb_b3_ff),
            rgba(0xd6_99_b6_ff),
            rgba(0x83_c0_92_ff),
            rgba(0xf2_ef_df_ff),
            rgba(0xa6_b0_a0_ff),
            rgba(0xf8_55_52_ff),
            rgba(0x8d_a1_01_ff),
            rgba(0xdf_a0_00_ff),
            rgba(0x3a_94_c5_ff),
            rgba(0xdf_69_ba_ff),
            rgba(0x35_a7_7c_ff),
            rgba(0xff_fb_ef_ff),
        ],
        background: Some(rgba(0x1e_23_26_ff)),
        foreground: Some(rgba(0xd3_c6_aa_ff)),
        cursor: Some(rgba(0xe6_98_75_ff)),
        cursor_text: Some(rgba(0x4c_37_43_ff)),
        selection_background: Some(rgba(0x4c_37_43_ff)),
        selection_foreground: Some(rgba(0xd3_c6_aa_ff)),
        ..ColorConfig::default()
    },
};

const EVERFOREST_DARK_MED_THEME: BuiltinTheme = BuiltinTheme {
    name: "Everforest Dark Med",
    metadata_name: "Everforest Dark Med",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Everforest Dark Med",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x7a_84_78_ff),
            rgba(0xe6_7e_80_ff),
            rgba(0xa7_c0_80_ff),
            rgba(0xdb_bc_7f_ff),
            rgba(0x7f_bb_b3_ff),
            rgba(0xd6_99_b6_ff),
            rgba(0x83_c0_92_ff),
            rgba(0xf2_ef_df_ff),
            rgba(0xa6_b0_a0_ff),
            rgba(0xf8_55_52_ff),
            rgba(0x8d_a1_01_ff),
            rgba(0xdf_a0_00_ff),
            rgba(0x3a_94_c5_ff),
            rgba(0xdf_69_ba_ff),
            rgba(0x35_a7_7c_ff),
            rgba(0xff_fb_ef_ff),
        ],
        background: Some(rgba(0x23_2a_2e_ff)),
        foreground: Some(rgba(0xd3_c6_aa_ff)),
        cursor: Some(rgba(0xe6_98_75_ff)),
        cursor_text: Some(rgba(0x54_3a_48_ff)),
        selection_background: Some(rgba(0x54_3a_48_ff)),
        selection_foreground: Some(rgba(0xd3_c6_aa_ff)),
        ..ColorConfig::default()
    },
};

const EVERFOREST_DARK_SOFT_THEME: BuiltinTheme = BuiltinTheme {
    name: "Everforest Dark Soft",
    metadata_name: "Everforest Dark Soft",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Everforest Dark Soft",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x7a_84_78_ff),
            rgba(0xe6_7e_80_ff),
            rgba(0xa7_c0_80_ff),
            rgba(0xdb_bc_7f_ff),
            rgba(0x7f_bb_b3_ff),
            rgba(0xd6_99_b6_ff),
            rgba(0x83_c0_92_ff),
            rgba(0xf2_ef_df_ff),
            rgba(0xa6_b0_a0_ff),
            rgba(0xf8_55_52_ff),
            rgba(0x8d_a1_01_ff),
            rgba(0xdf_a0_00_ff),
            rgba(0x3a_94_c5_ff),
            rgba(0xdf_69_ba_ff),
            rgba(0x35_a7_7c_ff),
            rgba(0xff_fb_ef_ff),
        ],
        background: Some(rgba(0x29_31_36_ff)),
        foreground: Some(rgba(0xd3_c6_aa_ff)),
        cursor: Some(rgba(0xe6_98_75_ff)),
        cursor_text: Some(rgba(0x5c_3f_4f_ff)),
        selection_background: Some(rgba(0x5c_3f_4f_ff)),
        selection_foreground: Some(rgba(0xd3_c6_aa_ff)),
        ..ColorConfig::default()
    },
};

const EVERFOREST_LIGHT_HARD_THEME: BuiltinTheme = BuiltinTheme {
    name: "Everforest Light Hard",
    metadata_name: "Everforest Light Hard",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Everforest Light Hard",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x7a_84_78_ff),
            rgba(0xe6_7e_80_ff),
            rgba(0x9a_b3_73_ff),
            rgba(0xce_af_72_ff),
            rgba(0x7f_bb_b3_ff),
            rgba(0xd6_99_b6_ff),
            rgba(0x83_c0_92_ff),
            rgba(0xb2_af_9f_ff),
            rgba(0xa6_b0_a0_ff),
            rgba(0xf8_55_52_ff),
            rgba(0x8d_a1_01_ff),
            rgba(0xdf_a0_00_ff),
            rgba(0x3a_94_c5_ff),
            rgba(0xdf_69_ba_ff),
            rgba(0x35_a7_7c_ff),
            rgba(0xff_fb_ef_ff),
        ],
        background: Some(rgba(0xf2_ef_df_ff)),
        foreground: Some(rgba(0x5c_6a_72_ff)),
        cursor: Some(rgba(0xf5_7d_26_ff)),
        cursor_text: Some(rgba(0xf0_f2_d4_ff)),
        selection_background: Some(rgba(0xf0_f2_d4_ff)),
        selection_foreground: Some(rgba(0x5c_6a_72_ff)),
        ..ColorConfig::default()
    },
};

const EVERFOREST_LIGHT_MED_THEME: BuiltinTheme = BuiltinTheme {
    name: "Everforest Light Med",
    metadata_name: "Everforest Light Med",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Everforest Light Med",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x7a_84_78_ff),
            rgba(0xe6_7e_80_ff),
            rgba(0x9a_b3_73_ff),
            rgba(0xc1_a2_66_ff),
            rgba(0x7f_bb_b3_ff),
            rgba(0xd6_99_b6_ff),
            rgba(0x83_c0_92_ff),
            rgba(0xb2_af_9f_ff),
            rgba(0xa6_b0_a0_ff),
            rgba(0xf8_55_52_ff),
            rgba(0x8d_a1_01_ff),
            rgba(0xdf_a0_00_ff),
            rgba(0x3a_94_c5_ff),
            rgba(0xdf_69_ba_ff),
            rgba(0x35_a7_7c_ff),
            rgba(0xff_fb_ef_ff),
        ],
        background: Some(rgba(0xef_eb_d4_ff)),
        foreground: Some(rgba(0x5c_6a_72_ff)),
        cursor: Some(rgba(0xf5_7d_26_ff)),
        cursor_text: Some(rgba(0xea_ed_c8_ff)),
        selection_background: Some(rgba(0xea_ed_c8_ff)),
        selection_foreground: Some(rgba(0x5c_6a_72_ff)),
        ..ColorConfig::default()
    },
};

const EVERFOREST_LIGHT_SOFT_THEME: BuiltinTheme = BuiltinTheme {
    name: "Everforest Light Soft",
    metadata_name: "Everforest Light Soft",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Everforest Light Soft",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x7a_84_78_ff),
            rgba(0xe6_7e_80_ff),
            rgba(0x8d_a6_66_ff),
            rgba(0xc1_a2_66_ff),
            rgba(0x72_ae_a6_ff),
            rgba(0xc9_8c_a9_ff),
            rgba(0x76_b3_85_ff),
            rgba(0xa5_a2_92_ff),
            rgba(0x99_a3_93_ff),
            rgba(0xf8_55_52_ff),
            rgba(0x8d_a1_01_ff),
            rgba(0xd2_93_00_ff),
            rgba(0x3a_94_c5_ff),
            rgba(0xdf_69_ba_ff),
            rgba(0x35_a7_7c_ff),
            rgba(0xff_fb_ef_ff),
        ],
        background: Some(rgba(0xe5_df_c5_ff)),
        foreground: Some(rgba(0x5c_6a_72_ff)),
        cursor: Some(rgba(0xf5_7d_26_ff)),
        cursor_text: Some(rgba(0xe1_e4_bd_ff)),
        selection_background: Some(rgba(0xe1_e4_bd_ff)),
        selection_foreground: Some(rgba(0x5c_6a_72_ff)),
        ..ColorConfig::default()
    },
};

const FLEXOKI_DARK_THEME: BuiltinTheme = BuiltinTheme {
    name: "Flexoki Dark",
    metadata_name: "Flexoki Dark",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Flexoki Dark",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x10_0f_0f_ff),
            rgba(0xd1_4d_41_ff),
            rgba(0x87_9a_39_ff),
            rgba(0xd0_a2_15_ff),
            rgba(0x43_85_be_ff),
            rgba(0xce_5d_97_ff),
            rgba(0x3a_a9_9f_ff),
            rgba(0x87_85_80_ff),
            rgba(0x57_56_53_ff),
            rgba(0xaf_30_29_ff),
            rgba(0x66_80_0b_ff),
            rgba(0xad_83_01_ff),
            rgba(0x20_5e_a6_ff),
            rgba(0xa0_2f_6f_ff),
            rgba(0x24_83_7b_ff),
            rgba(0xce_cd_c3_ff),
        ],
        background: Some(rgba(0x10_0f_0f_ff)),
        foreground: Some(rgba(0xce_cd_c3_ff)),
        cursor: Some(rgba(0xce_cd_c3_ff)),
        cursor_text: Some(rgba(0x10_0f_0f_ff)),
        selection_background: Some(rgba(0x40_3e_3c_ff)),
        selection_foreground: Some(rgba(0xce_cd_c3_ff)),
        ..ColorConfig::default()
    },
};

const FLEXOKI_LIGHT_THEME: BuiltinTheme = BuiltinTheme {
    name: "Flexoki Light",
    metadata_name: "Flexoki Light",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Flexoki Light",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x10_0f_0f_ff),
            rgba(0xaf_30_29_ff),
            rgba(0x66_80_0b_ff),
            rgba(0xad_83_01_ff),
            rgba(0x20_5e_a6_ff),
            rgba(0xa0_2f_6f_ff),
            rgba(0x24_83_7b_ff),
            rgba(0x6f_6e_69_ff),
            rgba(0xb7_b5_ac_ff),
            rgba(0xd1_4d_41_ff),
            rgba(0x87_9a_39_ff),
            rgba(0xd0_a2_15_ff),
            rgba(0x43_85_be_ff),
            rgba(0xce_5d_97_ff),
            rgba(0x3a_a9_9f_ff),
            rgba(0xce_cd_c3_ff),
        ],
        background: Some(rgba(0xff_fc_f0_ff)),
        foreground: Some(rgba(0x10_0f_0f_ff)),
        cursor: Some(rgba(0x10_0f_0f_ff)),
        cursor_text: Some(rgba(0xff_fc_f0_ff)),
        selection_background: Some(rgba(0xce_cd_c3_ff)),
        selection_foreground: Some(rgba(0x10_0f_0f_ff)),
        ..ColorConfig::default()
    },
};

const KANAGAWA_DRAGON_THEME: BuiltinTheme = BuiltinTheme {
    name: "Kanagawa Dragon",
    metadata_name: "Kanagawa Dragon",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Kanagawa Dragon",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x0d_0c_0c_ff),
            rgba(0xc4_74_6e_ff),
            rgba(0x8a_9a_7b_ff),
            rgba(0xc4_b2_8a_ff),
            rgba(0x8b_a4_b0_ff),
            rgba(0xa2_92_a3_ff),
            rgba(0x8e_a4_a2_ff),
            rgba(0xc8_c0_93_ff),
            rgba(0xa6_a6_9c_ff),
            rgba(0xe4_68_76_ff),
            rgba(0x87_a9_87_ff),
            rgba(0xe6_c3_84_ff),
            rgba(0x7f_b4_ca_ff),
            rgba(0x93_8a_a9_ff),
            rgba(0x7a_a8_9f_ff),
            rgba(0xc5_c9_c5_ff),
        ],
        background: Some(rgba(0x18_16_16_ff)),
        foreground: Some(rgba(0xc5_c9_c5_ff)),
        cursor: Some(rgba(0xc8_c0_93_ff)),
        cursor_text: Some(rgba(0x18_16_16_ff)),
        selection_background: Some(rgba(0xc5_c9_c5_ff)),
        selection_foreground: Some(rgba(0x18_16_16_ff)),
        ..ColorConfig::default()
    },
};

const KANAGAWA_LOTUS_THEME: BuiltinTheme = BuiltinTheme {
    name: "Kanagawa Lotus",
    metadata_name: "Kanagawa Lotus",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Kanagawa Lotus",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x1f_1f_28_ff),
            rgba(0xc8_40_53_ff),
            rgba(0x6f_89_4e_ff),
            rgba(0x77_71_3f_ff),
            rgba(0x4d_69_9b_ff),
            rgba(0xb3_5b_79_ff),
            rgba(0x59_7b_75_ff),
            rgba(0x54_54_64_ff),
            rgba(0x8a_89_80_ff),
            rgba(0xd7_47_4b_ff),
            rgba(0x6e_91_5f_ff),
            rgba(0x83_6f_4a_ff),
            rgba(0x66_93_bf_ff),
            rgba(0x62_4c_83_ff),
            rgba(0x5e_85_7a_ff),
            rgba(0x43_43_6c_ff),
        ],
        background: Some(rgba(0xf2_ec_bc_ff)),
        foreground: Some(rgba(0x54_54_64_ff)),
        cursor: Some(rgba(0x43_43_6c_ff)),
        cursor_text: Some(rgba(0xf2_ec_bc_ff)),
        selection_background: Some(rgba(0x54_54_64_ff)),
        selection_foreground: Some(rgba(0xf2_ec_bc_ff)),
        ..ColorConfig::default()
    },
};

const KANAGAWA_WAVE_THEME: BuiltinTheme = BuiltinTheme {
    name: "Kanagawa Wave",
    metadata_name: "Kanagawa Wave",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Kanagawa Wave",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x09_06_18_ff),
            rgba(0xc3_40_43_ff),
            rgba(0x76_94_6a_ff),
            rgba(0xc0_a3_6e_ff),
            rgba(0x7e_9c_d8_ff),
            rgba(0x95_7f_b8_ff),
            rgba(0x6a_95_89_ff),
            rgba(0xc8_c0_93_ff),
            rgba(0x72_71_69_ff),
            rgba(0xe8_24_24_ff),
            rgba(0x98_bb_6c_ff),
            rgba(0xe6_c3_84_ff),
            rgba(0x7f_b4_ca_ff),
            rgba(0x93_8a_a9_ff),
            rgba(0x7a_a8_9f_ff),
            rgba(0xdc_d7_ba_ff),
        ],
        background: Some(rgba(0x1f_1f_28_ff)),
        foreground: Some(rgba(0xdc_d7_ba_ff)),
        cursor: Some(rgba(0xdc_d7_ba_ff)),
        cursor_text: Some(rgba(0x1f_1f_28_ff)),
        selection_background: Some(rgba(0xdc_d7_ba_ff)),
        selection_foreground: Some(rgba(0x1f_1f_28_ff)),
        ..ColorConfig::default()
    },
};

const ROSE_PINE_THEME: BuiltinTheme = BuiltinTheme {
    name: "Rose Pine",
    metadata_name: "Rose Pine",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Rose Pine",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x26_23_3a_ff),
            rgba(0xeb_6f_92_ff),
            rgba(0x31_74_8f_ff),
            rgba(0xf6_c1_77_ff),
            rgba(0x9c_cf_d8_ff),
            rgba(0xc4_a7_e7_ff),
            rgba(0xeb_bc_ba_ff),
            rgba(0xe0_de_f4_ff),
            rgba(0x6e_6a_86_ff),
            rgba(0xeb_6f_92_ff),
            rgba(0x31_74_8f_ff),
            rgba(0xf6_c1_77_ff),
            rgba(0x9c_cf_d8_ff),
            rgba(0xc4_a7_e7_ff),
            rgba(0xeb_bc_ba_ff),
            rgba(0xe0_de_f4_ff),
        ],
        background: Some(rgba(0x19_17_24_ff)),
        foreground: Some(rgba(0xe0_de_f4_ff)),
        cursor: Some(rgba(0xe0_de_f4_ff)),
        cursor_text: Some(rgba(0x19_17_24_ff)),
        selection_background: Some(rgba(0x40_3d_52_ff)),
        selection_foreground: Some(rgba(0xe0_de_f4_ff)),
        ..ColorConfig::default()
    },
};

const ROSE_PINE_DAWN_THEME: BuiltinTheme = BuiltinTheme {
    name: "Rose Pine Dawn",
    metadata_name: "Rose Pine Dawn",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Rose Pine Dawn",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0xf2_e9_e1_ff),
            rgba(0xb4_63_7a_ff),
            rgba(0x28_69_83_ff),
            rgba(0xea_9d_34_ff),
            rgba(0x56_94_9f_ff),
            rgba(0x90_7a_a9_ff),
            rgba(0xd7_82_7e_ff),
            rgba(0x57_52_79_ff),
            rgba(0x98_93_a5_ff),
            rgba(0xb4_63_7a_ff),
            rgba(0x28_69_83_ff),
            rgba(0xea_9d_34_ff),
            rgba(0x56_94_9f_ff),
            rgba(0x90_7a_a9_ff),
            rgba(0xd7_82_7e_ff),
            rgba(0x57_52_79_ff),
        ],
        background: Some(rgba(0xfa_f4_ed_ff)),
        foreground: Some(rgba(0x57_52_79_ff)),
        cursor: Some(rgba(0x57_52_79_ff)),
        cursor_text: Some(rgba(0xfa_f4_ed_ff)),
        selection_background: Some(rgba(0xdf_da_d9_ff)),
        selection_foreground: Some(rgba(0x57_52_79_ff)),
        ..ColorConfig::default()
    },
};

const ROSE_PINE_MOON_THEME: BuiltinTheme = BuiltinTheme {
    name: "Rose Pine Moon",
    metadata_name: "Rose Pine Moon",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Rose Pine Moon",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x39_35_52_ff),
            rgba(0xeb_6f_92_ff),
            rgba(0x3e_8f_b0_ff),
            rgba(0xf6_c1_77_ff),
            rgba(0x9c_cf_d8_ff),
            rgba(0xc4_a7_e7_ff),
            rgba(0xea_9a_97_ff),
            rgba(0xe0_de_f4_ff),
            rgba(0x6e_6a_86_ff),
            rgba(0xeb_6f_92_ff),
            rgba(0x3e_8f_b0_ff),
            rgba(0xf6_c1_77_ff),
            rgba(0x9c_cf_d8_ff),
            rgba(0xc4_a7_e7_ff),
            rgba(0xea_9a_97_ff),
            rgba(0xe0_de_f4_ff),
        ],
        background: Some(rgba(0x23_21_36_ff)),
        foreground: Some(rgba(0xe0_de_f4_ff)),
        cursor: Some(rgba(0xe0_de_f4_ff)),
        cursor_text: Some(rgba(0x23_21_36_ff)),
        selection_background: Some(rgba(0x44_41_5a_ff)),
        selection_foreground: Some(rgba(0xe0_de_f4_ff)),
        ..ColorConfig::default()
    },
};

const TOKYONIGHT_NIGHT_THEME: BuiltinTheme = BuiltinTheme {
    name: "TokyoNight Night",
    metadata_name: "TokyoNight Night",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/TokyoNight Night",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x15_16_1e_ff),
            rgba(0xf7_76_8e_ff),
            rgba(0x9e_ce_6a_ff),
            rgba(0xe0_af_68_ff),
            rgba(0x7a_a2_f7_ff),
            rgba(0xbb_9a_f7_ff),
            rgba(0x7d_cf_ff_ff),
            rgba(0xa9_b1_d6_ff),
            rgba(0x41_48_68_ff),
            rgba(0xf7_76_8e_ff),
            rgba(0x9e_ce_6a_ff),
            rgba(0xe0_af_68_ff),
            rgba(0x7a_a2_f7_ff),
            rgba(0xbb_9a_f7_ff),
            rgba(0x7d_cf_ff_ff),
            rgba(0xc0_ca_f5_ff),
        ],
        background: Some(rgba(0x1a_1b_26_ff)),
        foreground: Some(rgba(0xc0_ca_f5_ff)),
        cursor: Some(rgba(0xc0_ca_f5_ff)),
        selection_background: Some(rgba(0x33_46_7c_ff)),
        selection_foreground: Some(rgba(0xc0_ca_f5_ff)),
        ..ColorConfig::default()
    },
};

const TOKYONIGHT_DAY_THEME: BuiltinTheme = BuiltinTheme {
    name: "TokyoNight Day",
    metadata_name: "TokyoNight Day",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/TokyoNight Day",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0xe9_e9_ed_ff),
            rgba(0xf5_2a_65_ff),
            rgba(0x58_75_39_ff),
            rgba(0x8c_6c_3e_ff),
            rgba(0x2e_7d_e9_ff),
            rgba(0x98_54_f1_ff),
            rgba(0x00_71_97_ff),
            rgba(0x61_72_b0_ff),
            rgba(0xa1_a6_c5_ff),
            rgba(0xf5_2a_65_ff),
            rgba(0x58_75_39_ff),
            rgba(0x8c_6c_3e_ff),
            rgba(0x2e_7d_e9_ff),
            rgba(0x98_54_f1_ff),
            rgba(0x00_71_97_ff),
            rgba(0x37_60_bf_ff),
        ],
        background: Some(rgba(0xe1_e2_e7_ff)),
        foreground: Some(rgba(0x37_60_bf_ff)),
        cursor: Some(rgba(0x37_60_bf_ff)),
        cursor_text: Some(rgba(0xe1_e2_e7_ff)),
        selection_background: Some(rgba(0x99_a7_df_ff)),
        selection_foreground: Some(rgba(0x37_60_bf_ff)),
        ..ColorConfig::default()
    },
};

const TOKYONIGHT_MOON_THEME: BuiltinTheme = BuiltinTheme {
    name: "TokyoNight Moon",
    metadata_name: "TokyoNight Moon",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/TokyoNight Moon",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x1b_1d_2b_ff),
            rgba(0xff_75_7f_ff),
            rgba(0xc3_e8_8d_ff),
            rgba(0xff_c7_77_ff),
            rgba(0x82_aa_ff_ff),
            rgba(0xc0_99_ff_ff),
            rgba(0x86_e1_fc_ff),
            rgba(0x82_8b_b8_ff),
            rgba(0x44_4a_73_ff),
            rgba(0xff_75_7f_ff),
            rgba(0xc3_e8_8d_ff),
            rgba(0xff_c7_77_ff),
            rgba(0x82_aa_ff_ff),
            rgba(0xc0_99_ff_ff),
            rgba(0x86_e1_fc_ff),
            rgba(0xc8_d3_f5_ff),
        ],
        background: Some(rgba(0x22_24_36_ff)),
        foreground: Some(rgba(0xc8_d3_f5_ff)),
        cursor: Some(rgba(0xc8_d3_f5_ff)),
        cursor_text: Some(rgba(0x22_24_36_ff)),
        selection_background: Some(rgba(0x2d_3f_76_ff)),
        selection_foreground: Some(rgba(0xc8_d3_f5_ff)),
        ..ColorConfig::default()
    },
};

const TOKYONIGHT_STORM_THEME: BuiltinTheme = BuiltinTheme {
    name: "TokyoNight Storm",
    metadata_name: "TokyoNight Storm",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/TokyoNight Storm",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x1d_20_2f_ff),
            rgba(0xf7_76_8e_ff),
            rgba(0x9e_ce_6a_ff),
            rgba(0xe0_af_68_ff),
            rgba(0x7a_a2_f7_ff),
            rgba(0xbb_9a_f7_ff),
            rgba(0x7d_cf_ff_ff),
            rgba(0xa9_b1_d6_ff),
            rgba(0x4e_55_75_ff),
            rgba(0xf7_76_8e_ff),
            rgba(0x9e_ce_6a_ff),
            rgba(0xe0_af_68_ff),
            rgba(0x7a_a2_f7_ff),
            rgba(0xbb_9a_f7_ff),
            rgba(0x7d_cf_ff_ff),
            rgba(0xc0_ca_f5_ff),
        ],
        background: Some(rgba(0x24_28_3b_ff)),
        foreground: Some(rgba(0xc0_ca_f5_ff)),
        cursor: Some(rgba(0xc0_ca_f5_ff)),
        cursor_text: Some(rgba(0x1d_20_2f_ff)),
        selection_background: Some(rgba(0x36_4a_82_ff)),
        selection_foreground: Some(rgba(0xc0_ca_f5_ff)),
        ..ColorConfig::default()
    },
};

const ITERM2_SOLARIZED_DARK_THEME: BuiltinTheme = BuiltinTheme {
    name: "Solarized Dark",
    metadata_name: "iTerm2 Solarized Dark",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/iTerm2 Solarized Dark",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x07_36_42_ff),
            rgba(0xdc_32_2f_ff),
            rgba(0x85_99_00_ff),
            rgba(0xb5_89_00_ff),
            rgba(0x26_8b_d2_ff),
            rgba(0xd3_36_82_ff),
            rgba(0x2a_a1_98_ff),
            rgba(0xee_e8_d5_ff),
            rgba(0x33_5e_69_ff),
            rgba(0xcb_4b_16_ff),
            rgba(0x58_6e_75_ff),
            rgba(0x65_7b_83_ff),
            rgba(0x83_94_96_ff),
            rgba(0x6c_71_c4_ff),
            rgba(0x93_a1_a1_ff),
            rgba(0xfd_f6_e3_ff),
        ],
        background: Some(rgba(0x00_2b_36_ff)),
        foreground: Some(rgba(0x83_94_96_ff)),
        cursor: Some(rgba(0x83_94_96_ff)),
        cursor_text: Some(rgba(0x07_36_42_ff)),
        selection_background: Some(rgba(0x07_36_42_ff)),
        selection_foreground: Some(rgba(0x93_a1_a1_ff)),
        ..ColorConfig::default()
    },
};

const ITERM2_SOLARIZED_LIGHT_THEME: BuiltinTheme = BuiltinTheme {
    name: "Solarized Light",
    metadata_name: "iTerm2 Solarized Light",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/iTerm2 Solarized Light",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x07_36_42_ff),
            rgba(0xdc_32_2f_ff),
            rgba(0x85_99_00_ff),
            rgba(0xb5_89_00_ff),
            rgba(0x26_8b_d2_ff),
            rgba(0xd3_36_82_ff),
            rgba(0x2a_a1_98_ff),
            rgba(0xbb_b5_a2_ff),
            rgba(0x00_2b_36_ff),
            rgba(0xcb_4b_16_ff),
            rgba(0x58_6e_75_ff),
            rgba(0x65_7b_83_ff),
            rgba(0x83_94_96_ff),
            rgba(0x6c_71_c4_ff),
            rgba(0x93_a1_a1_ff),
            rgba(0xfd_f6_e3_ff),
        ],
        background: Some(rgba(0xfd_f6_e3_ff)),
        foreground: Some(rgba(0x65_7b_83_ff)),
        cursor: Some(rgba(0x65_7b_83_ff)),
        cursor_text: Some(rgba(0xee_e8_d5_ff)),
        selection_background: Some(rgba(0xee_e8_d5_ff)),
        selection_foreground: Some(rgba(0x58_6e_75_ff)),
        ..ColorConfig::default()
    },
};

const XCODE_DARK_THEME: BuiltinTheme = BuiltinTheme {
    name: "Xcode Dark",
    metadata_name: "Xcode Dark",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Xcode Dark",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x41_44_53_ff),
            rgba(0xff_81_70_ff),
            rgba(0x78_c2_b3_ff),
            rgba(0xd9_c9_7c_ff),
            rgba(0x4e_b0_cc_ff),
            rgba(0xff_7a_b2_ff),
            rgba(0xb2_81_eb_ff),
            rgba(0xdf_df_e0_ff),
            rgba(0x7f_8c_98_ff),
            rgba(0xff_81_70_ff),
            rgba(0xac_f2_e4_ff),
            rgba(0xff_a1_4f_ff),
            rgba(0x6b_df_ff_ff),
            rgba(0xff_7a_b2_ff),
            rgba(0xda_ba_ff_ff),
            rgba(0xdf_df_e0_ff),
        ],
        background: Some(rgba(0x29_2a_30_ff)),
        foreground: Some(rgba(0xdf_df_e0_ff)),
        cursor: Some(rgba(0xdf_df_e0_ff)),
        cursor_text: Some(rgba(0x29_2a_30_ff)),
        selection_background: Some(rgba(0x41_44_53_ff)),
        selection_foreground: Some(rgba(0xdf_df_e0_ff)),
        ..ColorConfig::default()
    },
};

const XCODE_LIGHT_THEME: BuiltinTheme = BuiltinTheme {
    name: "Xcode Light",
    metadata_name: "Xcode Light",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Xcode Light",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0xb4_d8_fd_ff),
            rgba(0xd1_2f_1b_ff),
            rgba(0x3e_80_87_ff),
            rgba(0x78_49_2a_ff),
            rgba(0x0f_68_a0_ff),
            rgba(0xad_3d_a4_ff),
            rgba(0x80_4f_b8_ff),
            rgba(0x26_26_26_ff),
            rgba(0x8a_99_a6_ff),
            rgba(0xd1_2f_1b_ff),
            rgba(0x23_57_5c_ff),
            rgba(0x78_49_2a_ff),
            rgba(0x0b_4f_79_ff),
            rgba(0xad_3d_a4_ff),
            rgba(0x4b_21_b0_ff),
            rgba(0x26_26_26_ff),
        ],
        background: Some(rgba(0xff_ff_ff_ff)),
        foreground: Some(rgba(0x26_26_26_ff)),
        cursor: Some(rgba(0x26_26_26_ff)),
        cursor_text: Some(rgba(0xff_ff_ff_ff)),
        selection_background: Some(rgba(0xb4_d8_fd_ff)),
        selection_foreground: Some(rgba(0x26_26_26_ff)),
        ..ColorConfig::default()
    },
};

const GRUVBOX_DARK_THEME: BuiltinTheme = BuiltinTheme {
    name: "Gruvbox Dark",
    metadata_name: "Gruvbox Dark",
    source: "mbadolato/iTerm2-Color-Schemes ghostty/Gruvbox Dark",
    license: "MIT collection; individual theme provenance applies",
    colors: || ColorConfig {
        palette: vec![
            rgba(0x28_28_28_ff),
            rgba(0xcc_24_1d_ff),
            rgba(0x98_97_1a_ff),
            rgba(0xd7_99_21_ff),
            rgba(0x45_85_88_ff),
            rgba(0xb1_62_86_ff),
            rgba(0x68_9d_6a_ff),
            rgba(0xa8_99_84_ff),
            rgba(0x92_83_74_ff),
            rgba(0xfb_49_34_ff),
            rgba(0xb8_bb_26_ff),
            rgba(0xfa_bd_2f_ff),
            rgba(0x83_a5_98_ff),
            rgba(0xd3_86_9b_ff),
            rgba(0x8e_c0_7c_ff),
            rgba(0xeb_db_b2_ff),
        ],
        background: Some(rgba(0x28_28_28_ff)),
        foreground: Some(rgba(0xeb_db_b2_ff)),
        cursor: Some(rgba(0xeb_db_b2_ff)),
        selection_background: Some(rgba(0x50_49_45_ff)),
        selection_foreground: Some(rgba(0xeb_db_b2_ff)),
        ..ColorConfig::default()
    },
};
