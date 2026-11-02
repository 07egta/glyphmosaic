pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod cli;
pub mod error;

#[cfg(test)]
#[allow(dead_code)]
mod test_util;

#[cfg(test)]
mod smoke_tests {
    #[test]
    fn version_is_set() {
        assert!(!crate::VERSION.is_empty());
    }
}
