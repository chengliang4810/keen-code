use super::*;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn input_directory_environment_and_url_boundaries() {
    let root = tempfile::tempdir().unwrap();
    let nested = root.path().join("apps/web");
    std::fs::create_dir_all(&nested).unwrap();
    assert!(run_directory(root.path(), nested.to_str().unwrap()).is_ok());
    let outside = tempfile::tempdir().unwrap();
    assert!(run_directory(root.path(), outside.path().to_str().unwrap()).is_err());
    assert!(run_directory(root.path(), "missing-test-directory").is_err());
    let invalid = RunInput {
        project_id: "project".into(),
        command: " ".into(),
        cwd: nested.to_string_lossy().into(),
        env: None,
    };
    assert!(spawn(&invalid).is_err());
    assert!(
        spawn(&RunInput {
            command: "echo okay".into(),
            env: Some([("BAD=NAME".into(), "secret".into())].into()),
            ..invalid
        })
        .is_err()
    );
    let urls = declared_urls(
        "external https://example.com:8000 rejected; http://0.0.0.0:4173/token?secret=x https://[::1]:8443/path http://localhost:65536",
    );
    assert_eq!(
        urls.iter()
            .map(|u| u.origin().ascii_serialization())
            .collect::<Vec<_>>(),
        ["http://localhost:4173", "https://[::1]:8443"]
    );
}

#[test]
fn real_background_lifecycle_directory_env_ports_and_tree_stop() {
    runtime().block_on(async {
        let folder = tempfile::tempdir().unwrap();
        // 人工Node夹具只监听loopback并写入此临时目录，测试真实子进程而非模拟成功。
        std::fs::write(folder.path().join("server.cjs"), r#"
const fs = require('node:fs');
const http = require('node:http');
const server = http.createServer((_, response) => response.end('DEV_SERVER_FIXTURE_OK'));
server.listen(0, '127.0.0.1', () => {
 const port = server.address().port;
 console.log('http://127.0.0.1:' + port + '/private?token=fixture');
 fs.writeFileSync('proof.json', JSON.stringify({pid:process.pid,port,cwd:process.cwd(),env:process.env.DEV_FIXTURE}));
});
"#).unwrap();
        let manager = Arc::new(DevServers::default());
        let observed = Arc::new(Mutex::new(Vec::new()));
        let events = observed.clone();
        let sink: EventSink = Arc::new(move |event| events.lock().push(event));
        let input = || RunInput { project_id:"synthetic-project".into(), command:"node server.cjs".into(),
            cwd:folder.path().to_string_lossy().into(), env:Some([("DEV_FIXTURE".into(),"ENV_NATIVE_OK".into())].into()) };
        let run = manager.start(input(), sink.clone()).unwrap();
        assert!(manager.start(input(), sink.clone()).is_err());
        assert!(manager.stop_matching("synthetic-project", Some(run.pid.wrapping_add(1))).await.is_err());
        assert_eq!(manager.list().len(), 1);
        let proof = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(bytes) = std::fs::read(folder.path().join("proof.json"))
                    && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) { break value; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await;
        if let Err(e) = proof { manager.shutdown().await.unwrap(); panic!("Node夹具启动失败：{e}"); }
        let proof = proof.unwrap();
        assert_eq!(proof["env"],"ENV_NATIVE_OK");
        assert_eq!(Path::new(proof["cwd"].as_str().unwrap()).canonicalize().unwrap(),folder.path().canonicalize().unwrap());
        let child_pid = proof["pid"].as_u64().unwrap() as u32;
        let port = proof["port"].as_u64().unwrap() as u16;
        tokio::time::timeout(Duration::from_secs(2),async {
            while !run.announced.lock().contains_key(&port) { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await.unwrap();
        assert_eq!(run.announced.lock()[&port],format!("http://127.0.0.1:{port}"));
        #[cfg(windows)]
        {
            assert_eq!(local_servers::owner(&manager.list(),child_pid,port).unwrap(),"synthetic-project");
            let result = local_servers::list(&manager.list()).unwrap();
            let row = result["servers"].as_array().unwrap().iter().find(|r|r["pid"]==child_pid).unwrap();
            assert_eq!(row["isStoppable"],true); assert!(row["addresses"][0]["url"].as_str().unwrap().ends_with(&port.to_string()));
            assert!(local_servers::owner(&[],child_pid,port).is_err());
        }
        #[cfg(not(windows))]
        let _ = child_pid;
        let (first_stop, concurrent_stop) = tokio::join!(manager.stop("synthetic-project"), manager.stop("synthetic-project"));
        assert!(first_stop.unwrap());
        assert!(concurrent_stop.unwrap());
        assert!(manager.list().is_empty());
        assert!(!manager.stop("synthetic-project").await.unwrap());
        // 管理器停止必须关闭Node后代监听，而非仅终止cmd/sh父进程。
        assert!(std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST,port)).is_err());
        assert_eq!(observed.lock().len(),2);
        manager.shutdown().await.unwrap();
        assert!(manager.start(input(),sink).is_err());
    });
}

#[test]
fn natural_exit_reconciles_registry_and_allows_restart() {
    runtime().block_on(async {
        let folder = tempfile::tempdir().unwrap();
        let manager = Arc::new(DevServers::default());
        let (sent, mut received) = mpsc::unbounded_channel();
        let sink: EventSink = Arc::new(move |event| {
            let _ = sent.send(event);
        });
        for _ in 0..2 {
            manager
                .start(
                    RunInput {
                        project_id: "one-shot".into(),
                        command: "echo fixture".into(),
                        cwd: folder.path().to_string_lossy().into(),
                        env: None,
                    },
                    sink.clone(),
                )
                .unwrap();
            assert!(matches!(
                received.recv().await,
                Some(Event::Upserted { .. })
            ));
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(10), received.recv())
                    .await
                    .unwrap(),
                Some(Event::Removed {
                    reason: "exited",
                    ..
                })
            ));
            assert!(manager.list().is_empty());
        }
    });
}

#[test]
fn shutdown_stops_active_tree_and_closes_start_entry() {
    runtime().block_on(async {
        let folder = tempfile::tempdir().unwrap();
        let manager = Arc::new(DevServers::default());
        #[cfg(windows)]
        let command = "ping -n 30 127.0.0.1 >nul";
        #[cfg(not(windows))]
        let command = "sleep 30";
        let input = || RunInput {
            project_id: "shutdown-fixture".into(),
            command: command.into(),
            cwd: folder.path().to_string_lossy().into(),
            env: None,
        };
        manager.start(input(), Arc::new(|_| {})).unwrap();
        assert_eq!(manager.list().len(), 1);
        manager.shutdown().await.unwrap();
        assert!(manager.list().is_empty());
        assert!(manager.start(input(), Arc::new(|_| {})).is_err());
    });
}
