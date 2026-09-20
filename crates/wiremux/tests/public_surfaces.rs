//! Launch surfaces must not regress to stealth stubs.

#[test]
fn readme_is_the_product_page() {
    let readme = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"));
    assert!(
        readme.lines().next() == Some("# Wiremux"),
        "README heading must be the display name"
    );
    assert!(
        !readme.lines().any(|line| line == "Not ready."),
        "README must not be the stealth stub"
    );
    assert!(
        readme.contains("TokenProvider") && readme.contains("default-features = false"),
        "README must describe maps-only attach"
    );
}

#[test]
fn crate_metadata_is_not_reserved() {
    let wiremux = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
    let auth = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../wiremux-auth/Cargo.toml"
    ));
    for (name, toml) in [("wiremux", wiremux), ("wiremux-auth", auth)] {
        assert!(
            !toml.contains("description = \"Reserved.\""),
            "{name} description still Reserved"
        );
        assert!(
            !toml.contains("keywords = []"),
            "{name} keywords still empty"
        );
    }
}

#[test]
fn crate_rustdocs_are_not_stealth_stubs() {
    let maps = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let auth = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../wiremux-auth/src/lib.rs"
    ));
    let cli = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/bin/wiremux.rs"));
    assert!(
        !maps
            .lines()
            .next()
            .is_some_and(|line| line.contains("Not ready.")),
        "wiremux crate rustdoc still says Not ready."
    );
    assert!(
        !auth
            .lines()
            .next()
            .is_some_and(|line| line.contains("Not ready.")),
        "wiremux-auth crate rustdoc still says Not ready."
    );
    assert!(
        !cli.contains("about = \"Reserved.\""),
        "CLI about still Reserved."
    );
}

#[test]
fn social_preview_has_wordmark_and_sentence() {
    let svg = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/brand/social-preview.svg"
    ));
    assert!(svg.contains(">Wiremux<"), "wordmark");
    assert!(svg.contains("TokenProvider"), "advertisement sentence");
    assert!(svg.contains("viewBox=\"0 0 1280 640\""), "GitHub OG size");
    let png = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/brand/social-preview.png"
    );
    let meta = std::fs::metadata(png).expect("social-preview.png");
    assert!(meta.len() > 1024, "OG PNG too small: {}", meta.len());
}
