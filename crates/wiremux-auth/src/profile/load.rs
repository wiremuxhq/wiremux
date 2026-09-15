//! Catalog keyed by document `id`. Files with different ids never merge.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use super::error::ProfileError;
use super::overlay;
use super::parse::{RawProfile, parse_layer_file, parse_layer_str, resolve};
use super::shipped;
use super::types::{
    Betas, Dialect, Http, ListMerge, LoadOptions, ResolvedProfile, StreamUnknownPolicy,
    ToolTypePolicy, Wire,
};

/// Overlay directories `load_profile` walks (XDG, then macOS Application
/// Support, then `WIREMUX_PROFILE_DIR`). Later dirs win on the same id.
pub fn user_profile_dirs() -> Vec<PathBuf> {
    discover_user_profile_dirs()
}

/// Directory `wiremux profile ingest` writes when `--dir` is omitted.
/// Last [`user_profile_dirs`] entry so `WIREMUX_PROFILE_DIR` and the
/// macOS Application Support path win the same way load does.
pub fn default_user_profile_dir() -> Option<PathBuf> {
    user_profile_dirs().into_iter().next_back()
}

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

/// Load a profile for a dialect wire.
///
/// If a catalog id equals the wire name (`messages`, `chat-completions`,
/// `responses`, `gemini`), that profile is loaded. Otherwise the first
/// non-shipped (user, extra-dir, or explicit) profile whose resolved
/// `wire` matches is returned. Shipped vendor packs are never selected
/// as the unknown-name fallback. If none match, a minimal in-memory
/// profile is returned so a host never needs `UnknownProvider` for a
/// known dialect.
pub fn load_profile_for_wire(
    wire: Wire,
    opts: &LoadOptions<'_>,
) -> Result<ResolvedProfile, ProfileError> {
    let name = wire.as_str();
    match load_profile(name, opts) {
        Ok(profile) => return Ok(profile),
        Err(ProfileError::NotFound { .. }) => {}
        Err(err) => return Err(err),
    }

    for id in unique_non_shipped_layer_ids(opts)? {
        if id == name {
            continue;
        }
        match load_profile(&id, opts) {
            Ok(profile) if profile.dialect.wire == Some(wire) => return Ok(profile),
            Ok(_) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(minimal_profile_for_wire(wire))
}

fn unique_non_shipped_layer_ids(opts: &LoadOptions<'_>) -> Result<Vec<String>, ProfileError> {
    let mut seen = BTreeSet::new();
    let mut ids = Vec::new();
    for layer in collect_layers(opts)? {
        if layer.from_shipped {
            continue;
        }
        if seen.insert(layer.id.clone()) {
            ids.push(layer.id);
        }
    }
    Ok(ids)
}

fn minimal_profile_for_wire(wire: Wire) -> ResolvedProfile {
    ResolvedProfile {
        schema_version: 1,
        id: wire.as_str().to_string(),
        display_name: None,
        dialect: Dialect {
            wire: Some(wire),
            stream_events: wire
                .default_stream_events()
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            list_merge: ListMerge::default(),
            tool_type_policy: ToolTypePolicy::HardError,
            stream_unknown_policy: StreamUnknownPolicy::default(),
        },
        http: Http {
            base_url: None,
            chat_path: Some(wire.default_chat_path().to_string()),
            auth_scheme: Some(wire.default_auth_scheme()),
            headers: std::collections::BTreeMap::new(),
            header_merge: ListMerge::default(),
        },
        oauth: None,
        access_env: Vec::new(),
        fingerprint: None,
        betas: Betas::default_for(Some(wire), false),
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
    from_shipped: bool,
}

fn collect_layers(opts: &LoadOptions<'_>) -> Result<Vec<Layer>, ProfileError> {
    let mut layers = Vec::new();
    if include_shipped(opts) {
        for doc in shipped::documents() {
            let profile = parse_layer_str(doc.document)?;
            if catalog_id(&profile, None).is_some() {
                layers.push(Layer {
                    id: doc.id.to_owned(),
                    profile,
                    from_shipped: true,
                });
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
    layers.push(Layer {
        id,
        profile,
        from_shipped: false,
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_opts() -> LoadOptions<'static> {
        LoadOptions {
            id: None,
            explicit_file: None,
            extra_profile_dirs: Vec::new(),
            include_shipped: false,
            include_user_config: false,
        }
    }

    #[test]
    fn load_profile_for_wire_messages_does_not_need_catalog_id() {
        let profile = load_profile_for_wire(Wire::Messages, &empty_opts())
            .expect("known dialect must not be NotFound");
        assert_eq!(profile.dialect.wire, Some(Wire::Messages));
        assert_eq!(profile.id, "messages");
        assert_eq!(profile.schema_version, 1);
        assert_eq!(profile.dialect.tool_type_policy, ToolTypePolicy::HardError);
        let err = load_profile("messages", &empty_opts()).expect_err("no catalog id");
        assert!(matches!(err, ProfileError::NotFound { .. }));
    }

    #[test]
    fn load_profile_unknown_id_suggests_close_match() {
        let err = load_profile(
            "anthropic-oath",
            &LoadOptions {
                include_user_config: false,
                ..LoadOptions::default()
            },
        )
        .expect_err("near-miss id must be NotFound");
        let text = err.to_string();
        assert!(
            matches!(err, ProfileError::NotFound { .. }),
            "must stay NotFound, got {text}"
        );
        assert!(
            text.contains("did you mean") && text.contains("anthropic"),
            "near-miss must suggest a shipped id, got {text}"
        );
    }

    #[test]
    fn load_profile_xai_grok_bild_suggests_xai_grok_build() {
        let err = load_profile(
            "xai-grok-bild",
            &LoadOptions {
                include_user_config: false,
                ..LoadOptions::default()
            },
        )
        .expect_err("typo must be NotFound");
        let text = err.to_string();
        assert!(
            text.contains("did you mean `xai-grok-build`"),
            "one-letter typo must prefer xai-grok-build over xai, got {text}"
        );
        assert!(
            !text.contains("did you mean `xai`"),
            "must not prefer the short prefix, got {text}"
        );
    }

    #[test]
    fn load_profile_for_wire_prefers_catalog_id_equal_to_wire_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("messages.toml"),
            r#"
schema_version = 1
id = "messages"
wire = "messages"
display_name = "catalog-id-hit"
"#,
        )
        .expect("write");
        let opts = LoadOptions {
            extra_profile_dirs: vec![dir.path().to_path_buf()],
            ..empty_opts()
        };
        let profile = load_profile_for_wire(Wire::Messages, &opts).expect("catalog id");
        assert_eq!(profile.id, "messages");
        assert_eq!(profile.display_name.as_deref(), Some("catalog-id-hit"));
        assert_eq!(profile.dialect.wire, Some(Wire::Messages));
    }

    #[test]
    fn load_profile_for_wire_finds_first_matching_wire() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("custom.toml"),
            r#"
schema_version = 1
id = "custom-gemini"
wire = "gemini"
"#,
        )
        .expect("write");
        let opts = LoadOptions {
            extra_profile_dirs: vec![dir.path().to_path_buf()],
            ..empty_opts()
        };
        let profile = load_profile_for_wire(Wire::Gemini, &opts).expect("wire match");
        assert_eq!(profile.id, "custom-gemini");
        assert_eq!(profile.dialect.wire, Some(Wire::Gemini));
    }

    #[test]
    fn load_profile_for_wire_default_opts_does_not_return_shipped_oauth() {
        let opts = LoadOptions {
            include_shipped: true,
            include_user_config: false,
            extra_profile_dirs: Vec::new(),
            ..empty_opts()
        };
        let messages = load_profile_for_wire(Wire::Messages, &opts).expect("dialect skeleton");
        assert_eq!(messages.id, "messages");
        assert!(messages.oauth.is_none());
        assert_eq!(messages.dialect.wire, Some(Wire::Messages));

        let chat = load_profile_for_wire(Wire::ChatCompletions, &opts).expect("dialect skeleton");
        assert_eq!(chat.id, "chat-completions");
        assert!(chat.oauth.is_none());
        assert_ne!(chat.id, "grok-ollama");
    }
}
