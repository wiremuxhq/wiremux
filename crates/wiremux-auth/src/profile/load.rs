//! Catalog keyed by document `id`. Files with different ids never merge.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::error::ProfileError;
use super::parse::{parse_profile_file, parse_profile_str};
use super::shipped;
use super::types::{LoadOptions, ResolvedProfile};

/// List document ids from shipped ∪ extra dirs ∪ explicit file.
pub fn list_profiles(opts: &LoadOptions<'_>) -> Result<Vec<String>, ProfileError> {
    let mut ids = BTreeSet::new();
    for layer in collect_layers(opts)? {
        ids.insert(layer.id);
    }
    Ok(ids.into_iter().collect())
}

/// Load the profile whose document `id` equals `id`.
///
/// Layers with a different document id are ignored. Same-id field-wise overlay
/// is a later PR; when several files share an id, the last layer wins whole.
pub fn load_profile(id: &str, opts: &LoadOptions<'_>) -> Result<ResolvedProfile, ProfileError> {
    let mut found = None;
    for layer in collect_layers(opts)? {
        if layer.id == id {
            found = Some(layer.profile);
        }
    }
    found.ok_or_else(|| ProfileError::NotFound(id.to_string()))
}

/// `--profile` / path-vs-id disambiguation.
///
/// A value that contains `/` or `\`, ends in `.toml` / `.json`, or is an
/// existing path is a file. Otherwise it is a catalog id.
pub fn load_profile_from_cli(
    profile_arg: &str,
    opts: &LoadOptions<'_>,
) -> Result<ResolvedProfile, ProfileError> {
    if looks_like_path(profile_arg) {
        parse_profile_file(Path::new(profile_arg))
    } else {
        load_profile(profile_arg, opts)
    }
}

fn looks_like_path(s: &str) -> bool {
    s.contains('/')
        || s.contains('\\')
        || s.ends_with(".toml")
        || s.ends_with(".json")
        || Path::new(s).exists()
}

struct Layer {
    id: String,
    profile: ResolvedProfile,
}

fn collect_layers(opts: &LoadOptions<'_>) -> Result<Vec<Layer>, ProfileError> {
    let mut layers = Vec::new();
    if opts.include_shipped {
        for text in shipped::documents() {
            let profile = parse_profile_str(text)?;
            layers.push(Layer {
                id: profile.id.clone(),
                profile,
            });
        }
    }
    for dir in &opts.extra_profile_dirs {
        for path in list_profile_files(dir)? {
            push_layer(&mut layers, &path)?;
        }
    }
    if let Some(path) = opts.explicit_file {
        push_layer(&mut layers, path)?;
    }
    Ok(layers)
}

fn push_layer(layers: &mut Vec<Layer>, path: &Path) -> Result<(), ProfileError> {
    let profile = parse_profile_file(path)?;
    layers.push(Layer {
        id: profile.id.clone(),
        profile,
    });
    Ok(())
}

fn list_profile_files(dir: &Path) -> Result<Vec<PathBuf>, ProfileError> {
    let mut files = Vec::new();
    let entries = fs::read_dir(dir).map_err(|source| ProfileError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| ProfileError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        match path.extension().and_then(|e| e.to_str()) {
            Some("toml") | Some("json") => files.push(path),
            _ => {}
        }
    }
    files.sort();
    Ok(files)
}
