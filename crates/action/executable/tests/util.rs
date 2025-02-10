use std::{fs, path::Path, process::Command};

pub fn extract_docker_root(image_name: &str, dest: &Path) {
    let output = Command::new("docker").arg("create").arg(image_name).output().unwrap();
    assert!(output.status.success());
    let image_id = String::from_utf8(output.stdout).unwrap();
    let image_id = image_id.trim();
    let status = Command::new("docker")
        .arg("cp")
        .arg(format!("{}:/", image_id))
        .arg(dest)
        .status()
        .unwrap();
    assert!(status.success());

    fs::remove_file(dest.join(".dockerenv")).unwrap();
}
