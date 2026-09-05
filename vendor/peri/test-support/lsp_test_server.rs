//! 仅供 peri LSP 测试使用的最小 Rust 子进程伪服务器。
//!
//! 该 fixture 仅使用 Rust 标准库，避免测试依赖任何外部脚本运行时。它由
//! `peri-test-support` 的 build script 编译为测试子进程，只有父测试显式设置
//! 环境变量后才启动协议循环。

use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    thread,
    time::Duration,
};

const ENABLE_ENV: &str = "PERI_LSP_TEST_SERVER";
const COUNT_ENV: &str = "PERI_LSP_TEST_COUNT";
const DID_OPEN_ENV: &str = "PERI_LSP_TEST_DIDOPEN";
const PID_ENV: &str = "PERI_LSP_TEST_PID";
const CHILD_PID_ENV: &str = "PERI_LSP_TEST_CHILD_PID";
const TREE_MARKER_ENV: &str = "PERI_LSP_TEST_TREE_MARKER";

/// 仅在父测试设置启用标记时启动伪服务器，避免 Cargo 直接执行测试目标时阻塞。
fn main() {
    if env::var_os(ENABLE_ENV).is_none() {
        return;
    }

    let mode = env::args().nth(1).unwrap_or_else(|| "basic".to_string());
    if mode == "sleep" {
        thread::sleep(Duration::from_secs(60));
        return;
    }
    if mode == "tree" {
        run_process_tree_server();
        return;
    }
    if mode == "tree-grandchild" {
        write_env_file(
            CHILD_PID_ENV,
            format!("{}\n", std::process::id()).as_bytes(),
        );
        // 子进程只在树清理失败时写 marker，测试可据此区分“根进程已杀”与“整树已杀”。
        if let Some(marker) = env::var_os(TREE_MARKER_ENV) {
            thread::sleep(Duration::from_secs(2));
            append_env_file_path(&marker, b"grandchild-survived\n");
        } else {
            thread::sleep(Duration::from_secs(60));
        }
        return;
    }

    if mode == "basic-pid" || mode == "slow-pid" || mode == "close-stdin" {
        write_env_file(PID_ENV, format!("{}\n", std::process::id()).as_bytes());
    }
    if mode == "basic" || mode == "basic-pid" || mode == "record" {
        append_env_file(COUNT_ENV, b"spawned\n");
    }

    if mode == "unknown-request" {
        run_unknown_request_server();
    } else {
        run_lsp_server(&mode);
    }
}

/// 按 LSP 的 Content-Length 帧读取一条消息。
fn read_message(reader: &mut BufReader<File>) -> io::Result<Option<String>> {
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Ok(None);
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        headers.push_str(&line);
    }

    let content_length = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("Content-Length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    });
    let Some(content_length) = content_length else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "缺少 Content-Length",
        ));
    };

    let mut body = vec![0_u8; content_length];
    reader.read_exact(&mut body)?;
    String::from_utf8(body)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// 从测试消息中提取简单的数值 JSON-RPC id。
fn request_id(body: &str) -> Option<String> {
    let marker = "\"id\"";
    let start = body.find(marker)? + marker.len();
    let value = body[start..].trim_start().strip_prefix(':')?.trim_start();
    let end = value
        .find(|character: char| !character.is_ascii_digit() && character != '-')
        .unwrap_or(value.len());
    (end > 0).then(|| value[..end].to_string())
}

/// 向 stdout 写回一条 JSON-RPC 响应。
fn write_response(id: &str) -> io::Result<()> {
    let body = format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":null}}");
    let mut stdout = io::stdout().lock();
    write!(stdout, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    stdout.flush()
}

/// 运行普通、录制、慢响应和关闭 stdin 等测试模式。
fn run_lsp_server(mode: &str) {
    let mut reader = match stdin_reader() {
        Ok(reader) => reader,
        Err(_) => return,
    };

    loop {
        let body = match read_message(&mut reader) {
            Ok(Some(body)) => body,
            Ok(None) | Err(_) => return,
        };

        if mode == "record" && body.contains("textDocument/didOpen") {
            append_env_file(DID_OPEN_ENV, format!("{body}\n").as_bytes());
        }

        let Some(id) = request_id(&body) else {
            continue;
        };
        if mode == "slow" || mode == "slow-pid" {
            thread::sleep(Duration::from_secs(3));
        }
        if write_response(&id).is_err() {
            return;
        }

        if mode == "close-stdin" {
            // reader 持有 stdin 管道的唯一文件对象；drop 后父进程的下一次写入
            // 会收到 broken pipe，子进程继续持有 stdout 以覆盖主动清理路径。
            drop(reader);
            thread::sleep(Duration::from_secs(30));
            return;
        }
    }
}

/// 发送服务器主动请求，并验证客户端按 JSON-RPC 规范回传 MethodNotFound。
fn run_unknown_request_server() {
    let body =
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"workspace/configuration\",\"params\":[]}";
    let mut stdout = io::stdout().lock();
    if write!(stdout, "Content-Length: {}\r\n\r\n{}", body.len(), body)
        .and_then(|_| stdout.flush())
        .is_err()
    {
        return;
    }
    drop(stdout);

    let Ok(mut reader) = stdin_reader() else {
        return;
    };
    let Ok(Some(response)) = read_message(&mut reader) else {
        return;
    };
    if response.contains("\"code\":-32601") || response.contains("\"code\": -32601") {
        std::process::exit(0);
    }
    std::process::exit(1);
}

/// 启动一个会继续持有孙进程的长驻 fixture，覆盖 LSP 传输的整树清理语义。
fn run_process_tree_server() {
    write_env_file(PID_ENV, format!("{}\n", std::process::id()).as_bytes());

    let executable = match env::current_exe() {
        Ok(path) => path,
        Err(_) => return,
    };
    let child = std::process::Command::new(executable)
        .arg("tree-grandchild")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(child) = child else {
        return;
    };
    write_env_file(CHILD_PID_ENV, format!("{}\n", child.id()).as_bytes());
    // 根进程和孙进程都在 2 秒后写 marker；测试在 close/drop 后检查 marker，
    // 可同时发现“只杀根进程”与“整树未清理”两类回归。
    thread::sleep(Duration::from_secs(2));
    if let Some(marker) = env::var_os(TREE_MARKER_ENV) {
        append_env_file_path(&marker, b"root-survived\n");
    }
    thread::sleep(Duration::from_secs(58));
}

/// 将当前标准输入转换为拥有所有权的文件对象，便于测试显式关闭 stdin。
fn stdin_reader() -> io::Result<BufReader<File>> {
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;

        // SAFETY: 测试子进程的 fd 0 是父进程创建的专用 stdin 管道，当前函数取得
        // 唯一所有权，BufReader drop 时负责关闭它。
        let file = unsafe { File::from_raw_fd(0) };
        return Ok(BufReader::new(file));
    }

    #[cfg(windows)]
    {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};

        let stdin = io::stdin();
        let handle = stdin.as_raw_handle();
        // Stdin 只是标准句柄的轻量访问器；将原始句柄所有权交给 File，避免
        // 临时 Stdin 对象离开作用域后与 File 重复关闭同一句柄。
        std::mem::forget(stdin);
        // SAFETY: handle 来自当前子进程标准输入，且上面已转移其唯一所有权。
        let file = unsafe { File::from_raw_handle(handle) };
        return Ok(BufReader::new(file));
    }

    #[allow(unreachable_code)]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "当前平台不支持测试 stdin 管道",
    ))
}

/// 将内容追加到环境变量指定的文件。
fn append_env_file(name: &str, content: &[u8]) {
    let Some(path) = env::var_os(name) else {
        return;
    };
    append_env_file_path(&path, content);
}

/// 将内容追加到已解析的测试 marker 路径。
fn append_env_file_path(path: &std::ffi::OsStr, content: &[u8]) {
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = file.write_all(content);
}

/// 覆盖写入环境变量指定的文件，并创建缺失的父目录。
fn write_env_file(name: &str, content: &[u8]) {
    let Some(path) = env::var_os(name) else {
        return;
    };
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(mut file) = File::create(path) else {
        return;
    };
    let _ = file.write_all(content);
}
