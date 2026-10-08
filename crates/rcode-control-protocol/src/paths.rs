use std::path::{Path, PathBuf};

pub fn user_root(home: &Path) -> PathBuf {
    home.join(".rcode")
}

pub fn control_descriptor(home: &Path) -> PathBuf {
    user_root(home).join("run").join("control.json")
}
