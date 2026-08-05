//! 图片读取项目边界测试。

use super::*;

/// PNG 文件签名；当前 MIME 检测只需要格式签名。
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

#[tokio::test]
async fn middleware_reads_relative_image_from_configured_project_root() {
    let project = tempfile::tempdir().unwrap();
    let image_dir = project.path().join("images");
    std::fs::create_dir_all(&image_dir).unwrap();
    std::fs::write(image_dir.join("pixel.png"), PNG_SIGNATURE).unwrap();
    let middleware = ImageMiddleware::new(project.path().to_string_lossy().to_string());
    let mut state =
        peri_agent::agent::state::AgentState::new(project.path().to_string_lossy().to_string());
    state.add_message(BaseMessage::human("@image images/pixel.png"));

    middleware.before_agent(&mut state).await.unwrap();

    let blocks = state.messages()[0].content_blocks();
    assert_eq!(blocks.len(), 1);
    assert!(matches!(blocks[0], ContentBlock::Image { .. }));
}

#[test]
fn load_image_file_accepts_relative_path_inside_project() {
    let project = tempfile::tempdir().unwrap();
    let image_dir = project.path().join("images");
    std::fs::create_dir_all(&image_dir).unwrap();
    std::fs::write(image_dir.join("pixel.png"), PNG_SIGNATURE).unwrap();

    let loaded =
        load_image_file(project.path().to_str().unwrap(), "images/pixel.png", 1024).unwrap();

    assert_eq!(loaded.media_type, "image/png");
    assert_eq!(loaded.data, PNG_SIGNATURE);
}

#[test]
fn load_image_file_allows_absolute_path_outside_project() {
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_image = outside.path().join("outside.png");
    std::fs::write(&outside_image, PNG_SIGNATURE).unwrap();

    let loaded = load_image_file(
        project.path().to_str().unwrap(),
        outside_image.to_str().unwrap(),
        1024,
    )
    .unwrap();

    assert_eq!(loaded.media_type, "image/png");
}

#[cfg(unix)]
#[test]
fn load_image_file_allows_symlink_escape() {
    use std::os::unix::fs::symlink;

    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_image = outside.path().join("outside.png");
    std::fs::write(&outside_image, PNG_SIGNATURE).unwrap();
    symlink(&outside_image, project.path().join("linked.png")).unwrap();

    let loaded = load_image_file(project.path().to_str().unwrap(), "linked.png", 1024).unwrap();

    assert_eq!(loaded.data, PNG_SIGNATURE);
}
