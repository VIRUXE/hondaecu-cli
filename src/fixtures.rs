// ROM images the integration tests run against.
//
// The stock P28 image and the launch-control image are not distributable, so
// neither is in the repository and the tests that need them are opt-in:
//
//   feature `p28-rom`     + HONDAECU_P28_ROM     the stock P28-230 image
//   feature `launch-rom`  + HONDAECU_LAUNCH_ROM  the launch-control image
//   feature `rom-fixtures`                       both
//
// Without the feature those tests are reported as ignored, never as passed.
// With it, a path that is unset or does not name a file fails the test: a
// green run then means the ROM was really there. `.cargo/config.toml` gives
// both variables a default next to the checkout; set them to override.

use std::path::Path;

pub const P28_ROM_ENV: &str = "HONDAECU_P28_ROM";
pub const LAUNCH_ROM_ENV: &str = "HONDAECU_LAUNCH_ROM";

/// Path of the stock P28-230 image. Panics if it is not configured or absent.
pub fn p28_rom_path() -> String {
    required(P28_ROM_ENV, "the stock P28-230 ROM")
}

/// Path of the launch-control image. Panics if it is not configured or absent.
pub fn launch_rom_path() -> String {
    required(LAUNCH_ROM_ENV, "the launch-control ROM")
}

fn required(var: &str, what: &str) -> String {
    let path = std::env::var(var)
        .unwrap_or_else(|_| panic!("{var} is not set; it must name {what} (see README, Testing)"));
    assert!(
        Path::new(&path).is_file(),
        "{var} = {path:?} does not name a file; it must name {what}"
    );
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "is not set")]
    fn an_unset_fixture_variable_fails_instead_of_skipping() {
        required("HONDAECU_TEST_FIXTURE_THAT_IS_NEVER_SET", "a fixture");
    }

    #[test]
    #[should_panic(expected = "does not name a file")]
    fn a_fixture_path_without_a_file_fails_instead_of_skipping() {
        // Cargo sets this for every test run; the manifest directory is not a file.
        required("CARGO_MANIFEST_DIR", "a fixture");
    }
}
