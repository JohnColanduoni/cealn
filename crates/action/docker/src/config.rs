use std::{
    collections::BTreeMap,
    env,
    fs::File,
    io::{self, Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
};

use anyhow::{anyhow, bail, Context};
use dkregistry::reference::Reference;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DockerConfig {
    creds_store: Option<String>,
    #[serde(default)]
    cred_helpers: BTreeMap<String, String>,
}

impl DockerConfig {
    pub fn load() -> anyhow::Result<DockerConfig> {
        let path = Self::get_path()?;
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(ref err) if err.kind() == io::ErrorKind::NotFound => return Ok(DockerConfig::default()),
            Err(err) => return Err(err.into()),
        };
        let config: DockerConfig = serde_json::from_reader(&mut file)?;
        Ok(config)
    }

    pub fn get_credentials(&self, image_id: &Reference) -> anyhow::Result<Option<Credential>> {
        let Some(cred_helper) = self.cred_helpers.get(&image_id.registry()).or_else(|| self.creds_store.as_ref()) else {
            return Ok(None);
        };
        let mut helper_process = Command::new(format!("docker-credential-{}", cred_helper))
            .arg("get")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn credential helper {}", cred_helper))?;

        {
            let mut stdin = helper_process.stdin.take().unwrap();
            stdin.write_all(image_id.registry().as_bytes())?;
            stdin.flush()?;
        }

        let output = helper_process.wait_with_output()?;

        if !output.status.success() {
            bail!(
                "credential helper {} exited with status {} (stderr: {:?})",
                cred_helper,
                output.status,
                String::from_utf8_lossy(&output.stderr),
            );
        }

        let data: Credential = serde_json::from_slice(&output.stdout)?;

        Ok(Some(data))
    }

    fn get_path() -> anyhow::Result<PathBuf> {
        if let Some(path) = env::var_os("DOCKER_CONFIG").map(PathBuf::from) {
            return Ok(path);
        }

        cfg_if::cfg_if! {
            if #[cfg(unix)] {
                let mut dir = dirs::home_dir().ok_or_else(|| anyhow!("missing home directory"))?;
                dir.push(".docker/config.json");
                Ok(dir)
            } else {
                compile_error!("unsupported platform");
            }
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct Credential {
    pub secret: String,
    pub username: String,
}
