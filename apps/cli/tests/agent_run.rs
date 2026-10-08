use serde_json::{json, Value};
use std::{
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

struct RunResult {
    directory: tempfile::TempDir,
    status: ExitStatus,
    stdout: String,
    stderr: String,
    requests: Vec<Value>,
}

impl RunResult {
    fn events(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn workspace(&self) -> std::path::PathBuf {
        self.directory.path().join("workspace")
    }

    fn tool_result(&self) -> Value {
        self.events()
            .into_iter()
            .find(|event| event["type"] == "tool_result")
            .expect("工具结果必须通过共享事件返回")["result"]
            .clone()
    }
}

fn read_request(stream: &mut TcpStream) -> io::Result<Value> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    assert!(line.starts_with("POST /v1/chat/completions "), "{line}");
    let mut length = 0;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
    }
    assert!((1..=1024 * 1024).contains(&length));
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}

fn sse(delta: Value, reason: &str) -> String {
    let chunk = json!({"id":"fixture", "model":"fixture-model", "choices":[{
        "index":0, "delta":delta, "finish_reason":null
    }]});
    let end = json!({"id":"fixture", "choices":[{
        "index":0, "delta":{}, "finish_reason":reason
    }]});
    format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n")
}

fn run(
    target: &str,
    permission: &str,
    plan: bool,
    json_output: bool,
    fail_provider: bool,
) -> RunResult {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    fs::write(root.join("AGENTS.md"), "workspace fixture instructions").unwrap();
    fs::write(root.join(".env.fixture"), "synthetic private content").unwrap();
    let arguments = json!({"file_path":root.join(target), "content":"CLI 独立写入成功\n"});
    let first_response = sse(
        json!({"role":"assistant", "tool_calls":[{
            "index":0, "id":"fixture-write", "type":"function",
            "function":{"name":"Write", "arguments":arguments.to_string()}
        }]}),
        "tool_calls",
    );
    let final_response = sse(
        json!({"role":"assistant", "content":"CLI_FIXTURE_DONE"}),
        "stop",
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server = thread::spawn(move || -> io::Result<Vec<Value>> {
        let mut requests = Vec::new();
        for round in 0..4 {
            let (mut stream, _) = listener.accept()?;
            if server_stop.load(Ordering::SeqCst) {
                break;
            }
            stream.set_write_timeout(Some(Duration::from_secs(3)))?;
            requests.push(read_request(&mut stream)?);
            let (status, content_type, body): (&str, &str, &str) = if fail_provider {
                (
                    "401 Unauthorized",
                    "application/json",
                    "{\"error\":{\"message\":\"fixture authentication rejected\"}}",
                )
            } else {
                (
                    "200 OK",
                    "text/event-stream",
                    if round == 0 {
                        &first_response
                    } else {
                        &final_response
                    },
                )
            };
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
            stream.flush()?;
        }
        Ok(requests)
    });
    let stdout_path = directory.path().join("stdout");
    let stderr_path = directory.path().join("stderr");
    let mut command = Command::new(env!("CARGO_BIN_EXE_rcode-cli"));
    command
        .args([
            "run",
            "--base-url",
            &format!("http://{address}/v1"),
            "--model",
            "fixture-model",
            "--permission",
            permission,
        ])
        .arg("--cwd")
        .arg(&root)
        .env_remove("RCODE_API_KEY")
        .env_remove("RCODE_CONTROL_ADDR")
        .env_remove("RCODE_CONTROL_TOKEN")
        .stdin(Stdio::null())
        .stdout(fs::File::create(&stdout_path).unwrap())
        .stderr(fs::File::create(&stderr_path).unwrap());
    if plan {
        command.arg("--plan");
    }
    if json_output {
        command.arg("--json");
    }
    command.arg("complete fixture task");
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        thread::sleep(Duration::from_millis(10));
    };
    stop.store(true, Ordering::SeqCst);
    let _ = TcpStream::connect_timeout(&address, Duration::from_secs(1));
    let requests = server.join().unwrap().unwrap();
    let stdout = fs::read_to_string(stdout_path).unwrap();
    let stderr = fs::read_to_string(stderr_path).unwrap();
    RunResult {
        directory,
        status: status.unwrap_or_else(|| panic!("CLI 超时: {stderr}")),
        stdout,
        stderr,
        requests,
    }
}

#[test]
fn standalone_edit_writes_and_streams_shared_events() {
    let result = run("result.txt", "edit", false, true, false);
    assert!(result.status.success(), "{}", result.stderr);
    assert_eq!(
        fs::read_to_string(result.workspace().join("result.txt")).unwrap(),
        "CLI 独立写入成功\n"
    );
    assert_eq!(result.requests.len(), 2);
    assert!(result.requests[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("workspace fixture instructions"));
    assert_eq!(result.tool_result()["isError"], false);
    assert!(result.requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "tool" && message["tool_call_id"] == "fixture-write"));
    let events = result.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "tool_result")
            .count(),
        1
    );
    assert_eq!(events[events.len() - 2]["type"], "finish");
    assert_eq!(events.last().unwrap()["type"], "end");
    assert!(!events.iter().any(|event| event["type"] == "approval"));
}

#[test]
fn text_mode_prints_model_response() {
    let result = run("result.txt", "edit", false, false, false);
    assert!(result.status.success(), "{}", result.stderr);
    assert_eq!(result.stdout, "CLI_FIXTURE_DONE\n");
}

#[test]
fn noninteractive_ask_denies_write_and_returns_result_to_model() {
    let result = run("result.txt", "ask", false, true, false);
    assert!(result.status.success(), "{}", result.stderr);
    assert!(!result.workspace().join("result.txt").exists());
    assert_eq!(result.requests.len(), 2);
    assert_eq!(result.tool_result()["isError"], true);
    assert!(result
        .events()
        .iter()
        .any(|event| event["type"] == "approval"));
}

#[test]
fn plan_excludes_mutations_even_with_full_access() {
    let result = run("result.txt", "full-access", true, true, false);
    assert!(!result.workspace().join("result.txt").exists());
    let tools = result.requests[0]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 3);
    assert!(tools.iter().all(|tool| matches!(
        tool["function"]["name"].as_str(),
        Some("Read" | "Glob" | "Grep")
    )));
    assert_eq!(result.tool_result()["isError"], true);
}

#[test]
fn full_access_preserves_secret_and_workspace_boundaries() {
    for target in [".env.fixture", "../outside.txt"] {
        let result = run(target, "full-access", false, true, false);
        assert!(result.status.success(), "{}", result.stderr);
        assert_eq!(result.tool_result()["isError"], true);
        assert!(result.tool_result().to_string().contains("path_denied"));
        assert_eq!(
            fs::read_to_string(result.workspace().join(".env.fixture")).unwrap(),
            "synthetic private content"
        );
        assert!(!result.directory.path().join("outside.txt").exists());
        assert!(!result.stdout.contains("synthetic private content"));
    }
}

#[test]
fn provider_failure_emits_error_before_end_and_exits_nonzero() {
    let result = run("result.txt", "edit", false, true, true);
    assert!(!result.status.success());
    let events = result.events();
    let error = events
        .iter()
        .position(|event| event["type"] == "error")
        .unwrap();
    let finish = events
        .iter()
        .position(|event| event["type"] == "finish")
        .unwrap();
    assert!(error < finish);
    assert_eq!(events.last().unwrap()["type"], "end");
    assert!(!result.workspace().join("result.txt").exists());
    assert_eq!(
        serde_json::from_str::<Value>(result.stderr.trim()).unwrap()["ok"],
        false
    );
}

#[cfg(unix)]
#[test]
fn interrupt_cancels_an_open_model_stream_and_exits_promptly() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (ready, received) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        let chunk = json!({"choices":[{"index":0,"delta":{"role":"assistant","content":"waiting"},"finish_reason":null}]});
        write!(stream, "data: {chunk}\n\n").unwrap();
        stream.flush().unwrap();
        ready.send(()).unwrap();
        let mut byte = [0];
        let _ = stream.read(&mut byte);
    });
    let stdout_path = root.join("stdout");
    let mut child = Command::new(env!("CARGO_BIN_EXE_rcode-cli"))
        .args([
            "run",
            "--base-url",
            &format!("http://{address}/v1"),
            "--model",
            "fixture-model",
            "--plan",
            "--json",
        ])
        .arg("--cwd")
        .arg(&root)
        .arg("wait for interruption")
        .env_remove("RCODE_API_KEY")
        .stdin(Stdio::null())
        .stdout(fs::File::create(&stdout_path).unwrap())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = received.recv_timeout(Duration::from_secs(5)).is_ok();
    if started {
        // 子进程属于本测试，只向该 CLI 发送终端中断信号。
        assert_eq!(
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
            0
        );
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline || !started {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        thread::sleep(Duration::from_millis(10));
    };
    if !started {
        let _ = TcpStream::connect_timeout(&address, Duration::from_secs(1));
    }
    let _ = server.join();
    let status = status.expect("Ctrl+C 必须及时结束 CLI");
    assert_eq!(status.code(), Some(1));
    let events: Vec<Value> = fs::read_to_string(stdout_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events
        .iter()
        .any(|event| event["type"] == "finish" && event["cancelled"] == true));
    assert_eq!(events.last().unwrap()["type"], "end");
}
