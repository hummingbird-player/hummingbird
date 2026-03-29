use std::{fs::File, io::Read, path::PathBuf};

use minisign_verify::{PublicKey, Signature};
use rand::{RngExt, distr::Alphanumeric, rng};
use tokio::{fs::create_dir, io::AsyncWriteExt};
use tracing::info;

use crate::update::check::Update;

pub async fn download(update: Update, package: &str) -> anyhow::Result<PathBuf> {
    // place in randomly generated sub-folder
    let temp_path = std::env::temp_dir().join(format!(
        "hb-update-{}",
        rng()
            .sample_iter(&Alphanumeric)
            .take(16)
            .map(|b| b as char)
            .collect::<String>()
    ));

    create_dir(&temp_path).await?;

    let package_path = temp_path.join(package);

    let client = zed_reqwest::Client::new();
    let mut response = client.get(&update.url).send().await?;

    let mut file = tokio::fs::File::create(&package_path).await?;

    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk).await?;
    }

    file.flush().await?;

    let minisign_url = update.url + ".minisig";
    let minisign_response = client.get(&minisign_url).send().await?;
    let minisign_signature = minisign_response.text().await?;

    let package_path_clone = package_path.clone();

    // verify minisign signature
    crate::RUNTIME
        .spawn_blocking(move || -> anyhow::Result<()> {
            let signature = Signature::decode(&minisign_signature)?;
            let key =
                PublicKey::from_base64("RWTVGbNhJ/77g9Dm280SNcfxaPz118Hgg8vI55tFX83sIMiObZuxpDyV")?;
            let mut verifier = key.verify_stream(&signature)?;

            let mut file = File::open(&package_path_clone)?;
            let mut buffer = [0u8; 4096];
            loop {
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    break;
                }

                verifier.update(&buffer[..n]);
            }

            verifier.finalize()?;

            Ok(())
        })
        .await??;

    info!(
        "Successfully verified minisign signature for {}",
        package_path.file_name().unwrap().display()
    );

    Ok(package_path)
}
