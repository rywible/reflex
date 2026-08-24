use std::path::PathBuf;
use std::process::Command;

use crate::harness::AnyError;

const NATIVE_RUSTFLAGS: &str = "-Ctarget-cpu=native";

pub(super) fn build_native(arguments: &[String]) -> Result<(), AnyError> {
    let profile = parse_profile(arguments)?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let target = PathBuf::from("target/native");
    let status = Command::new(cargo)
        .args(["build", "--workspace", "--profile", profile])
        .env("RUSTFLAGS", NATIVE_RUSTFLAGS)
        .env("REFLEX_REQUESTED_PROFILE", profile)
        .env("REFLEX_REQUESTED_RUSTFLAGS", NATIVE_RUSTFLAGS)
        .env("CARGO_TARGET_DIR", &target)
        .status()?;
    if !status.success() {
        return Err(format!("host-native {profile} build failed with {status}").into());
    }
    println!(
        "host-native {profile} workspace built in {} with {NATIVE_RUSTFLAGS}",
        target.display()
    );
    Ok(())
}

fn parse_profile(arguments: &[String]) -> Result<&str, AnyError> {
    match arguments {
        [] => Ok("production"),
        [flag, profile]
            if flag == "--profile" && matches!(profile.as_str(), "production" | "profiling") =>
        {
            Ok(profile)
        }
        _ => Err("build-native accepts only --profile production|profiling".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_profile;

    #[test]
    fn native_build_profiles_are_explicit_and_bounded() {
        assert_eq!(parse_profile(&[]).unwrap(), "production");
        assert_eq!(
            parse_profile(&["--profile".into(), "profiling".into()]).unwrap(),
            "profiling"
        );
        assert!(parse_profile(&["--profile".into(), "release".into()]).is_err());
        assert!(parse_profile(&["extra".into()]).is_err());
    }
}
