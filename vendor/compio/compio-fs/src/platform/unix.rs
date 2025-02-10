pub trait PermissionsExt {
    fn mode(&self) -> u32;
}

pub trait MetadataExt {
    fn dev(&self) -> u64;
    fn ino(&self) -> u64;
}
