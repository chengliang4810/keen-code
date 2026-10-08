//! 原生更新的验收、暂存、替换和重启。
//!
//! `app_updates` 已经完成下载缓存的 SHA-256 与 minisign 校验；这里继续限制
//! 缓存路径、sidecar 和可执行文件格式，再由一个独立的旧进程副本等待当前进程
//! 退出后替换目标文件。这样更新流程不会把未经验证的缓存直接当作进程启动。

use std::{
    fs,
    fs::OpenOptions,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicIsize, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{native_controls::NativeWindowHandle, native_paths::NativePaths};

const UPDATE_CACHE_RELATIVE_PATH: &str = "updates/pending-update.bin";
const UPDATE_HELPER_ARG: &str = "--keencode-apply-update";
const UPDATE_SIDECAR_SCHEMA: u32 = 1;
const UPDATE_RETRY_COUNT: usize = 300;
const UPDATE_RETRY_DELAY: Duration = Duration::from_millis(100);

#[derive(Clone)]
struct StagedUpdate {
    staged_path: PathBuf,
    target_path: PathBuf,
}

/// 更新正文旁的受控元数据；助手不信任命令行中的摘要或签名。
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UpdateSidecar {
    schema: u32,
    sha256: String,
    signature: String,
}

/// 已验证更新的原生交接器；只保存当前一次安装的暂存路径，不复制包正文。
pub(crate) struct NativeUpdateInstaller {
    paths: Arc<NativePaths>,
    staged: Mutex<Option<StagedUpdate>>,
}

impl NativeUpdateInstaller {
    pub(crate) fn new(paths: Arc<NativePaths>) -> Arc<Self> {
        Arc::new(Self {
            paths,
            staged: Mutex::new(None),
        })
    }

    /// 将 `app_updates` 刚完成验签的缓存复制到独立暂存文件和 sidecar。
    ///
    /// 这里再次校验摘要、签名和可执行格式，并在重新读取缓存后重复校验，避免
    /// 下载缓存从主状态机交接到原生助手的过程中被替换。
    pub(crate) fn install_verified_update(
        &self,
        package_path: &Path,
        bytes: &[u8],
        expected_sha256: &[u8; 32],
        signature: &str,
    ) -> Result<(), String> {
        verify_update_payload(bytes, expected_sha256, signature)?;
        let expected_path = self.paths.data_root.join(UPDATE_CACHE_RELATIVE_PATH);
        let expected_path = fs::canonicalize(&expected_path)
            .map_err(|error| format!("解析已验证更新缓存失败：{error}"))?;
        let package_metadata = fs::symlink_metadata(package_path)
            .map_err(|error| format!("读取已验证更新缓存失败：{error}"))?;
        if package_metadata.file_type().is_symlink() || !package_metadata.is_file() {
            return Err("更新缓存必须是普通文件".to_owned());
        }
        let package_path = fs::canonicalize(package_path)
            .map_err(|error| format!("解析已验证更新缓存失败：{error}"))?;
        if !same_path(&package_path, &expected_path) {
            return Err("更新缓存路径不是当前应用的受控缓存".to_owned());
        }
        let on_disk = fs::read(&package_path)
            .map_err(|error| format!("重新读取已验证更新缓存失败：{error}"))?;
        if on_disk != bytes {
            return Err("更新缓存内容在验签后发生变化".to_owned());
        }
        verify_update_payload(&on_disk, expected_sha256, signature)?;

        let target_path = fs::canonicalize(
            std::env::current_exe().map_err(|error| format!("读取当前程序路径失败：{error}"))?,
        )
        .map_err(|error| format!("解析当前程序路径失败：{error}"))?;
        let target_metadata = fs::symlink_metadata(&target_path)
            .map_err(|error| format!("读取当前程序文件失败：{error}"))?;
        if target_metadata.file_type().is_symlink() || !target_metadata.is_file() {
            return Err("当前程序路径不是普通文件".to_owned());
        }

        let update_dir = self.paths.data_root.join("updates");
        fs::create_dir_all(&update_dir)
            .map_err(|error| format!("创建更新暂存目录失败：{error}"))?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let staged_path = update_dir.join(format!(
            "keencode-update-{}-{nonce}.bin",
            std::process::id()
        ));
        let temporary_path = staged_path.with_extension("tmp");
        if let Err(error) = write_synced_file(&temporary_path, bytes) {
            remove_update_file(&temporary_path);
            return Err(error);
        }
        if let Err(error) = fs::rename(&temporary_path, &staged_path) {
            remove_update_file(&temporary_path);
            remove_update_file(&staged_path);
            return Err(format!("提交更新暂存文件失败：{error}"));
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = target_metadata.permissions().mode();
            if let Err(error) = fs::set_permissions(&staged_path, fs::Permissions::from_mode(mode))
            {
                remove_update_file(&staged_path);
                return Err(format!("设置更新程序权限失败：{error}"));
            }
        }

        let staged_bytes = match fs::read(&staged_path) {
            Ok(staged_bytes) => staged_bytes,
            Err(error) => {
                remove_update_file(&staged_path);
                return Err(format!("重新读取更新暂存文件失败：{error}"));
            }
        };
        if let Err(error) = verify_update_payload(&staged_bytes, expected_sha256, signature) {
            remove_update_file(&staged_path);
            return Err(error);
        }
        let staged_sidecar_path = sidecar_path(&staged_path);
        let sidecar = UpdateSidecar {
            schema: UPDATE_SIDECAR_SCHEMA,
            sha256: digest_hex(expected_sha256),
            signature: signature.to_owned(),
        };
        let sidecar_bytes = match serde_json::to_vec(&sidecar) {
            Ok(sidecar_bytes) => sidecar_bytes,
            Err(error) => {
                remove_update_file(&staged_path);
                return Err(format!("编码更新 sidecar 失败：{error}"));
            }
        };
        let sidecar_temporary = staged_sidecar_path.with_extension("tmp");
        if let Err(error) = write_synced_file(&sidecar_temporary, &sidecar_bytes).and_then(|()| {
            fs::rename(&sidecar_temporary, &staged_sidecar_path)
                .map_err(|error| format!("提交更新 sidecar 失败：{error}"))
        }) {
            let _ = fs::remove_file(&staged_path);
            let _ = fs::remove_file(&sidecar_temporary);
            return Err(error);
        }

        let mut staged = self
            .staged
            .lock()
            .map_err(|_| "更新安装器状态锁已损坏".to_owned())?;
        if let Some(previous) = staged.replace(StagedUpdate {
            staged_path: staged_path.clone(),
            target_path,
        }) {
            remove_update_file(&previous.staged_path);
            remove_update_file(&sidecar_path(&previous.staged_path));
        }
        Ok(())
    }

    /// 启动同一程序的更新助手；助手完成替换后再启动新版本，当前进程立即退出。
    pub(crate) fn restart_after_update(&self) -> Result<(), String> {
        let staged = self
            .staged
            .lock()
            .map_err(|_| "更新安装器状态锁已损坏".to_owned())?
            .take()
            .ok_or_else(|| "没有已暂存的更新程序".to_owned())?;
        let current = match std::env::current_exe()
            .map_err(|error| format!("读取当前程序路径失败：{error}"))
            .and_then(|path| {
                fs::canonicalize(path).map_err(|error| format!("解析当前程序路径失败：{error}"))
            }) {
            Ok(current) => current,
            Err(error) => {
                return Err(restore_staged_after_helper_failure(self, staged, error));
            }
        };
        if !same_path(&current, &staged.target_path) {
            let error = "更新目标不是当前正在运行的程序".to_owned();
            return Err(restore_staged_after_helper_failure(self, staged, error));
        }
        let Some(current_dir) = current.parent() else {
            return Err(restore_staged_after_helper_failure(
                self,
                staged,
                "当前程序路径缺少父目录".to_owned(),
            ));
        };
        let mut helper = Command::new(&current);
        helper
            .arg(UPDATE_HELPER_ARG)
            .arg(&staged.staged_path)
            .arg(&staged.target_path)
            .arg(&self.paths.data_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .current_dir(current_dir);
        if let Err(error) = helper.spawn() {
            return Err(restore_staged_after_helper_failure(
                self,
                staged,
                format!("启动原生更新助手失败：{error}"),
            ));
        }
        // 用户已经明确触发安装，且当前 Runtime 已完成 prepare_for_update；继续运行
        // 会让 Windows 锁住目标文件，也会造成旧版本继续接收输入。
        std::process::exit(0);
    }
}

/// 处理更新助手入口；返回 `true` 表示本次进程已完成替换并启动新版本。
pub(crate) fn run_update_helper_if_requested() -> Result<bool, String> {
    let mut args = std::env::args_os();
    let _program = args.next();
    let Some(flag) = args.next() else {
        return Ok(false);
    };
    if flag != UPDATE_HELPER_ARG {
        return Ok(false);
    }
    let staged_path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| "更新助手缺少暂存路径".to_owned())?;
    let target_path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| "更新助手缺少目标路径".to_owned())?;
    let data_root = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| "更新助手缺少数据根路径".to_owned())?;
    if args.next().is_some() {
        return Err("更新助手参数过多".to_owned());
    }

    let current = fs::canonicalize(
        std::env::current_exe().map_err(|error| format!("读取更新助手路径失败：{error}"))?,
    )
    .map_err(|error| format!("解析更新助手路径失败：{error}"))?;
    let target =
        fs::canonicalize(&target_path).map_err(|error| format!("解析更新目标失败：{error}"))?;
    if !same_path(&current, &target) {
        return Err("更新助手目标不是当前程序".to_owned());
    }
    let paths =
        NativePaths::discover().map_err(|error| format!("解析更新数据目录失败：{error}"))?;
    let expected_data_root = fs::canonicalize(&paths.data_root)
        .map_err(|error| format!("解析当前数据根路径失败：{error}"))?;
    let explicit_data_root = fs::canonicalize(&data_root)
        .map_err(|error| format!("解析更新助手数据根路径失败：{error}"))?;
    if !same_path(&explicit_data_root, &expected_data_root) {
        return Err("更新助手数据根路径不是当前应用的数据根".to_owned());
    }
    let staged_metadata = fs::symlink_metadata(&staged_path)
        .map_err(|error| format!("读取更新暂存文件失败：{error}"))?;
    if staged_metadata.file_type().is_symlink() || !staged_metadata.is_file() {
        return Err("更新暂存文件必须是普通文件".to_owned());
    }
    let staged =
        fs::canonicalize(&staged_path).map_err(|error| format!("解析更新暂存文件失败：{error}"))?;
    let updates_dir = fs::canonicalize(expected_data_root.join("updates"))
        .map_err(|error| format!("解析更新暂存目录失败：{error}"))?;
    validate_staged_update_path(&staged, &updates_dir)?;
    let sidecar = read_update_sidecar(&staged, &updates_dir)?;
    let bytes = fs::read(&staged).map_err(|error| format!("读取更新暂存文件失败：{error}"))?;
    verify_sidecar_payload(&bytes, &sidecar)?;

    let backup = backup_path(&staged);
    let recovery = recovery_path(&staged);
    let target_dir = target
        .parent()
        .ok_or_else(|| "更新目标缺少父目录".to_owned())?;
    copy_synced_file(&target, &backup)
        .map_err(|error| format!("备份当前 KeenCode 失败：{error}"))?;
    if let Err(error) = copy_synced_file(&staged, &recovery) {
        remove_update_file(&backup);
        return Err(format!("保存更新恢复副本失败：{error}"));
    }

    let mut last_error = None;
    for _ in 0..UPDATE_RETRY_COUNT {
        match replace_target(&staged, &target) {
            Ok(()) => {
                let mut restarted = Command::new(&target);
                let spawn_result = restarted
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .current_dir(target_dir)
                    .spawn();
                match spawn_result {
                    Ok(_) => {
                        cleanup_update_artifacts(&staged, &backup, &recovery)?;
                        return Ok(true);
                    }
                    Err(error) => {
                        let rollback = rollback_failed_update(&staged, &target, &backup, &recovery);
                        return Err(match rollback {
                            Ok(()) => format!("启动更新后的 KeenCode 失败：{error}"),
                            Err(rollback_error) => format!(
                                "启动更新后的 KeenCode 失败：{error}；回滚更新失败：{rollback_error}"
                            ),
                        });
                    }
                }
            }
            Err(error) => {
                last_error = Some(error.to_string());
                thread::sleep(UPDATE_RETRY_DELAY);
            }
        }
    }
    remove_update_file(&backup);
    remove_update_file(&recovery);
    Err(format!(
        "更新助手等待旧程序释放目标文件超时：{}",
        last_error.unwrap_or_else(|| "未知错误".to_owned())
    ))
}

fn verify_update_payload(
    bytes: &[u8],
    expected_sha256: &[u8; 32],
    signature: &str,
) -> Result<(), String> {
    if sha256(bytes) != *expected_sha256 {
        return Err("更新包 SHA-256 校验失败".to_owned());
    }
    crate::app_updates::verify_update_signature(bytes, signature)?;
    validate_executable_image(bytes)
}

fn verify_sidecar_payload(bytes: &[u8], sidecar: &UpdateSidecar) -> Result<(), String> {
    let expected_sha256 = parse_digest(&sidecar.sha256)?;
    verify_update_payload(bytes, &expected_sha256, &sidecar.signature)
}

fn read_update_sidecar(staged: &Path, updates_dir: &Path) -> Result<UpdateSidecar, String> {
    let sidecar = sidecar_path(staged);
    if sidecar
        .parent()
        .is_none_or(|parent| !same_path(parent, updates_dir))
    {
        return Err("更新 sidecar 不在当前应用的受控目录中".to_owned());
    }
    let metadata = fs::symlink_metadata(&sidecar)
        .map_err(|error| format!("读取更新 sidecar 失败：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("更新 sidecar 必须是普通文件".to_owned());
    }
    let canonical =
        fs::canonicalize(&sidecar).map_err(|error| format!("解析更新 sidecar 失败：{error}"))?;
    if !same_path(&canonical, &sidecar) {
        return Err("更新 sidecar 路径发生变化".to_owned());
    }
    let bytes = fs::read(&canonical).map_err(|error| format!("读取更新 sidecar 失败：{error}"))?;
    let sidecar: UpdateSidecar = serde_json::from_slice(&bytes)
        .map_err(|error| format!("解析更新 sidecar 失败：{error}"))?;
    if sidecar.schema != UPDATE_SIDECAR_SCHEMA {
        return Err("更新 sidecar 版本不受支持".to_owned());
    }
    let _ = parse_digest(&sidecar.sha256)?;
    if sidecar.signature.trim().is_empty() {
        return Err("更新 sidecar 缺少 minisign 签名".to_owned());
    }
    Ok(sidecar)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn digest_hex(digest: &[u8; 32]) -> String {
    let mut value = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

fn parse_digest(value: &str) -> Result<[u8; 32], String> {
    let value = value.as_bytes();
    if value.len() != 64 {
        return Err("更新 sidecar 的 SHA-256 长度无效".to_owned());
    }
    let mut digest = [0u8; 32];
    for (index, pair) in value.as_chunks::<2>().0.iter().enumerate() {
        let high =
            hex_digit(pair[0]).ok_or_else(|| "更新 sidecar 的 SHA-256 格式无效".to_owned())?;
        let low =
            hex_digit(pair[1]).ok_or_else(|| "更新 sidecar 的 SHA-256 格式无效".to_owned())?;
        digest[index] = (high << 4) | low;
    }
    Ok(digest)
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn sidecar_path(staged: &Path) -> PathBuf {
    staged.with_extension("json")
}

fn backup_path(staged: &Path) -> PathBuf {
    staged.with_extension("backup.bin")
}

fn recovery_path(staged: &Path) -> PathBuf {
    staged.with_extension("recovery.bin")
}

fn write_synced_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("创建更新暂存文件失败：{error}"))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("写入更新暂存文件失败：{error}"))
}

fn copy_synced_file(source: &Path, destination: &Path) -> Result<(), String> {
    let temporary = destination.with_extension("tmp");
    let result = (|| {
        let source_metadata =
            fs::metadata(source).map_err(|error| format!("读取更新备份源属性失败：{error}"))?;
        let mut source_file =
            fs::File::open(source).map_err(|error| format!("读取更新备份源失败：{error}"))?;
        let mut destination_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("创建更新备份临时文件失败：{error}"))?;
        io::copy(&mut source_file, &mut destination_file)
            .map_err(|error| format!("复制更新备份失败：{error}"))?;
        fs::set_permissions(&temporary, source_metadata.permissions())
            .map_err(|error| format!("同步更新备份权限失败：{error}"))?;
        destination_file
            .sync_all()
            .map_err(|error| format!("持久化更新备份失败：{error}"))?;
        drop(destination_file);
        fs::rename(&temporary, destination)
            .map_err(|error| format!("提交更新备份失败：{error}"))?;
        Ok::<(), String>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn remove_update_file(path: &Path) {
    let _ = fs::remove_file(path);
}

fn cleanup_update_artifacts(staged: &Path, backup: &Path, recovery: &Path) -> Result<(), String> {
    let mut errors = Vec::new();
    for path in [sidecar_path(staged), backup.to_owned(), recovery.to_owned()] {
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            errors.push(format!("{}：{error}", path.display()));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("清理更新助手文件失败：{}", errors.join("；")))
    }
}

fn rollback_failed_update(
    staged: &Path,
    target: &Path,
    backup: &Path,
    recovery: &Path,
) -> Result<(), String> {
    replace_target(backup, target).map_err(|error| format!("恢复旧程序失败：{error}"))?;
    replace_target(recovery, staged).map_err(|error| format!("恢复更新暂存文件失败：{error}"))?;
    Ok(())
}

fn restore_staged_after_helper_failure(
    installer: &NativeUpdateInstaller,
    staged: StagedUpdate,
    error: String,
) -> String {
    match installer.staged.lock() {
        Ok(mut current) if current.is_none() => {
            *current = Some(staged);
            error
        }
        Ok(_) => format!("{error}；更新暂存状态已有其他记录，未覆盖该记录"),
        Err(_) => format!("{error}；更新安装器状态锁已损坏，无法恢复暂存状态"),
    }
}

fn validate_executable_image(bytes: &[u8]) -> Result<(), String> {
    let valid = if cfg!(windows) {
        bytes.starts_with(b"MZ")
    } else if cfg!(target_os = "macos") {
        matches!(
            bytes.get(..4),
            Some([0xfe, 0xed, 0xfa, 0xce])
                | Some([0xce, 0xfa, 0xed, 0xfe])
                | Some([0xfe, 0xed, 0xfa, 0xcf])
                | Some([0xcf, 0xfa, 0xed, 0xfe])
                | Some([0xca, 0xfe, 0xba, 0xbe])
                | Some([0xbe, 0xba, 0xfe, 0xca])
        )
    } else {
        bytes.starts_with(b"\x7fELF")
    };
    valid
        .then_some(())
        .ok_or_else(|| "签名更新包不是当前平台的可执行文件格式".to_owned())
}

/// 更新助手只接受安装器生成的、位于当前数据根 updates 目录中的暂存文件。
/// 这会拒绝把命令行参数变成任意本地文件替换入口，即使调用者伪造了助手参数。
fn validate_staged_update_path(staged: &Path, updates_dir: &Path) -> Result<(), String> {
    if staged
        .parent()
        .is_none_or(|parent| !same_path(parent, updates_dir))
    {
        return Err("更新暂存文件不在当前应用的受控目录中".to_owned());
    }
    let valid_name = staged
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            let Some(name) = name.strip_prefix("keencode-update-") else {
                return false;
            };
            let Some(name) = name.strip_suffix(".bin") else {
                return false;
            };
            let mut parts = name.split('-');
            parts.next().is_some_and(|pid| pid.parse::<u32>().is_ok())
                && parts
                    .next()
                    .is_some_and(|nonce| nonce.parse::<u128>().is_ok())
                && parts.next().is_none()
        });
    if !valid_name {
        return Err("更新暂存文件名不是安装器生成的格式".to_owned());
    }
    Ok(())
}

fn replace_target(staged: &Path, target: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };

        let staged = staged
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let target = target
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let result = unsafe {
            MoveFileExW(
                staged.as_ptr(),
                target.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if result == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(staged, target).map_err(|error| error.to_string())
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::{
        UPDATE_SIDECAR_SCHEMA, UpdateSidecar, digest_hex, parse_digest, validate_executable_image,
        validate_staged_update_path,
    };
    use std::path::Path;

    #[test]
    fn helper_accepts_only_installer_staged_paths() {
        assert!(
            validate_staged_update_path(
                Path::new("updates/keencode-update-42-123.bin"),
                Path::new("updates"),
            )
            .is_ok()
        );
        assert!(
            validate_staged_update_path(
                Path::new("other/keencode-update-42-123.bin"),
                Path::new("updates"),
            )
            .is_err()
        );
        assert!(
            validate_staged_update_path(
                Path::new("updates/pending-update.bin"),
                Path::new("updates"),
            )
            .is_err()
        );
        assert!(
            validate_staged_update_path(
                Path::new("updates/keencode-update-42-123.exe"),
                Path::new("updates"),
            )
            .is_err()
        );
    }

    #[test]
    fn helper_rejects_empty_or_wrong_platform_images() {
        assert!(validate_executable_image(&[]).is_err());
        assert!(validate_executable_image(b"not-an-executable").is_err());
    }

    #[test]
    fn sidecar_digest_round_trips_as_lowercase_hex() {
        let mut digest = [0u8; 32];
        digest[0] = 0xab;
        digest[31] = 0xcd;
        let encoded = digest_hex(&digest);
        assert_eq!(encoded.len(), 64);
        assert_eq!(parse_digest(&encoded).unwrap(), digest);
        assert_eq!(parse_digest(&encoded.to_uppercase()).unwrap(), digest);
    }

    #[test]
    fn sidecar_schema_is_strict_and_versioned() {
        let sidecar = UpdateSidecar {
            schema: UPDATE_SIDECAR_SCHEMA,
            sha256: "00".repeat(32),
            signature: "minisign".to_owned(),
        };
        let encoded = serde_json::to_vec(&sidecar).unwrap();
        let decoded: UpdateSidecar = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.schema, UPDATE_SIDECAR_SCHEMA);
        assert!(serde_json::from_slice::<UpdateSidecar>(
            br#"{"schema":1,"sha256":"0000000000000000000000000000000000000000000000000000000000000000","signature":"x","extra":true}"#,
        )
        .is_err());
    }
}

/// 通过受控 HWND 判断当前窗口是否是前台窗口；非 Windows 平台返回 false，
/// 由系统通知实现决定是否抑制桌面提示。
#[derive(Clone, Default)]
pub(crate) struct WindowFocusProbe {
    hwnd: Arc<AtomicIsize>,
}

impl WindowFocusProbe {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn set_window(&self, window: NativeWindowHandle) {
        self.hwnd
            .store(window.raw().unwrap_or_default(), Ordering::Release);
    }

    pub(crate) fn is_focused(&self) -> bool {
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
            let hwnd = self.hwnd.load(Ordering::Acquire);
            hwnd != 0 && unsafe { GetForegroundWindow() } == hwnd as _
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}
