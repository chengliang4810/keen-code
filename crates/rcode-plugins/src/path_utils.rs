use std::path::Path;

pub(crate) fn path_to_frontend(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
