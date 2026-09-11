//! Catalog keyed by document `id`. Files with different ids never merge.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use super::error::ProfileError;
use super::overlay;
use super::parse::{RawProfile, parse_layer_file, parse_layer_str, resolve};
use super::shipped;
use super::types::{LoadOptions, ResolvedProfile};

/// List document ids from shipped ∪ user dir ∪ explicit file.
pub fn list_profiles(opts: &LoadOptions<'_>) -> Result<Vec<String>, ProfileError> {
    let mut ids = BTreeSet::new();
    for layer in collect_layers(opts)? {
        ids.insert(layer.id);
    }
    Ok(ids.into_iter().collect())
}

/// Load the profile whose document `id` equals `id`.
///
/// Layers: shipped (unless skipped) < user-dir files (path-sorted) < explicit
/// file. Only layers whose catalog key equals `id` merge, field-wise.
pub fn load_profile(id: &str, opts: &LoadOptions<'_>) -> Result<ResolvedProfile, ProfileError> {
    let layers = collect_layers(opts)?;
    let mut known = BTreeSet::new();
    let mut acc: Option<RawProfile> = None;
    for layer in layers {
        known.insert(layer.id.clone());
        if layer.id != id {
            continue;
        }
        acc = Some(match acc {
            None => layer.profile,
            Some(earlier) => overlay::merge(earlier, layer.profile),
        });
    }
    match acc {
        Some(raw) => resolve(raw),
        None => Err(ProfileError::NotFound {
            id: id.to_string(),
            known: known.into_iter().collect(),
        }),
    }
}

/// `--profile` / path-vs-id disambiguation.
///
/// A value that contains `/` or `\`, ends in `.toml` / `.json`, or is an
/// existing path is a file. Its document `id` is the load key; the file is
/// overlaid on shipped + user-dir layers for that same id.
pub fn load_profile_from_cli(
    profile_arg: &str,
    opts: &LoadOptions<'_>,
) -> Result<ResolvedProfile, ProfileError> {
    if looks_like_path(profile_arg) {
        let path = Path::new(profile_arg);
        let layer = parse_layer_file(path)?;
        let id = match layer.id.as_deref() {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return Err(ProfileError::MissingField("id")),
        };
        let opts = LoadOptions {
            id: Some(id.as_str()),
            explicit_file: Some(path),
            extra_profile_dirs: opts.extra_profile_dirs.clone(),
            include_shipped: opts.include_shipped,
            include_user_config: opts.include_user_config,
        };
        load_profile(&id, &opts)
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
    profile: RawProfile,
}

fn collect_layers(opts: &LoadOptions<'_>) -> Result<Vec<Layer>, ProfileError> {
    let mut layers = Vec::new();
    if include_shipped(opts) {
        for text in shipped::documents() {
            let profile = parse_layer_str(text)?;
            if let Some(id) = catalog_id(&profile, None) {
                layers.push(Layer { id, profile });
            }
        }
    }
    for path in user_layer_files(opts)? {
        push_layer(&mut layers, &path)?;
    }
    if let Some(path) = opts.explicit_file {
        push_layer(&mut layers, path)?;
    }
    Ok(layers)
}

fn include_shipped(opts: &LoadOptions<'_>) -> bool {
    opts.include_shipped && !env_flag_true("WIREMUX_NO_SHIPPED_PRESETS")
}

fn env_flag_true(name: &str) -> bool {
    env::var(name)
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

fn user_layer_files(opts: &LoadOptions<'_>) -> Result<Vec<PathBuf>, ProfileError> {
    let mut files = Vec::new();
    if opts.include_user_config {
        for dir in discover_user_profile_dirs() {
            if dir.is_dir() {
                files.extend(list_profile_files(&dir)?);
            }
        }
    }
    for dir in &opts.extra_profile_dirs {
        files.extend(list_profile_files(dir)?);
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn discover_user_profile_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            dirs.push(PathBuf::from(xdg).join("wiremux").join("profiles"));
        }
    } else if let Some(home) = home_dir() {
        dirs.push(home.join(".config").join("wiremux").join("profiles"));
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = home_dir() {
        dirs.push(
            home.join("Library")
                .join("Application Support")
                .join("wiremux")
                .join("profiles"),
        );
    }
    if let Ok(extra) = env::var("WIREMUX_PROFILE_DIR") {
        for part in env::split_paths(&extra) {
            if !part.as_os_str().is_empty() {
                dirs.push(part);
            }
        }
    }
    dirs
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn push_layer(layers: &mut Vec<Layer>, path: &Path) -> Result<(), ProfileError> {
    let profile = parse_layer_file(path)?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let Some(id) = catalog_id(&profile, Some(stem)) else {
        return Ok(());
    };
    if let Some(doc_id) = profile.id.as_deref()
        && !doc_id.is_empty()
        && !stem.is_empty()
        && doc_id != stem
    {
        tracing::warn!(
            path = %path.display(),
            stem,
            id = doc_id,
            "profile filename stem differs from document id; using document id"
        );
    }
    layers.push(Layer { id, profile });
    Ok(())
}

fn catalog_id(profile: &RawProfile, stem: Option<&str>) -> Option<String> {
    match profile.id.as_deref() {
        Some(id) if !id.is_empty() => Some(id.to_string()),
        _ => stem
            .filter(|s| !s.is_empty())
            .map(std::string::ToString::to_string),
    }
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
