use std::{env, path::PathBuf};

use anyhow::{Context as _, Result};
use vergen::{BuildBuilder, Emitter};

fn read_or_create_file(path: &str) -> Result<String> {
    std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .and_then(std::io::read_to_string)
        .with_context(|| format!("failed to read or create file '{path}'"))
}

fn option_env(var: &str) -> Option<Result<String>> {
    println!("cargo:rerun-if-env-changed={var}");
    Some(std::env::var(var))
        .filter(|res| !matches!(res, Err(std::env::VarError::NotPresent)))
        .map(|res| res.with_context(|| format!("invalid environment variable '{var}'")))
}

fn version_id_from_git() -> Result<String> {
    println!("cargo:rerun-if-changed=.git/logs/HEAD");
    let git = std::process::Command::new("git")
        .arg("--git-dir=.git")
        .args(["rev-parse", "HEAD"])
        .output()
        .context("failed to run git: is it installed?")?;
    if git.status.success()
        && let output = git.stdout.trim_ascii_end()
        && output.iter().all(u8::is_ascii_hexdigit)
        && let Ok(sha) = std::str::from_utf8(output)
    {
        Ok(sha[..7].to_owned())
    } else {
        anyhow::bail!(
            "git returned an error: `git rev-parse HEAD` exited with {}\
            \n== stdout ==\n{}\n== stderr ==\n{}",
            git.status,
            String::from_utf8_lossy(&git.stdout),
            String::from_utf8_lossy(&git.stderr),
        );
    }
}

fn main() -> Result<()> {
    // set build time information
    let build = BuildBuilder::default()
        .build_timestamp(true)
        .use_local(false)
        .build()?;

    Emitter::default().add_instructions(&build)?.emit()?;

    // read env
    let envfile = read_or_create_file(".env")?;
    println!("cargo:rerun-if-changed=.env");
    dotenvy::from_read(envfile.as_bytes())?;
    let mut vars = dotenvy::from_read_iter(envfile.as_bytes());
    while let Some((key, _)) = vars.next().transpose()? {
        println!("cargo:rustc-env={key}={}", std::env::var(&key)?);
    }

    // generate translations
    let path: PathBuf = env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is not set")
        .into();

    cntp_i18n_gen::generate_default(&path);

    // prefer env over file, default to `dev`. fails if non-utf8 or io fails
    let channel = option_env("HUMMINGBIRD_RELEASE_CHANNEL").unwrap_or_else(|| {
        let channel = read_or_create_file("package/RELEASE_CHANNEL")?;
        println!("cargo:rerun-if-changed=package/RELEASE_CHANNEL");
        Ok(channel.trim_ascii_end().to_owned())
    });

    println!(
        "cargo:rustc-env=HUMMINGBIRD_CHANNEL={}",
        channel.as_deref().unwrap_or("dev")
    );

    // get parenthesized version id based on channel kind
    channel.and_then(|kind| {
        let id = option_env("HUMMINGBIRD_VERSION_ID");
        let (suffix, mut id) = match &*kind {
            "" | "dev" => ("-dev", id.unwrap_or_else(version_id_from_git)?),
            "stable" => ("", id.unwrap_or_else(|| Ok("release".to_owned()))?),
            "flake" => ("-flake", id.context("nix didn't provide git sha")??),
            other => anyhow::bail!("invalid release channel '{other}'"),
        };
        if !id.is_empty() {
            id = format!(" ({id})");
        }
        println!(
            "cargo:rustc-env=HUMMINGBIRD_VERSION_STRING={}{suffix}{id}",
            std::env::var("CARGO_PKG_VERSION")?,
        );
        Ok(())
    })
}
