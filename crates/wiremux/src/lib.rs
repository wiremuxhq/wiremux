//! Dialect maps. Not ready.

pub use wiremux_auth::VERSION;

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_package() {
        assert_eq!(super::VERSION, env!("CARGO_PKG_VERSION"));
    }
}
