use serde::Serialize;
use std::{
    collections::BTreeMap,
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    process,
};

const SUPPORTED_PLATFORMS: [&str; 4] = [
    "darwin-aarch64",
    "darwin-x86_64",
    "linux-x86_64",
    "windows-x86_64",
];
const UPDATE_MANIFEST_SCHEMA: &str = "keencode/native-update";
const UPDATE_MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug)]
struct Config {
    version: String,
    release: String,
    notes: String,
    pub_date: String,
    repository: String,
    asset_dir: PathBuf,
    output: PathBuf,
    artifacts: Vec<(String, String)>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateManifest {
    schema: &'static str,
    schema_version: u32,
    version: String,
    release: String,
    notes: String,
    pub_date: String,
    platforms: BTreeMap<String, UpdateArtifact>,
}

#[derive(Debug, Serialize)]
struct UpdateArtifact {
    url: String,
    signature: String,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("生成更新清单失败：{error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_usage();
        return Ok(());
    }

    let config = parse_args(&args)?;
    validate_version(&config.version)?;
    if config.release != format!("v{}", config.version) {
        return Err("release 必须是与 version 对应的 vX.Y.Z 标签。".to_owned());
    }
    validate_repository(&config.repository)?;
    if config.notes.trim().is_empty() {
        return Err("notes 不能为空。".to_owned());
    }
    if config.pub_date.trim().is_empty() {
        return Err("pubDate 不能为空。".to_owned());
    }
    if config.artifacts.is_empty() {
        return Err("至少需要一个平台 artifact。".to_owned());
    }

    let mut platforms = BTreeMap::new();
    for (platform, filename) in &config.artifacts {
        if !SUPPORTED_PLATFORMS.contains(&platform.as_str()) {
            return Err(format!("不支持的平台键：{platform}"));
        }
        if platforms.contains_key(platform) {
            return Err(format!("平台 artifact 重复：{platform}"));
        }
        validate_filename(filename)?;

        let artifact_path = config.asset_dir.join(filename);
        if !artifact_path.is_file() {
            return Err(format!("桌面二进制不存在：{}", artifact_path.display()));
        }
        if fs::metadata(&artifact_path)
            .map_err(|error| format!("读取二进制元数据失败：{error}"))?
            .len()
            == 0
        {
            return Err(format!("桌面二进制为空：{}", artifact_path.display()));
        }

        let signature_path = config.asset_dir.join(format!("{filename}.minisig"));
        if !signature_path.is_file() {
            return Err(format!("签名文件不存在：{}", signature_path.display()));
        }
        let signature = fs::read_to_string(&signature_path)
            .map_err(|error| format!("读取签名文件失败：{error}"))?;
        if signature.trim().is_empty() {
            return Err(format!("签名文件为空：{}", signature_path.display()));
        }

        let url = format!(
            "https://github.com/{}/releases/download/{}/{}",
            config.repository, config.release, filename
        );
        platforms.insert(platform.clone(), UpdateArtifact { url, signature });
    }

    let manifest = UpdateManifest {
        schema: UPDATE_MANIFEST_SCHEMA,
        schema_version: UPDATE_MANIFEST_SCHEMA_VERSION,
        version: config.version,
        release: config.release,
        notes: config.notes,
        pub_date: config.pub_date,
        platforms,
    };
    let contents = serde_json::to_string_pretty(&manifest)
        .map_err(|error| format!("序列化更新清单失败：{error}"))?
        + "\n";
    write_atomic(&config.output, contents.as_bytes())?;
    println!("已生成更新清单：{}", config.output.display());
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Config, String> {
    let mut version = None;
    let mut release = None;
    let mut notes = None;
    let mut pub_date = None;
    let mut repository = None;
    let mut asset_dir = None;
    let mut output = None;
    let mut artifacts = Vec::new();

    let mut index = 0;
    while index < args.len() {
        let flag = &args[index];
        index += 1;
        match flag.as_str() {
            "--version" => version = Some(next_value(args, &mut index, flag)?),
            "--release" => release = Some(next_value(args, &mut index, flag)?),
            "--notes" => notes = Some(next_value(args, &mut index, flag)?),
            "--pub-date" => pub_date = Some(next_value(args, &mut index, flag)?),
            "--repository" => repository = Some(next_value(args, &mut index, flag)?),
            "--asset-dir" => {
                asset_dir = Some(PathBuf::from(next_value(args, &mut index, flag)?));
            }
            "--output" => output = Some(PathBuf::from(next_value(args, &mut index, flag)?)),
            "--artifact" => {
                let value = next_value(args, &mut index, flag)?;
                let (platform, filename) = value
                    .split_once('=')
                    .ok_or_else(|| "--artifact 必须使用 platform=filename 格式。".to_owned())?;
                if platform.is_empty() || filename.is_empty() {
                    return Err("--artifact 的平台键和文件名都不能为空。".to_owned());
                }
                artifacts.push((platform.to_owned(), filename.to_owned()));
            }
            _ => return Err(format!("未知参数：{flag}")),
        }
    }

    Ok(Config {
        version: required(version, "--version")?,
        release: required(release, "--release")?,
        notes: required(notes, "--notes")?,
        pub_date: required(pub_date, "--pub-date")?,
        repository: required(repository, "--repository")?,
        asset_dir: required(asset_dir, "--asset-dir")?,
        output: required(output, "--output")?,
        artifacts,
    })
}

fn next_value(args: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    let value = args
        .get(*index)
        .cloned()
        .ok_or_else(|| format!("{flag} 缺少参数值。"))?;
    *index += 1;
    Ok(value)
}

fn required<T>(value: Option<T>, flag: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("缺少参数：{flag}"))
}

fn validate_version(value: &str) -> Result<(), String> {
    let parts: Vec<&str> = value.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || (part.len() > 1 && part.starts_with('0'))
                || !part.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return Err("version 必须是三段、无前导零的数字版本号。".to_owned());
    }
    Ok(())
}

fn validate_repository(value: &str) -> Result<(), String> {
    let parts: Vec<&str> = value.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
    {
        return Err("repository 必须是 owner/name 格式。".to_owned());
    }
    Ok(())
}

fn validate_filename(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    // 文件名来自构建步骤；拒绝路径分隔符和特殊组件，避免清单工具读取 asset-dir 外的文件。
    if value.is_empty()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(format!("非法 artifact 文件名：{value}"));
    }
    Ok(())
}

fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), String> {
    if path.exists() {
        return Err(format!("输出文件已存在，拒绝覆盖：{}", path.display()));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| format!("创建输出目录失败：{error}"))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| "输出路径缺少文件名。".to_owned())?
        .to_string_lossy();
    let temporary = parent.join(format!(".{file_name}.tmp-{}", process::id()));

    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("创建临时清单失败：{error}"))?;
        file.write_all(contents)
            .map_err(|error| format!("写入临时清单失败：{error}"))?;
        file.sync_all()
            .map_err(|error| format!("同步临时清单失败：{error}"))?;
        fs::rename(&temporary, path).map_err(|error| format!("原子落盘更新清单失败：{error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn print_usage() {
    println!("keencode-release 纯 Rust 更新清单生成器");
    println!("需要 --version、--release、--notes、--pub-date、--repository、--asset-dir、--output");
    println!("并重复传入 --artifact platform=filename。");
}

#[cfg(test)]
mod tests {
    use super::{
        UPDATE_MANIFEST_SCHEMA, UPDATE_MANIFEST_SCHEMA_VERSION, UpdateArtifact, UpdateManifest,
        validate_filename, validate_repository, validate_version,
    };
    use std::collections::BTreeMap;

    #[test]
    fn serializes_the_native_update_manifest_contract() {
        let mut platforms = BTreeMap::new();
        platforms.insert(
            "windows-x86_64".to_owned(),
            UpdateArtifact {
                url: "https://github.com/chengliang4810/keen-code/releases/download/v1.2.3/KeenCode.exe"
                    .to_owned(),
                signature: "fixture-signature".to_owned(),
            },
        );
        let value = serde_json::to_value(UpdateManifest {
            schema: UPDATE_MANIFEST_SCHEMA,
            schema_version: UPDATE_MANIFEST_SCHEMA_VERSION,
            version: "1.2.3".to_owned(),
            release: "v1.2.3".to_owned(),
            notes: "native update fixture".to_owned(),
            pub_date: "2026-10-05T00:00:00Z".to_owned(),
            platforms,
        })
        .expect("更新清单应可序列化");

        assert_eq!(value["schema"], "keencode/native-update");
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["version"], "1.2.3");
        assert_eq!(value["release"], "v1.2.3");
        assert_eq!(value["notes"], "native update fixture");
        assert_eq!(value["pubDate"], "2026-10-05T00:00:00Z");
        assert_eq!(
            value["platforms"]["windows-x86_64"]["signature"],
            "fixture-signature"
        );
        assert!(value["platforms"]["windows-x86_64"]["url"].is_string());
    }

    #[test]
    fn accepts_release_version_shape() {
        assert!(validate_version("0.0.1").is_ok());
        assert!(validate_version("1.20.300").is_ok());
        assert!(validate_version("1.02.3").is_err());
        assert!(validate_version("1.2").is_err());
    }

    #[test]
    fn rejects_paths_in_asset_names() {
        assert!(validate_filename("KeenCode_v0.0.1_windows-x86_64.exe").is_ok());
        assert!(validate_filename("../secret").is_err());
        assert!(validate_filename("nested/file").is_err());
    }

    #[test]
    fn validates_github_repository_shape() {
        assert!(validate_repository("chengliang4810/keen-code").is_ok());
        assert!(validate_repository("owner").is_err());
        assert!(validate_repository("owner/other/repo").is_err());
    }
}
