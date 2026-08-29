//! TOML configuration for the AT-URI dictionaries.
//!
//! Example `config.toml`:
//! ```toml
//! # Extra collection NSIDs, appended after the built-ins (index order matters
//! # for decoding old cards, so only ever append).
//! collections = ["app.rocksky.scrobble", "com.example.thing"]
//!
//! # Authorities (DIDs / handles) to encode as a 1-byte index — the most
//! # compact form, ideal for a DID you store repeatedly.
//! authorities = ["did:plc:7vdlgi2bflelz7mmuxoqjfcr"]
//! ```
//!
//! Load order: `--config <path>` if given, else `$HOME/.config/sledge/config.toml`
//! if it exists, else built-in defaults only.

use crate::aturi::Dict;
use serde::Deserialize;
use std::error::Error;
use std::path::PathBuf;

/// Built-in collections, always present at the start of the dictionary so their
/// indices are stable regardless of user config.
const DEFAULT_COLLECTIONS: &[&str] = &[
    "app.rocksky.playlist",
    "app.rocksky.album",
    "app.rocksky.artist",
];

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    collections: Vec<String>,
    #[serde(default)]
    authorities: Vec<String>,
}

/// Build the dictionary: built-in collections first, then any user-configured
/// collections (appended, de-duplicated), plus user-configured authorities.
pub fn load(explicit: Option<&str>) -> Result<Dict, Box<dyn Error>> {
    let path = match explicit {
        Some(p) => Some(PathBuf::from(p)),
        None => default_path().filter(|p| p.exists()),
    };

    let file = match &path {
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .map_err(|e| format!("reading config {}: {e}", p.display()))?;
            toml::from_str::<FileConfig>(&text)
                .map_err(|e| format!("parsing config {}: {e}", p.display()))?
        }
        None => FileConfig::default(),
    };

    let mut collections: Vec<String> = DEFAULT_COLLECTIONS.iter().map(|s| s.to_string()).collect();
    for c in file.collections {
        if !collections.contains(&c) {
            collections.push(c);
        }
    }

    Ok(Dict {
        collections,
        authorities: file.authorities,
    })
}

fn default_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/sledge/config.toml"))
}
