use super::*;

fn temp_store() -> (Arc<FileCredentialStore>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oauth_tokens.json");
    (Arc::new(FileCredentialStore::with_path(path)), dir)
}

#[test]
fn test_new_creates_default_path() {
    let store = FileCredentialStore::new();
    assert!(store.path().to_string_lossy().contains(".peri"));
}

#[tokio::test]
async fn test_ensure_file_creates_file_with_initial_content() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    drop(tmp);
    let store = FileCredentialStore::with_path(path.clone());
    store.ensure_file().unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let file: OAuthTokenFile = serde_json::from_str(&content).unwrap();
    assert_eq!(file.version, TOKEN_FILE_VERSION);
    assert!(file.tokens.is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

/// Unix MCP OAuth 凭据文件覆盖历史宽权限时必须收紧为私有权限。
#[cfg(unix)]
#[tokio::test]
async fn test_ensure_file_tightens_existing_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("oauth_tokens.json");
    std::fs::write(&path, r#"{"version":1,"tokens":{}}"#).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let store = FileCredentialStore::with_path(path.clone());
    store.ensure_file().unwrap();

    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test]
async fn test_load_nonexistent_server_returns_none() {
    let (store, _tmp) = temp_store();
    assert!(store.load_server("nonexistent").await.unwrap().is_none());
}

#[tokio::test]
async fn test_file_persists_across_instances() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    drop(tmp);
    let store1 = Arc::new(FileCredentialStore::with_path(path.clone()));
    store1
        .save_server(
            "srv1",
            StoredCredentials::new("client1".into(), None, vec![], None),
        )
        .await
        .unwrap();
    let store2 = Arc::new(FileCredentialStore::with_path(path));
    assert!(store2.load_server("srv1").await.unwrap().is_some());
}

#[tokio::test]
async fn test_clear_server() {
    let (store, _tmp) = temp_store();
    store
        .save_server(
            "srv",
            StoredCredentials::new("c".into(), None, vec![], None),
        )
        .await
        .unwrap();
    store.clear_server("srv").await.unwrap();
    assert!(store.load_server("srv").await.unwrap().is_none());
}

#[tokio::test]
async fn test_overwrite_server_token() {
    let (store, _tmp) = temp_store();
    store
        .save_server(
            "srv",
            StoredCredentials::new("c1".into(), None, vec![], None),
        )
        .await
        .unwrap();
    store
        .save_server(
            "srv",
            StoredCredentials::new("c2".into(), None, vec![], None),
        )
        .await
        .unwrap();
    assert_eq!(
        store.load_server("srv").await.unwrap().unwrap().client_id,
        "c2"
    );
}

#[tokio::test]
async fn test_clear_all() {
    let (store, _tmp) = temp_store();
    store
        .save_server("s1", StoredCredentials::new("c".into(), None, vec![], None))
        .await
        .unwrap();
    store
        .save_server("s2", StoredCredentials::new("c".into(), None, vec![], None))
        .await
        .unwrap();
    store.clear_all().await.unwrap();
    assert!(store.load_server("s1").await.unwrap().is_none());
}

#[tokio::test]
async fn test_list_servers() {
    let (store, _tmp) = temp_store();
    store
        .save_server("s1", StoredCredentials::new("c".into(), None, vec![], None))
        .await
        .unwrap();
    store
        .save_server("s2", StoredCredentials::new("c".into(), None, vec![], None))
        .await
        .unwrap();
    let servers = store.list_servers().await.unwrap();
    assert_eq!(servers.len(), 2);
}

#[tokio::test]
async fn test_concurrent_save_does_not_corrupt() {
    let (store, _tmp) = temp_store();
    let mut handles = vec![];
    for i in 0..10 {
        let s = store.clone();
        handles.push(tokio::spawn(async move {
            s.save_server(
                &format!("srv{}", i),
                StoredCredentials::new(format!("c{}", i), None, vec![], None),
            )
            .await
            .unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(store.list_servers().await.unwrap().len(), 10);
}
