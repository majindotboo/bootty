use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use thiserror::Error;

static PROCESS_IDENTITY: OnceLock<ApplicationIdentity> = OnceLock::new();
static DEVELOPMENT_NAMES: OnceLock<ApplicationNames> = OnceLock::new();

pub const APPLICATION_IDENTITY_ENV: &str = "BOOTTY_APPLICATION_IDENTITY";
pub const DEVELOPMENT_NAMESPACE_ENV: &str = "BOOTTY_DEVELOPMENT_NAMESPACE";

const PRODUCTION_NAMESPACE: &str = "bootty";
const DEVELOPMENT_NAMESPACE_PREFIX: &str = "bootty-dev-";
const PRODUCTION_BUNDLE_IDENTIFIER: &str = "dev.bootty.desktop";
const DEVELOPMENT_BUNDLE_IDENTIFIER_PREFIX: &str = "dev.bootty.desktop.dev.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationNames {
    display_name: String,
    namespace: String,
    bundle_identifier: String,
}

impl ApplicationNames {
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    #[must_use]
    pub fn cli_name(&self) -> &str {
        self.namespace()
    }

    #[must_use]
    pub fn bundle_identifier(&self) -> &str {
        &self.bundle_identifier
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplicationIdentity {
    Production,
    Development,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error(
    "application identity is already initialized as {active:?}; cannot change it to {requested:?}"
)]
pub struct ApplicationIdentityConflict {
    active: ApplicationIdentity,
    requested: ApplicationIdentity,
}

impl ApplicationIdentity {
    pub const fn current() -> Self {
        if cfg!(any(debug_assertions, feature = "bootty-dev")) {
            Self::Development
        } else {
            Self::Production
        }
    }

    pub fn for_process() -> Self {
        PROCESS_IDENTITY.get().copied().unwrap_or(Self::Production)
    }

    pub fn initialize_process(self) -> Result<(), ApplicationIdentityConflict> {
        if PROCESS_IDENTITY.set(self).is_err() && Self::for_process() != self {
            return Err(ApplicationIdentityConflict {
                active: Self::for_process(),
                requested: self,
            });
        }
        Ok(())
    }

    pub fn display_name(self) -> &'static str {
        self.names().display_name()
    }

    pub fn namespace(self) -> &'static str {
        self.names().namespace()
    }

    pub fn cli_name(self) -> &'static str {
        self.namespace()
    }

    pub fn bundle_identifier(self) -> &'static str {
        self.names().bundle_identifier()
    }

    pub fn names_for_workspace(self, workspace_root: &Path) -> ApplicationNames {
        match self {
            Self::Production => production_names(),
            Self::Development => development_names_for_workspace(workspace_root),
        }
    }

    pub fn development_namespace_environment(self) -> Option<(&'static str, &'static str)> {
        matches!(self, Self::Development).then(|| (DEVELOPMENT_NAMESPACE_ENV, self.namespace()))
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "bootty" => Some(Self::Production),
            "bootty-dev" => Some(Self::Development),
            _ => None,
        }
    }

    pub fn default_config_path(self) -> PathBuf {
        config_path_from_env(
            self,
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
    }

    pub const fn automatic_updates_enabled(self) -> bool {
        matches!(self, Self::Production)
    }

    fn names(self) -> &'static ApplicationNames {
        static PRODUCTION_NAMES: OnceLock<ApplicationNames> = OnceLock::new();
        match self {
            Self::Production => PRODUCTION_NAMES.get_or_init(production_names),
            Self::Development => DEVELOPMENT_NAMES.get_or_init(|| {
                development_names_from_namespace(
                    std::env::var_os(DEVELOPMENT_NAMESPACE_ENV)
                        .as_deref()
                        .and_then(valid_development_namespace),
                )
            }),
        }
    }
}

#[must_use]
pub fn development_names_for_workspace(workspace_root: &Path) -> ApplicationNames {
    let root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let discriminator = stable_path_discriminator(&root);
    names_from_discriminator(&discriminator)
}

#[must_use]
pub fn development_namespace_for_workspace(workspace_root: &Path) -> String {
    development_names_for_workspace(workspace_root).namespace
}

fn production_names() -> ApplicationNames {
    ApplicationNames {
        display_name: "Bootty".to_owned(),
        namespace: PRODUCTION_NAMESPACE.to_owned(),
        bundle_identifier: PRODUCTION_BUNDLE_IDENTIFIER.to_owned(),
    }
}

fn development_names_from_namespace(inherited: Option<&str>) -> ApplicationNames {
    inherited.map_or_else(
        || development_names_for_workspace(compiled_workspace_root()),
        |namespace| {
            let discriminator = namespace
                .strip_prefix(DEVELOPMENT_NAMESPACE_PREFIX)
                .expect("validated development namespace has its prefix");
            names_from_discriminator(discriminator)
        },
    )
}

fn names_from_discriminator(discriminator: &str) -> ApplicationNames {
    ApplicationNames {
        display_name: format!("BoottyDev-{discriminator}"),
        namespace: format!("{DEVELOPMENT_NAMESPACE_PREFIX}{discriminator}"),
        bundle_identifier: format!("{DEVELOPMENT_BUNDLE_IDENTIFIER_PREFIX}{discriminator}"),
    }
}

fn valid_development_namespace(value: &OsStr) -> Option<&str> {
    let value = value.to_str()?;
    let discriminator = value.strip_prefix(DEVELOPMENT_NAMESPACE_PREFIX)?;
    (discriminator.len() == 16
        && discriminator
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(value)
}

fn compiled_workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("bootty-identity lives under <workspace>/crates")
}

fn stable_path_discriminator(path: &Path) -> String {
    // FNV-1a is deliberately implemented here: its output is stable across Rust releases, unlike
    // `DefaultHasher`, and this identifier becomes part of filesystem and IPC names.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in path.to_string_lossy().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

pub fn config_path_from_env(
    identity: ApplicationIdentity,
    xdg_config_home: Option<impl AsRef<Path>>,
    home: Option<impl AsRef<Path>>,
) -> PathBuf {
    if let Some(xdg) = xdg_config_home {
        return xdg.as_ref().join(identity.namespace()).join("config.toml");
    }
    if let Some(home) = home {
        return home
            .as_ref()
            .join(".config")
            .join(identity.namespace())
            .join("config.toml");
    }
    PathBuf::from(identity.namespace()).join("config.toml")
}

pub fn legacy_config_path_from_env(
    identity: ApplicationIdentity,
    xdg_config_home: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    xdg_config_home
        .map(Path::to_path_buf)
        .or_else(|| home.map(|home| home.join(".config")))
        .map(|root| root.join(identity.namespace()).join("config.toml"))
}

pub fn unix_daemon_state_path(
    identity: ApplicationIdentity,
    explicit: Option<&Path>,
    xdg_state_home: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(explicit) = explicit {
        return Some(explicit.to_path_buf());
    }
    xdg_state_home
        .map(Path::to_path_buf)
        .or_else(|| home.map(|home| home.join(".local/state")))
        .map(|root| root.join(identity.namespace()).join("daemon.sqlite"))
}

pub fn windows_daemon_state_path(
    identity: ApplicationIdentity,
    explicit: Option<&Path>,
    local_app_data: Option<&Path>,
    app_data: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(explicit) = explicit {
        return Some(explicit.to_path_buf());
    }
    local_app_data
        .or(app_data)
        .map(|root| root.join(identity.namespace()).join("daemon.sqlite"))
}
