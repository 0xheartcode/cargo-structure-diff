//! Typed `.csd.toml` configuration.
//!
//! Parses the config that drives the views, lints, and CI behaviour (SPEC.md section 7). It is
//! pure apart from [`Config::load`], which reads a file. Unknown keys are rejected so a typo fails
//! loudly instead of being silently ignored, and every layer glob is compiled at parse time so a
//! bad pattern is caught here rather than deep in the linter.

use std::path::Path;

use serde::Deserialize;

/// Parse or load errors, all with enough context to fix the config by hand.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("failed to read {path}: {source}")]
    Read {
        /// The path we tried to read.
        path: String,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// The TOML did not parse or had unknown/invalid keys.
    #[error("invalid .csd.toml: {0}")]
    Toml(#[from] toml::de::Error),
    /// A layer glob was not a valid pattern.
    #[error("invalid glob {glob:?} for layer {layer:?}: {source}")]
    Glob {
        /// The offending glob string.
        glob: String,
        /// The layer it was mapped to.
        layer: String,
        /// The glob compiler error.
        source: glob::PatternError,
    },
    /// A layer referenced in the map or a forbid rule is not in `layers.order`.
    #[error("layer {0:?} is used but not declared in layers.order")]
    UnknownLayer(String),
}

/// The whole configuration. Every section is optional so a minimal config is valid.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Architectural strata and the forbidden edges between them.
    #[serde(default)]
    pub layers: Layers,
    /// Which views are enabled.
    #[serde(default)]
    pub views: Views,
    /// Lint deny/warn selection.
    #[serde(default)]
    pub lint: Lint,
    /// CI failure behaviour.
    #[serde(default)]
    pub ratchet: Ratchet,
}

/// Declared strata, the module-to-layer mapping, and forbidden inter-layer edges.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layers {
    /// Strata top (may depend downward) to bottom.
    #[serde(default)]
    pub order: Vec<String>,
    /// Module path glob to layer, first match wins.
    #[serde(default)]
    pub map: Vec<LayerMap>,
    /// Edges that must not exist, expressed layer to layer.
    #[serde(default)]
    pub forbid: Vec<Forbid>,
}

/// One `{ layer, glob }` mapping entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerMap {
    /// The layer a matching module belongs to.
    pub layer: String,
    /// A path glob matched against a module's source path.
    pub glob: String,
}

/// One forbidden `from -> to` layer edge.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Forbid {
    /// The depending layer.
    pub from: String,
    /// The depended-on layer that is not allowed.
    pub to: String,
}

/// Enabled views.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Views {
    /// View names, for example `modules`, `states`.
    #[serde(default)]
    pub enabled: Vec<String>,
}

/// Lint selection: `deny` fails the build, `warn` only reports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lint {
    /// Lints that fail the build.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Lints that only warn.
    #[serde(default)]
    pub warn: Vec<String>,
}

/// How the CI gate treats pre-existing violations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RatchetMode {
    /// Fail only on violations new since the base ref (the brownfield default).
    NewOnly,
    /// Fail on any violation, pre-existing or new.
    All,
}

/// Ratchet section. Defaults to [`RatchetMode::NewOnly`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ratchet {
    /// The failure mode.
    #[serde(default = "default_ratchet_mode")]
    pub mode: RatchetMode,
}

fn default_ratchet_mode() -> RatchetMode {
    RatchetMode::NewOnly
}

impl Default for Ratchet {
    fn default() -> Self {
        Self {
            mode: RatchetMode::NewOnly,
        }
    }
}

impl Config {
    /// Parse config from a TOML string, then validate it.
    pub fn parse(toml_src: &str) -> Result<Self, ConfigError> {
        let config: Config = toml::from_str(toml_src)?;
        config.validate()?;
        Ok(config)
    }

    /// Read and parse config from a file path.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let src = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&src)
    }

    /// Check that every glob compiles and every referenced layer is declared.
    fn validate(&self) -> Result<(), ConfigError> {
        for entry in &self.layers.map {
            self.require_layer(&entry.layer)?;
            glob::Pattern::new(&entry.glob).map_err(|source| ConfigError::Glob {
                glob: entry.glob.clone(),
                layer: entry.layer.clone(),
                source,
            })?;
        }
        for rule in &self.layers.forbid {
            self.require_layer(&rule.from)?;
            self.require_layer(&rule.to)?;
        }
        Ok(())
    }

    fn require_layer(&self, layer: &str) -> Result<(), ConfigError> {
        if self.layers.order.iter().any(|l| l == layer) {
            Ok(())
        } else {
            Err(ConfigError::UnknownLayer(layer.to_string()))
        }
    }

    /// The layer a module source path belongs to, first matching glob wins, or `None`.
    pub fn layer_of(&self, module_path: &str) -> Option<&str> {
        self.layers.map.iter().find_map(|entry| {
            glob::Pattern::new(&entry.glob)
                .ok()
                .filter(|p| p.matches(module_path))
                .map(|_| entry.layer.as_str())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
[layers]
order = ["api", "domain", "ports", "infra"]
map = [
    { layer = "api",    glob = "crates/*/src/api/**" },
    { layer = "domain", glob = "crates/*/src/domain/**" },
    { layer = "infra",  glob = "crates/*/src/infra/**" },
]
forbid = [{ from = "domain", to = "infra" }]

[views]
enabled = ["modules", "states"]

[lint]
deny = ["layering", "cycles"]
warn = ["fan_in"]

[ratchet]
mode = "new-only"
"#;

    #[test]
    fn parses_the_example() {
        let c = Config::parse(EXAMPLE).unwrap();
        assert_eq!(c.layers.order, ["api", "domain", "ports", "infra"]);
        assert_eq!(c.layers.map.len(), 3);
        assert_eq!(
            c.layers.forbid[0],
            Forbid {
                from: "domain".into(),
                to: "infra".into()
            }
        );
        assert_eq!(c.views.enabled, ["modules", "states"]);
        assert_eq!(c.lint.deny, ["layering", "cycles"]);
        assert_eq!(c.ratchet.mode, RatchetMode::NewOnly);
    }

    #[test]
    fn empty_config_is_valid_with_defaults() {
        let c = Config::parse("").unwrap();
        assert!(c.layers.order.is_empty());
        assert_eq!(c.ratchet.mode, RatchetMode::NewOnly);
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = Config::parse("[layers]\norder = []\nbogus = 1\n").unwrap_err();
        assert!(matches!(err, ConfigError::Toml(_)), "got {err:?}");
    }

    #[test]
    fn undeclared_layer_in_map_is_rejected() {
        let src = "[layers]\norder = [\"api\"]\nmap = [{ layer = \"ghost\", glob = \"**\" }]\n";
        let err = Config::parse(src).unwrap_err();
        assert!(
            matches!(err, ConfigError::UnknownLayer(ref l) if l == "ghost"),
            "got {err:?}"
        );
    }

    #[test]
    fn bad_glob_is_rejected() {
        let src = "[layers]\norder = [\"api\"]\nmap = [{ layer = \"api\", glob = \"a/***/b\" }]\n";
        let err = Config::parse(src).unwrap_err();
        assert!(matches!(err, ConfigError::Glob { .. }), "got {err:?}");
    }

    #[test]
    fn layer_of_matches_first_glob() {
        let c = Config::parse(EXAMPLE).unwrap();
        assert_eq!(c.layer_of("crates/app/src/api/routes.rs"), Some("api"));
        assert_eq!(c.layer_of("crates/app/src/infra/db.rs"), Some("infra"));
        assert_eq!(c.layer_of("crates/app/src/main.rs"), None);
    }
}
