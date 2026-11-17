//! The shim harness: R8-supported subsets of the Test262 harness files,
//! loaded from `harness/` next to this crate. Tests whose `includes` are
//! not in the shim set are statically unsupported.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Shim {
    files: HashMap<String, PathBuf>,
}

impl Shim {
    pub fn load() -> Result<Self, String> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("harness");
        let mut files = HashMap::new();
        let entries = std::fs::read_dir(&dir).map_err(|error| format!("read shim dir: {error}"))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("shim entry: {error}"))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("js")
                && let Some(name) = path.file_name().and_then(|n| n.to_str())
            {
                files.insert(name.to_string(), path);
            }
        }
        Ok(Self { files })
    }

    pub fn provides(&self, name: &str) -> Option<&PathBuf> {
        self.files.get(name)
    }
}
