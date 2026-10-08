//! Native 插件市场来源的受控物化。
//!
//! 这个模块把网络、Git 和归档处理限定在插件安装事务之前：下载正文有界，
//! 远程目录只能通过同文件系统 `rename` 发布，归档不会跟随链接或写出目标根。

use super::*;
use flate2::read::GzDecoder;
use std::io::{self, Write};
use std::time::Duration;
use tar::Archive;
use url::Url;
use zip::ZipArchive;

const MAX_MARKETPLACE_HTTP_BYTES: usize = 8 * 1024 * 1024;
const MAX_MARKETPLACE_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;
const MAX_MARKETPLACE_EXTRACTED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_MARKETPLACE_ARCHIVE_ENTRIES: usize = 4096;
const MARKETPLACE_HTTP_TIMEOUT: Duration = Duration::from_secs(60);
const MARKETPLACE_CACHE_COMPLETE: &str = ".rcode-complete";

/// 已经通过清单 Schema 校验的市场来源。
#[derive(Debug)]
pub struct MarketplaceMaterialization {
    root: PathBuf,
    manifest_path: PathBuf,
    manifest: MarketplaceManifest,
    /// 原始 marketplace 输入，随安装记录保存以支持后续更新。
    source: PluginInstallSource,
    /// 更新操作要求重新取得远程内容；首次安装允许复用完整缓存。
    refresh: bool,
}

impl MarketplaceMaterialization {
    pub fn manifest(&self) -> &MarketplaceManifest {
        &self.manifest
    }

    pub fn source(&self) -> &PluginInstallSource {
        &self.source
    }
}

pub fn validate_marketplace_source(source: &str) -> Result<Option<PathBuf>> {
    if source.len() > 8192 || source.chars().any(char::is_control) {
        return Err(PluginError::Invalid("市场来源过长或包含控制字符".into()));
    }
    let spec = parse_marketplace_source(source)?;
    match spec {
        MarketplaceSourceSpec::File(path) | MarketplaceSourceSpec::Directory(path) => {
            Ok(Some(fs::canonicalize(path)?))
        }
        _ => {
            if source.trim_start().starts_with('{') {
                let value: Value = serde_json::from_str(source)?;
                let object = value
                    .as_object()
                    .ok_or_else(|| PluginError::Invalid("市场来源无效".into()))?;
                if object.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "source"
                            | "url"
                            | "repo"
                            | "ref"
                            | "path"
                            | "sparsePaths"
                            | "package"
                            | "version"
                            | "registry"
                    )
                }) {
                    return Err(PluginError::Invalid("市场来源不接受凭据或额外字段".into()));
                }
            }
            Ok(None)
        }
    }
}

#[derive(Clone, Debug)]
enum MarketplaceSourceSpec {
    Url {
        url: String,
        headers: BTreeMap<String, String>,
    },
    Git {
        url: String,
        reference: Option<String>,
        path: Option<String>,
        sparse_paths: Vec<String>,
    },
    Npm {
        package: String,
        version: Option<String>,
        registry: Option<String>,
    },
    Pip {
        package: String,
        version: Option<String>,
        registry: Option<String>,
    },
    File(PathBuf),
    Directory(PathBuf),
}

#[derive(Clone, Debug)]
enum PluginSourceSpec {
    Relative(String),
    Git {
        url: String,
        path: Option<String>,
        reference: Option<String>,
        sha: Option<String>,
        sparse_paths: Vec<String>,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
    Npm {
        package: String,
        version: Option<String>,
        registry: Option<String>,
    },
    Pip {
        package: String,
        version: Option<String>,
        registry: Option<String>,
    },
}

/// 物化 Native 设置输入中的 marketplace，并在成功后返回可持久使用的目录。
pub fn materialize_marketplace_source(
    source: &str,
    cache_root: &Path,
) -> Result<MarketplaceMaterialization> {
    materialize_marketplace_source_with_mode(source, cache_root, false)
}

/// 强制重新取得 marketplace 及其插件来源；只有新内容完成校验后才替换旧缓存。
pub fn refresh_marketplace_source(
    source: &str,
    cache_root: &Path,
) -> Result<MarketplaceMaterialization> {
    materialize_marketplace_source_with_mode(source, cache_root, true)
}

fn materialize_marketplace_source_with_mode(
    source: &str,
    cache_root: &Path,
    refresh: bool,
) -> Result<MarketplaceMaterialization> {
    let source = source.trim().to_owned();
    let spec = parse_marketplace_source(&source)?;
    let persisted_source = match &spec {
        MarketplaceSourceSpec::File(path) | MarketplaceSourceSpec::Directory(path) => {
            fs::canonicalize(path)?.to_string_lossy().into_owned()
        }
        _ => source,
    };
    let mut materialized = match spec {
        MarketplaceSourceSpec::File(path) => load_local_marketplace(&path),
        MarketplaceSourceSpec::Directory(path) => load_local_marketplace(&path),
        MarketplaceSourceSpec::Url { url, headers } => {
            let key = cache_key("market-url", &url, &headers);
            let target = cache_directory(cache_root, &key, refresh, |staging| {
                let bytes = http_get(&url, &headers, MAX_MARKETPLACE_HTTP_BYTES, "市场清单")?;
                let manifest = super::parse_marketplace_manifest(&bytes)?;
                let path = staging.join(MARKETPLACE_MANIFEST);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(path, bytes)?;
                super::parse_marketplace_manifest(&serde_json::to_vec(&manifest)?)?;
                Ok(())
            })?;
            load_cached_marketplace(&target)
        }
        MarketplaceSourceSpec::Git {
            url,
            reference,
            path,
            sparse_paths,
        } => {
            let key = cache_key(
                "market-git",
                &url,
                &(reference.clone(), path.clone(), sparse_paths.clone()),
            );
            let target = cache_directory(cache_root, &key, refresh, |staging| {
                let repo = clone_git_source(
                    &url,
                    reference.as_deref(),
                    &sparse_paths,
                    staging,
                    "Git 市场来源",
                )?;
                let manifest_relative = path.as_deref().unwrap_or(MARKETPLACE_MANIFEST).to_owned();
                let manifest_relative = safe_relative_path(&manifest_relative, "市场清单路径")?;
                if !sparse_paths.is_empty() {
                    let mut checkout_paths = sparse_paths.clone();
                    let manifest_path = manifest_relative.to_string_lossy().replace('\\', "/");
                    if !checkout_paths.iter().any(|item| item == &manifest_path) {
                        checkout_paths.push(manifest_path);
                    }
                    apply_sparse_checkout(&repo, &checkout_paths, "Git 市场来源")?;
                }
                let manifest = canonical_child(&repo, &manifest_relative, "Git 市场清单")?;
                let bytes = super::read_limited(&manifest)?;
                super::parse_marketplace_manifest(&bytes)?;
                Ok(())
            })?;
            load_cached_marketplace(&target)
        }
        MarketplaceSourceSpec::Npm {
            package,
            version,
            registry,
        } => {
            let key = cache_key("market-npm", &package, &(version.clone(), registry.clone()));
            let target = cache_directory(cache_root, &key, refresh, |staging| {
                let archive = download_package_archive(
                    PackageKind::Npm,
                    &package,
                    version.as_deref(),
                    registry.as_deref(),
                )?;
                extract_archive(&archive, staging, "npm 市场归档")?;
                let (_root, _manifest) = locate_marketplace(staging)?;
                Ok(())
            })?;
            load_cached_marketplace(&target)
        }
        MarketplaceSourceSpec::Pip {
            package,
            version,
            registry,
        } => {
            let key = cache_key("market-pip", &package, &(version.clone(), registry.clone()));
            let target = cache_directory(cache_root, &key, refresh, |staging| {
                let archive = download_package_archive(
                    PackageKind::Pip,
                    &package,
                    version.as_deref(),
                    registry.as_deref(),
                )?;
                extract_archive(&archive, staging, "pip 市场归档")?;
                let (_root, _manifest) = locate_marketplace(staging)?;
                Ok(())
            })?;
            load_cached_marketplace(&target)
        }
    }?;
    materialized.source = PluginInstallSource::Marketplace {
        source: persisted_source,
    };
    materialized.refresh = refresh;
    Ok(materialized)
}

/// 只物化请求插件及其依赖，返回依赖优先的安装列表。
pub fn resolve_marketplace_plugin_install_plan(
    marketplace: &MarketplaceMaterialization,
    requested_plugin: Option<&str>,
    cache_root: &Path,
) -> Result<Vec<MaterializedPlugin>> {
    resolve_marketplace_plugin_install_plan_checked(
        marketplace,
        requested_plugin,
        cache_root,
        &|_| Ok(()),
    )
}

pub fn resolve_marketplace_plugin_install_plan_checked(
    marketplace: &MarketplaceMaterialization,
    requested_plugin: Option<&str>,
    cache_root: &Path,
    check: &dyn Fn(&Path) -> Result<()>,
) -> Result<Vec<MaterializedPlugin>> {
    let requested_name = requested_plugin
        .or_else(|| {
            (marketplace.manifest.plugins.len() == 1)
                .then(|| marketplace.manifest.plugins[0].name.as_str())
        })
        .ok_or_else(|| {
            PluginError::Invalid("市场包含多个插件，请使用 marketplace.json#插件名".to_owned())
        })?;
    let requested = PluginId::parse(requested_name)?.in_marketplace(&marketplace.manifest.name)?;
    let index = super::validated_marketplace_index(&requested, &marketplace.manifest)?;
    let raw_sources = load_raw_plugin_sources(&marketplace.manifest_path)?;
    let mut roots = BTreeMap::new();
    let mut manifests = BTreeMap::new();
    let mut pending = vec![index.requested.clone()];
    let mut visited = BTreeSet::new();

    while let Some(id) = pending.pop() {
        let key = super::marketplace_name_key(&id.plugin);
        if !visited.insert(key.clone()) {
            continue;
        }
        let entry = index.plugins.get(&key).ok_or_else(|| {
            PluginError::Invalid(format!(
                "市场 {} 中找不到插件 {}",
                index.marketplace_name, id.plugin
            ))
        })?;
        let raw_source = raw_sources.get(&key);
        let root = materialize_plugin_entry(
            entry,
            raw_source,
            &marketplace.root,
            &marketplace.manifest,
            cache_root,
            marketplace.refresh,
            check,
        )?;
        let manifest = super::load_plugin_manifest(&root)?;
        let mut dependencies = entry.dependencies.clone();
        dependencies.extend(manifest.dependencies.clone());
        for dependency in dependencies.keys() {
            let parsed = PluginId::parse(dependency)?;
            match parsed.marketplace.as_deref() {
                None => {
                    let dep = index
                        .plugins
                        .get(&super::marketplace_name_key(&parsed.plugin))
                        .map(|entry| entry.name.clone())
                        .unwrap_or(parsed.plugin);
                    pending.push(PluginId::from_components(
                        &dep,
                        Some(&index.marketplace_name),
                    )?);
                }
                Some(namespace) if namespace.eq_ignore_ascii_case(&index.marketplace_name) => {
                    let dep = index
                        .plugins
                        .get(&super::marketplace_name_key(&parsed.plugin))
                        .map(|entry| entry.name.clone())
                        .unwrap_or(parsed.plugin);
                    pending.push(PluginId::from_components(
                        &dep,
                        Some(&index.marketplace_name),
                    )?);
                }
                Some(_) => {
                    return Err(PluginError::Invalid(format!(
                        "跨市场依赖 {dependency} 需要由上层市场解析器提供"
                    )));
                }
            }
        }
        roots.insert(key.clone(), root);
        manifests.insert(key, manifest);
    }

    let order = super::dependency_closure(&index.requested, &marketplace.manifest, &manifests)?;
    order
        .into_iter()
        .map(|id| {
            let key = super::marketplace_name_key(&id.plugin);
            let entry = index
                .plugins
                .get(&key)
                .ok_or_else(|| PluginError::Invalid(format!("市场中找不到插件 {}", id.plugin)))?;
            let root = roots.get(&key).ok_or_else(|| {
                PluginError::Invalid(format!("插件 {} 的来源没有物化", id.plugin))
            })?;
            Ok(MaterializedPlugin {
                id: PluginId::from_components(&entry.name, Some(&index.marketplace_name))?,
                source_root: root.clone(),
                source: Some(marketplace.source.clone()),
            })
        })
        .collect()
}

fn parse_marketplace_source(source: &str) -> Result<MarketplaceSourceSpec> {
    let source = source.trim();
    if source.is_empty() {
        return Err(PluginError::Invalid("市场来源不能为空".to_owned()));
    }
    let path = PathBuf::from(source);
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() {
            return Err(PluginError::Invalid("市场来源不能是符号链接".to_owned()));
        }
        if metadata.is_file() {
            return Ok(MarketplaceSourceSpec::File(path));
        }
        if metadata.is_dir() {
            return Ok(MarketplaceSourceSpec::Directory(path));
        }
    }
    if source.starts_with('{') {
        return parse_marketplace_source_value(&serde_json::from_str(source)?);
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        return Ok(MarketplaceSourceSpec::Url {
            url: validate_http_url(source, "市场 URL")?,
            headers: BTreeMap::new(),
        });
    }
    if let Some(repo) = source.strip_prefix("github:") {
        let (repo, reference) = split_ref(repo, '@');
        return Ok(MarketplaceSourceSpec::Git {
            url: github_url(&repo)?,
            reference,
            path: None,
            sparse_paths: Vec::new(),
        });
    }
    if let Some(value) = source.strip_prefix("git:") {
        let (url, reference) = split_ref(value, '#');
        return Ok(MarketplaceSourceSpec::Git {
            url: validate_git_url(&url, "Git 市场来源")?,
            reference,
            path: None,
            sparse_paths: Vec::new(),
        });
    }
    if let Some(package) = source.strip_prefix("npm:") {
        let (package, version) = split_package_version(package);
        return Ok(MarketplaceSourceSpec::Npm {
            package: validate_package_name(&package, "npm 包名")?,
            version,
            registry: None,
        });
    }
    if source.matches('/').count() == 1
        && !source.starts_with('.')
        && !source.starts_with('/')
        && !source.starts_with('\\')
        && !source.starts_with('~')
        && !source.contains(char::is_whitespace)
    {
        let (repo, reference) = split_ref(source, '@');
        if repo.split('/').count() == 2 {
            return Ok(MarketplaceSourceSpec::Git {
                url: github_url(&repo)?,
                reference,
                path: None,
                sparse_paths: Vec::new(),
            });
        }
    }
    Err(PluginError::Invalid(format!("无法识别市场来源：{source}")))
}

fn parse_marketplace_source_value(value: &Value) -> Result<MarketplaceSourceSpec> {
    let object = value
        .as_object()
        .ok_or_else(|| PluginError::Invalid("市场 source 必须是对象".to_owned()))?;
    let source = object
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| PluginError::Invalid("市场 source 缺少 source 字段".to_owned()))?;
    match source {
        "url" => Ok(MarketplaceSourceSpec::Url {
            url: validate_http_url(&string_field(object, "url")?, "市场 URL")?,
            headers: parse_headers(object.get("headers"))?,
        }),
        "github" => Ok(MarketplaceSourceSpec::Git {
            url: github_url(&string_field(object, "repo")?)?,
            reference: optional_string(object, "ref")?,
            path: optional_string(object, "path")?,
            sparse_paths: string_array(object.get("sparsePaths"))?,
        }),
        "git" => Ok(MarketplaceSourceSpec::Git {
            url: validate_git_url(&string_field(object, "url")?, "Git 市场来源")?,
            reference: optional_string(object, "ref")?,
            path: optional_string(object, "path")?,
            sparse_paths: string_array(object.get("sparsePaths"))?,
        }),
        "npm" => Ok(MarketplaceSourceSpec::Npm {
            package: validate_package_name(&string_field(object, "package")?, "npm 包名")?,
            version: optional_string(object, "version")?,
            registry: optional_registry(object, "registry")?,
        }),
        "pip" => Ok(MarketplaceSourceSpec::Pip {
            package: validate_package_name(&string_field(object, "package")?, "pip 包名")?,
            version: optional_string(object, "version")?,
            registry: optional_registry(object, "registry")?,
        }),
        "file" => Ok(MarketplaceSourceSpec::File(PathBuf::from(string_field(
            object, "path",
        )?))),
        "directory" => Ok(MarketplaceSourceSpec::Directory(PathBuf::from(
            string_field(object, "path")?,
        ))),
        "settings" => Err(PluginError::Invalid(
            "Native 设置不允许从隐藏 settings 递归解析市场来源".to_owned(),
        )),
        other => Err(PluginError::Invalid(format!(
            "不支持的市场 source：{other}"
        ))),
    }
}

fn string_field(object: &Map<String, Value>, key: &str) -> Result<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| PluginError::Invalid(format!("市场 source 缺少非空 {key}")))
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .map(|value| Some(value.to_owned()))
            .ok_or_else(|| PluginError::Invalid(format!("市场 source.{key} 必须是非空字符串"))),
    }
}

fn string_array(value: Option<&Value>) -> Result<Vec<String>> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| !value.trim().is_empty())
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| {
                        PluginError::Invalid("sparsePaths 必须是非空字符串数组".to_owned())
                    })
            })
            .collect(),
        Some(_) => Err(PluginError::Invalid(
            "sparsePaths 必须是字符串数组".to_owned(),
        )),
    }
}

fn parse_headers(value: Option<&Value>) -> Result<BTreeMap<String, String>> {
    let Some(Value::Object(object)) = value else {
        return if value.is_none() || value == Some(&Value::Null) {
            Ok(BTreeMap::new())
        } else {
            Err(PluginError::Invalid("HTTP headers 必须是对象".to_owned()))
        };
    };
    let mut headers = BTreeMap::new();
    for (name, value) in object {
        let value = value
            .as_str()
            .ok_or_else(|| PluginError::Invalid(format!("HTTP header {name} 必须是字符串")))?;
        if name.trim().is_empty()
            || name.chars().any(char::is_control)
            || value.chars().any(char::is_control)
        {
            return Err(PluginError::Invalid(format!(
                "HTTP header {name} 含非法控制字符"
            )));
        }
        headers.insert(name.clone(), value.to_owned());
    }
    Ok(headers)
}

fn optional_registry(object: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    optional_string(object, key)?
        .map(|value| validate_http_url(&value, "registry URL"))
        .transpose()
}

fn load_local_marketplace(path: &Path) -> Result<MarketplaceMaterialization> {
    let canonical = fs::canonicalize(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(PluginError::Invalid("市场来源不能是符号链接".to_owned()));
    }
    if metadata.is_file() {
        let manifest = super::parse_marketplace_manifest(&super::read_limited(&canonical)?)?;
        return Ok(MarketplaceMaterialization {
            root: market_root_for_manifest(&canonical)?,
            manifest_path: canonical,
            manifest,
            source: PluginInstallSource::Marketplace {
                source: String::new(),
            },
            refresh: false,
        });
    }
    if !metadata.is_dir() {
        return Err(PluginError::Invalid(
            "市场来源必须是目录或普通文件".to_owned(),
        ));
    }
    load_cached_marketplace(&canonical)
}

fn load_cached_marketplace(root: &Path) -> Result<MarketplaceMaterialization> {
    let (manifest_path, market_root) = locate_marketplace(root)?;
    let manifest = super::load_marketplace_manifest(&market_root)?;
    Ok(MarketplaceMaterialization {
        root: market_root,
        manifest_path,
        manifest,
        source: PluginInstallSource::Marketplace {
            source: String::new(),
        },
        refresh: false,
    })
}

fn locate_marketplace(root: &Path) -> Result<(PathBuf, PathBuf)> {
    let root = fs::canonicalize(root)?;
    if !fs::symlink_metadata(&root)?.is_dir() {
        return Err(PluginError::Invalid("市场根目录必须是目录".to_owned()));
    }
    let direct = root.join(MARKETPLACE_MANIFEST);
    if is_regular_file(&direct)? {
        return Ok((direct, root));
    }
    let mut matches = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(PluginError::Invalid(format!(
                "市场归档不能包含符号链接：{}",
                path.display()
            )));
        }
        if metadata.is_dir() && is_regular_file(&path.join(MARKETPLACE_MANIFEST))? {
            matches.push(path);
        }
    }
    match matches.as_slice() {
        [path] => Ok((path.join(MARKETPLACE_MANIFEST), path.clone())),
        [] => Err(PluginError::Invalid(
            "市场来源缺少 .claude-plugin/marketplace.json".to_owned(),
        )),
        _ => Err(PluginError::Invalid(
            "市场归档包含多个 marketplace.json".to_owned(),
        )),
    }
}

fn market_root_for_manifest(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| PluginError::Invalid("市场清单缺少父目录".to_owned()))?;
    if parent.file_name().and_then(|name| name.to_str()) == Some(".claude-plugin") {
        return parent
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| PluginError::Invalid("市场清单缺少市场根目录".to_owned()));
    }
    Ok(parent.to_path_buf())
}

fn load_raw_plugin_sources(path: &Path) -> Result<BTreeMap<String, Value>> {
    let value: Value = serde_json::from_slice(&super::read_limited(path)?)?;
    let entries = value
        .get("plugins")
        .and_then(Value::as_array)
        .ok_or_else(|| PluginError::Invalid("市场 plugins 必须是数组".to_owned()))?;
    let mut result = BTreeMap::new();
    for entry in entries {
        let Some(object) = entry.as_object() else {
            continue;
        };
        let Some(name) = object.get("name").and_then(Value::as_str) else {
            continue;
        };
        if let Some(source) = object.get("source") {
            result.insert(super::marketplace_name_key(name), source.clone());
        }
    }
    Ok(result)
}

fn materialize_plugin_entry(
    entry: &MarketplacePlugin,
    raw_source: Option<&Value>,
    marketplace_root: &Path,
    marketplace: &MarketplaceManifest,
    cache_root: &Path,
    refresh: bool,
    check: &dyn Fn(&Path) -> Result<()>,
) -> Result<PathBuf> {
    let source = raw_source
        .map(parse_plugin_source_value)
        .transpose()?
        .unwrap_or_else(|| plugin_source_from_model(&entry.source));
    match source {
        PluginSourceSpec::Relative(path) => {
            let base = marketplace
                .metadata
                .get("pluginRoot")
                .and_then(Value::as_str)
                .map(|path| safe_relative_path(path, "市场 pluginRoot"))
                .transpose()?
                .map(|relative| canonical_child(marketplace_root, &relative, "市场 pluginRoot"))
                .transpose()?
                .unwrap_or_else(|| marketplace_root.to_path_buf());
            let root = canonical_child(
                &base,
                &safe_relative_path(&path, "插件相对路径")?,
                "市场插件路径",
            )?;
            validate_plugin_root(&root, &entry.name, check)?;
            Ok(root)
        }
        PluginSourceSpec::Git {
            url,
            path,
            reference,
            sha,
            sparse_paths,
        } => {
            let key = cache_key(
                "plugin-git",
                &url,
                &(
                    path.clone(),
                    reference.clone(),
                    sha.clone(),
                    sparse_paths.clone(),
                ),
            );
            let target = cache_directory(cache_root, &key, refresh, |staging| {
                let repo = clone_git_source(
                    &url,
                    reference.as_deref(),
                    &sparse_paths,
                    staging,
                    "Git 插件来源",
                )?;
                let mut checkout_paths = sparse_paths.clone();
                if let Some(path) = &path {
                    checkout_paths.push(path.clone());
                }
                apply_sparse_checkout(&repo, &checkout_paths, "Git 插件来源")?;
                if let Some(sha) = sha.as_deref() {
                    checkout_git_sha(&repo, sha, "Git 插件来源")?;
                }
                let root = match path.as_deref() {
                    Some(path) => canonical_child(
                        &repo,
                        &safe_relative_path(path, "Git 插件 path")?,
                        "Git 插件目录",
                    )?,
                    None => locate_plugin_root(&repo)?,
                };
                validate_plugin_root(&root, &entry.name, check)?;
                Ok(())
            })?;
            let repo = target.join("repo");
            let root = match path {
                Some(path) => canonical_child(
                    &repo,
                    &safe_relative_path(&path, "Git 插件 path")?,
                    "Git 插件目录",
                )?,
                None => locate_plugin_root(&repo)?,
            };
            validate_plugin_root(&root, &entry.name, check)?;
            Ok(root)
        }
        PluginSourceSpec::Http { url, headers } => {
            let key = cache_key("plugin-http", &url, &headers);
            let target = cache_directory(cache_root, &key, refresh, |staging| {
                let bytes = http_get(
                    &url,
                    &headers,
                    MAX_MARKETPLACE_ARCHIVE_BYTES,
                    "插件 HTTP 归档",
                )?;
                extract_archive(&bytes, staging, "插件 HTTP 归档")?;
                let root = locate_plugin_root(staging)?;
                validate_plugin_root(&root, "HTTP 归档", check)?;
                Ok(())
            })?;
            let root = locate_plugin_root(&target)?;
            validate_plugin_root(&root, &entry.name, check)?;
            Ok(root)
        }
        PluginSourceSpec::Npm {
            package,
            version,
            registry,
        } => {
            let key = cache_key("plugin-npm", &package, &(version.clone(), registry.clone()));
            let target = cache_directory(cache_root, &key, refresh, |staging| {
                let archive = download_package_archive(
                    PackageKind::Npm,
                    &package,
                    version.as_deref(),
                    registry.as_deref(),
                )?;
                extract_archive(&archive, staging, "npm 插件归档")?;
                let root = locate_plugin_root(staging)?;
                validate_plugin_root(&root, "npm 归档", check)?;
                Ok(())
            })?;
            let root = locate_plugin_root(&target)?;
            validate_plugin_root(&root, &entry.name, check)?;
            Ok(root)
        }
        PluginSourceSpec::Pip {
            package,
            version,
            registry,
        } => {
            let key = cache_key("plugin-pip", &package, &(version.clone(), registry.clone()));
            let target = cache_directory(cache_root, &key, refresh, |staging| {
                let archive = download_package_archive(
                    PackageKind::Pip,
                    &package,
                    version.as_deref(),
                    registry.as_deref(),
                )?;
                extract_archive(&archive, staging, "pip 插件归档")?;
                let root = locate_plugin_root(staging)?;
                validate_plugin_root(&root, "pip 归档", check)?;
                Ok(())
            })?;
            let root = locate_plugin_root(&target)?;
            validate_plugin_root(&root, &entry.name, check)?;
            Ok(root)
        }
    }
}

fn plugin_source_from_model(source: &PluginSource) -> PluginSourceSpec {
    match source {
        PluginSource::Relative { path } => PluginSourceSpec::Relative(path.clone()),
        PluginSource::Npm {
            package,
            version,
            registry,
        } => PluginSourceSpec::Npm {
            package: package.clone(),
            version: version.clone(),
            registry: registry.clone(),
        },
        PluginSource::Url {
            url,
            reference,
            sha,
        } => PluginSourceSpec::Git {
            url: url.clone(),
            path: None,
            reference: reference.clone(),
            sha: sha.clone(),
            sparse_paths: Vec::new(),
        },
        PluginSource::Github {
            repo,
            reference,
            sha,
        } => PluginSourceSpec::Git {
            url: format!("https://github.com/{repo}.git"),
            path: None,
            reference: reference.clone(),
            sha: sha.clone(),
            sparse_paths: Vec::new(),
        },
        PluginSource::GitSubdir {
            url,
            path,
            reference,
            sha,
        } => PluginSourceSpec::Git {
            url: url.clone(),
            path: Some(path.clone()),
            reference: reference.clone(),
            sha: sha.clone(),
            sparse_paths: Vec::new(),
        },
        PluginSource::Pip {
            package,
            version,
            registry,
        } => PluginSourceSpec::Pip {
            package: package.clone(),
            version: version.clone(),
            registry: registry.clone(),
        },
    }
}

fn parse_plugin_source_value(value: &Value) -> Result<PluginSourceSpec> {
    if let Some(path) = value.as_str() {
        return Ok(PluginSourceSpec::Relative(path.to_owned()));
    }
    let object = value
        .as_object()
        .ok_or_else(|| PluginError::Invalid("插件 source 必须是字符串或对象".to_owned()))?;
    let source = object
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| PluginError::Invalid("插件 source 缺少 source 字段".to_owned()))?;
    match source {
        "github" => Ok(PluginSourceSpec::Git {
            url: github_url(&string_field(object, "repo")?)?,
            path: optional_string(object, "path")?,
            reference: optional_string(object, "ref")?,
            sha: optional_string(object, "sha")?,
            sparse_paths: string_array(object.get("sparsePaths"))?,
        }),
        "git" | "git-subdir" => Ok(PluginSourceSpec::Git {
            url: validate_git_url(&string_field(object, "url")?, "Git 插件来源")?,
            path: if source == "git-subdir" {
                Some(string_field(object, "path")?)
            } else {
                optional_string(object, "path")?
            },
            reference: optional_string(object, "ref")?,
            sha: optional_string(object, "sha")?,
            sparse_paths: string_array(object.get("sparsePaths"))?,
        }),
        "url" => {
            let url = string_field(object, "url")?;
            let path = optional_string(object, "path")?;
            let headers = parse_headers(object.get("headers"))?;
            if path.is_none() && (!headers.is_empty() || looks_like_archive_url(&url)) {
                Ok(PluginSourceSpec::Http {
                    url: validate_http_url(&url, "插件 URL")?,
                    headers,
                })
            } else {
                Ok(PluginSourceSpec::Git {
                    url: validate_git_url(&url, "插件 Git URL")?,
                    path,
                    reference: optional_string(object, "ref")?,
                    sha: optional_string(object, "sha")?,
                    sparse_paths: string_array(object.get("sparsePaths"))?,
                })
            }
        }
        "npm" => Ok(PluginSourceSpec::Npm {
            package: validate_package_name(&string_field(object, "package")?, "npm 包名")?,
            version: optional_string(object, "version")?,
            registry: optional_registry(object, "registry")?,
        }),
        "pip" => Ok(PluginSourceSpec::Pip {
            package: validate_package_name(&string_field(object, "package")?, "pip 包名")?,
            version: optional_string(object, "version")?,
            registry: optional_registry(object, "registry")?,
        }),
        other => Err(PluginError::Invalid(format!(
            "不支持的插件 source：{other}"
        ))),
    }
}

fn cache_key<T: Serialize>(kind: &str, name: &str, options: &T) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(name.as_bytes());
    hasher.update([0]);
    hasher.update(serde_json::to_vec(options).unwrap_or_default());
    format!("{:x}", hasher.finalize())
}

fn cache_directory<F>(cache_root: &Path, key: &str, refresh: bool, build: F) -> Result<PathBuf>
where
    F: FnOnce(&Path) -> Result<()>,
{
    fs::create_dir_all(cache_root)?;
    let metadata = fs::symlink_metadata(cache_root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PluginError::Invalid(
            "marketplace 缓存根必须是普通目录".to_owned(),
        ));
    }
    let target = cache_root.join(key);
    if let Ok(metadata) = fs::symlink_metadata(&target) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(PluginError::Invalid(
                "marketplace 缓存目标必须是普通目录".to_owned(),
            ));
        }
        if !refresh && is_regular_file(&target.join(MARKETPLACE_CACHE_COMPLETE))? {
            return Ok(target);
        }
        if !refresh {
            fs::remove_dir_all(&target)?;
        }
    }
    let staging = tempfile::Builder::new()
        .prefix(".partial-marketplace-")
        .tempdir_in(cache_root)?;
    build(staging.path())?;
    fs::write(staging.path().join(MARKETPLACE_CACHE_COMPLETE), b"")?;
    if refresh && fs::symlink_metadata(&target).is_ok() {
        let backup = target.with_extension("previous");
        if let Ok(metadata) = fs::symlink_metadata(&backup) {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(PluginError::Invalid(
                    "marketplace 旧缓存替换目录无效".to_owned(),
                ));
            }
            fs::remove_dir_all(&backup)?;
        }
        fs::rename(&target, &backup)?;
        if let Err(error) = fs::rename(staging.path(), &target) {
            let _ = fs::rename(&backup, &target);
            return Err(PluginError::Io(error));
        }
        let _ = fs::remove_dir_all(backup);
    } else if let Err(error) = fs::rename(staging.path(), &target) {
        if error.kind() == io::ErrorKind::AlreadyExists
            && is_regular_file(&target.join(MARKETPLACE_CACHE_COMPLETE))?
        {
            return Ok(target);
        }
        return Err(PluginError::Io(error));
    }
    Ok(target)
}

fn http_get(
    url: &str,
    headers: &BTreeMap<String, String>,
    max_bytes: usize,
    label: &str,
) -> Result<Vec<u8>> {
    let url = validate_http_url(url, label)?;
    let has_headers = !headers.is_empty();
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5
                || validate_http_url(attempt.url().as_str(), "市场重定向").is_err()
            {
                attempt.error("市场重定向地址无效")
            } else if attempt
                .previous()
                .last()
                .is_some_and(|previous| previous.origin() != attempt.url().origin())
                && has_headers
            {
                attempt.error("带请求头的市场来源不允许跨域重定向")
            } else {
                attempt.follow()
            }
        }))
        .connect_timeout(MARKETPLACE_HTTP_TIMEOUT)
        .timeout(MARKETPLACE_HTTP_TIMEOUT)
        .user_agent("RCode/0.0.1 plugin-marketplace")
        .build()
        .map_err(|error| PluginError::Invalid(format!("创建{label}客户端失败：{error}")))?;
    let mut request = client.get(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .map_err(|error| PluginError::Invalid(format!("下载{label}失败：{error}")))?
        .error_for_status()
        .map_err(|error| PluginError::Invalid(format!("下载{label}返回错误：{error}")))?;
    match crate::http_response::read_http_response_limited(response, max_bytes) {
        Ok(bytes) => Ok(bytes),
        Err(crate::http_response::HttpResponseReadError::TooLarge { max_bytes }) => Err(
            PluginError::Invalid(format!("{label}超过 {max_bytes} 字节限制")),
        ),
        Err(crate::http_response::HttpResponseReadError::Read(error)) => {
            Err(PluginError::Invalid(format!("读取{label}失败：{error}")))
        }
    }
}

fn clone_git_source(
    url: &str,
    reference: Option<&str>,
    sparse_paths: &[String],
    staging: &Path,
    label: &str,
) -> Result<PathBuf> {
    let url = validate_git_url(url, label)?;
    if let Some(reference) = reference {
        validate_git_reference(reference, label)?;
    }
    let repo = staging.join("repo");
    let repo_string = repo
        .to_str()
        .ok_or_else(|| PluginError::Invalid(format!("{label}路径不是有效 UTF-8")))?;
    let args = vec![
        "clone".to_owned(),
        "--depth".to_owned(),
        "1".to_owned(),
        "--no-checkout".to_owned(),
        "--".to_owned(),
        url,
        repo_string.to_owned(),
    ];
    run_git(staging, &args, label)?;
    if let Some(reference) = reference {
        run_git(
            &repo,
            &[
                "fetch".to_owned(),
                "--depth".to_owned(),
                "1".to_owned(),
                "origin".to_owned(),
                reference.to_owned(),
            ],
            label,
        )?;
        run_git(
            &repo,
            &[
                "checkout".to_owned(),
                "--detach".to_owned(),
                "FETCH_HEAD".to_owned(),
            ],
            label,
        )?;
    } else {
        run_git(
            &repo,
            &[
                "checkout".to_owned(),
                "--detach".to_owned(),
                "HEAD".to_owned(),
            ],
            label,
        )?;
    }
    if !sparse_paths.is_empty() {
        apply_sparse_checkout(&repo, sparse_paths, label)?;
    }
    Ok(repo)
}

fn checkout_git_sha(root: &Path, sha: &str, label: &str) -> Result<()> {
    if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(PluginError::Invalid(format!(
            "{label} sha 必须是 40 位十六进制"
        )));
    }
    run_git(
        root,
        &[
            "fetch".to_owned(),
            "--depth".to_owned(),
            "1".to_owned(),
            "origin".to_owned(),
            sha.to_owned(),
        ],
        label,
    )?;
    run_git(
        root,
        &["checkout".to_owned(), "--detach".to_owned(), sha.to_owned()],
        label,
    )
}

fn apply_sparse_checkout(root: &Path, paths: &[String], label: &str) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec![
        "sparse-checkout".to_owned(),
        "set".to_owned(),
        "--no-cone".to_owned(),
    ];
    for path in paths {
        let relative = safe_relative_path(path, "Git sparsePaths")?;
        args.push(format!(
            "/{}",
            relative
                .to_string_lossy()
                .replace('\\', "/")
                .trim_start_matches('/')
        ));
    }
    run_git(root, &args, label)
}

fn run_git(root: &Path, args: &[String], label: &str) -> Result<()> {
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = crate::workspace::run_git_with_timeout(root, &refs)
        .map_err(|error| PluginError::Invalid(format!("{label}执行 git 失败：{error}")))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim().chars().take(1_000).collect::<String>();
    Err(PluginError::Invalid(format!(
        "{label} git 返回失败：{detail}"
    )))
}

fn safe_relative_path(value: &str, label: &str) -> Result<PathBuf> {
    let value = value.trim();
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::Prefix(_)
                    | std::path::Component::RootDir
            )
        })
    {
        return Err(PluginError::Invalid(format!(
            "{label}必须是安全相对路径：{value}"
        )));
    }
    Ok(path.to_path_buf())
}

fn canonical_child(root: &Path, relative: &Path, label: &str) -> Result<PathBuf> {
    let root = fs::canonicalize(root)?;
    let mut current = root.clone();
    for component in relative.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(name) => current.push(name),
            _ => return Err(PluginError::Invalid(format!("{label}路径越界"))),
        }
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            PluginError::Invalid(format!("读取{label}失败 {}：{error}", current.display()))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(PluginError::Invalid(format!("{label}不能包含符号链接")));
        }
    }
    let candidate = fs::canonicalize(current)?;
    if !candidate.starts_with(&root) {
        return Err(PluginError::Invalid(format!("{label}越出根目录")));
    }
    Ok(candidate)
}

fn validate_plugin_root(
    root: &Path,
    label: &str,
    check: &dyn Fn(&Path) -> Result<()>,
) -> Result<()> {
    check(root)?;
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PluginError::Invalid(format!("市场插件 {label} 根目录无效")));
    }
    let manifest = root.join(PLUGIN_MANIFEST);
    if !is_regular_file(&manifest)? {
        return Err(PluginError::Invalid(format!(
            "市场插件 {label} 缺少 {PLUGIN_MANIFEST}"
        )));
    }
    super::load_plugin_manifest(root)?;
    Ok(())
}

fn locate_plugin_root(root: &Path) -> Result<PathBuf> {
    let root = fs::canonicalize(root)?;
    if is_regular_file(&root.join(PLUGIN_MANIFEST))? {
        return Ok(root);
    }
    let mut matches = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(PluginError::Invalid(format!(
                "插件归档不能包含符号链接：{}",
                path.display()
            )));
        }
        if metadata.is_dir() && is_regular_file(&path.join(PLUGIN_MANIFEST))? {
            matches.push(path);
        }
    }
    match matches.as_slice() {
        [path] => Ok(fs::canonicalize(path)?),
        [] => Err(PluginError::Invalid(
            "插件来源缺少 .claude-plugin/plugin.json".to_owned(),
        )),
        _ => Err(PluginError::Invalid(
            "插件来源包含多个插件根目录".to_owned(),
        )),
    }
}

fn is_regular_file(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_file() && !metadata.file_type().is_symlink()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[derive(Clone, Copy)]
enum PackageKind {
    Npm,
    Pip,
}

fn download_package_archive(
    kind: PackageKind,
    package: &str,
    requested_version: Option<&str>,
    registry: Option<&str>,
) -> Result<Vec<u8>> {
    match kind {
        PackageKind::Npm => download_npm_archive(package, requested_version, registry),
        PackageKind::Pip => download_pip_archive(package, requested_version, registry),
    }
}

fn download_npm_archive(
    package: &str,
    requested_version: Option<&str>,
    registry: Option<&str>,
) -> Result<Vec<u8>> {
    let registry = validate_http_url(
        registry.unwrap_or("https://registry.npmjs.org"),
        "npm registry URL",
    )?;
    let mut endpoint = Url::parse(&format!("{}/", registry.trim_end_matches('/')))
        .map_err(|error| PluginError::Invalid(format!("npm registry URL 无效：{error}")))?;
    let mut segments = endpoint
        .path_segments_mut()
        .map_err(|_| PluginError::Invalid("npm registry URL 不能包含查询参数".to_owned()))?;
    if let Some((scope, name)) = package
        .strip_prefix('@')
        .and_then(|value| value.split_once('/'))
    {
        let scope = format!("@{scope}");
        segments.push(&scope).push(name);
    } else {
        segments.push(package);
    }
    drop(segments);
    let document = serde_json::from_slice::<Value>(&http_get(
        endpoint.as_str(),
        &BTreeMap::new(),
        MAX_MARKETPLACE_HTTP_BYTES,
        "npm package metadata",
    )?)?;
    let versions = document
        .get("versions")
        .and_then(Value::as_object)
        .ok_or_else(|| PluginError::Invalid("npm registry 返回缺少 versions".to_owned()))?;
    let version = select_npm_version(&document, versions, requested_version)?;
    let tarball = versions
        .get(&version)
        .and_then(|value| value.get("dist"))
        .and_then(|value| value.get("tarball"))
        .and_then(Value::as_str)
        .ok_or_else(|| PluginError::Invalid(format!("npm 包 {package}@{version} 缺少 tarball")))?;
    http_get(
        tarball,
        &BTreeMap::new(),
        MAX_MARKETPLACE_ARCHIVE_BYTES,
        "npm 包归档",
    )
}

fn select_npm_version(
    document: &Value,
    versions: &Map<String, Value>,
    requested: Option<&str>,
) -> Result<String> {
    if let Some(requested) = requested.map(str::trim).filter(|value| !value.is_empty()) {
        if versions.contains_key(requested) {
            return Ok(requested.to_owned());
        }
        if let Some(tag) = document
            .get("dist-tags")
            .and_then(Value::as_object)
            .and_then(|tags| tags.get(requested))
            .and_then(Value::as_str)
            && versions.contains_key(tag)
        {
            return Ok(tag.to_owned());
        }
        return select_npm_range(versions, requested)
            .ok_or_else(|| PluginError::Invalid(format!("npm 版本要求无法解析：{requested}")));
    }
    document
        .get("dist-tags")
        .and_then(Value::as_object)
        .and_then(|tags| tags.get("latest"))
        .and_then(Value::as_str)
        .filter(|version| versions.contains_key(*version))
        .map(ToOwned::to_owned)
        .or_else(|| versions.keys().max().cloned())
        .ok_or_else(|| PluginError::Invalid("npm 包没有可用版本".to_owned()))
}

fn select_npm_range(versions: &Map<String, Value>, request: &str) -> Option<String> {
    let (mode, version) = if let Some(value) = request.strip_prefix('^') {
        ('^', value)
    } else if let Some(value) = request.strip_prefix('~') {
        ('~', value)
    } else if request == "*" {
        ('*', "0.0.0")
    } else {
        return None;
    };
    let base = parse_version(version)?;
    versions
        .keys()
        .filter_map(|candidate| parse_version(candidate).map(|value| (candidate, value)))
        .filter(|(_, value)| match mode {
            '*' => true,
            '^' => value.0 == base.0 && *value >= base,
            '~' => value.0 == base.0 && value.1 == base.1 && *value >= base,
            _ => false,
        })
        .max_by_key(|(_, value)| *value)
        .map(|(candidate, _)| candidate.clone())
}

fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.strip_prefix('v').unwrap_or(value);
    let mut parts = value.split(['.', '-']);
    Some((
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ))
}

fn download_pip_archive(
    package: &str,
    requested_version: Option<&str>,
    registry: Option<&str>,
) -> Result<Vec<u8>> {
    let registry = validate_http_url(registry.unwrap_or("https://pypi.org"), "PyPI registry URL")?;
    let base = registry.trim_end_matches('/').trim_end_matches("/simple");
    let endpoint = format!("{base}/pypi/{package}/json");
    let document = serde_json::from_slice::<Value>(&http_get(
        &endpoint,
        &BTreeMap::new(),
        MAX_MARKETPLACE_HTTP_BYTES,
        "pip package metadata",
    )?)?;
    let version = requested_version
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            document
                .get("info")
                .and_then(|info| info.get("version"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .ok_or_else(|| PluginError::Invalid("PyPI metadata 缺少版本".to_owned()))?;
    let releases = document
        .get("releases")
        .and_then(Value::as_object)
        .and_then(|releases| releases.get(&version))
        .and_then(Value::as_array)
        .ok_or_else(|| PluginError::Invalid(format!("PyPI 包 {package}@{version} 不存在")))?;
    let url = releases
        .iter()
        .filter(|file| {
            matches!(
                file.get("packagetype").and_then(Value::as_str),
                Some("sdist") | Some("bdist_wheel")
            )
        })
        .find_map(|file| file.get("url").and_then(Value::as_str))
        .ok_or_else(|| PluginError::Invalid(format!("PyPI 包 {package}@{version} 没有可用归档")))?;
    http_get(
        url,
        &BTreeMap::new(),
        MAX_MARKETPLACE_ARCHIVE_BYTES,
        "pip 包归档",
    )
}

fn extract_archive(bytes: &[u8], target: &Path, label: &str) -> Result<()> {
    if bytes.len() > MAX_MARKETPLACE_ARCHIVE_BYTES {
        return Err(PluginError::Invalid(format!("{label}超过大小限制")));
    }
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        return extract_zip(bytes, target, label);
    }
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let stream_limit = MAX_MARKETPLACE_EXTRACTED_BYTES
            .saturating_add((MAX_MARKETPLACE_ARCHIVE_ENTRIES as u64).saturating_mul(1024))
            .saturating_add(1024);
        return extract_tar(
            target,
            BoundedReader::new(GzDecoder::new(io::Cursor::new(bytes)), stream_limit),
            label,
        );
    }
    if bytes.len() >= 262 && &bytes[257..262] == b"ustar" {
        return extract_tar(target, io::Cursor::new(bytes), label);
    }
    Err(PluginError::Invalid(format!(
        "{label}格式必须是 ZIP、TAR 或 TAR.GZ"
    )))
}

/// 限制压缩 tar 解码后的读取总量，避免归档尾部或元数据无限膨胀。
struct BoundedReader<R> {
    inner: R,
    remaining: u64,
}

impl<R> BoundedReader<R> {
    fn new(inner: R, limit: u64) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
}

impl<R: io::Read> io::Read for BoundedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "归档解码流超过安全限制",
            ));
        }
        let length = buffer.len().min(self.remaining as usize);
        let read = self.inner.read(&mut buffer[..length])?;
        self.remaining = self.remaining.saturating_sub(read as u64);
        Ok(read)
    }
}

fn extract_zip(bytes: &[u8], target: &Path, label: &str) -> Result<()> {
    let mut archive = ZipArchive::new(io::Cursor::new(bytes))
        .map_err(|error| PluginError::Invalid(format!("读取{label}失败：{error}")))?;
    if archive.len() > MAX_MARKETPLACE_ARCHIVE_ENTRIES {
        return Err(PluginError::Invalid(format!("{label}条目数超过限制")));
    }
    let mut total = 0_u64;
    let mut seen = BTreeSet::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| PluginError::Invalid(format!("读取{label}条目失败：{error}")))?;
        if entry.is_symlink() {
            return Err(PluginError::Invalid(format!("{label}不允许符号链接")));
        }
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| PluginError::Invalid(format!("{label}条目路径越界：{}", entry.name())))?
            .to_path_buf();
        if !seen.insert(relative.clone()) {
            return Err(PluginError::Invalid(format!("{label}包含重复条目")));
        }
        let destination = target.join(relative);
        if entry.is_dir() {
            ensure_directory(target, &destination, label)?;
            continue;
        }
        let size = entry.size();
        total = total
            .checked_add(size)
            .ok_or_else(|| PluginError::Invalid(format!("{label}解包大小溢出")))?;
        if total > MAX_MARKETPLACE_EXTRACTED_BYTES {
            return Err(PluginError::Invalid(format!("{label}解包后超过大小限制")));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| PluginError::Invalid(format!("{label}条目缺少父目录")))?;
        ensure_directory(target, parent, label)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        let copied = io::copy(
            &mut entry.by_ref().take(size.saturating_add(1)),
            &mut output,
        )?;
        if copied != size {
            return Err(PluginError::Invalid(format!("{label}条目大小不一致")));
        }
        output.flush()?;
    }
    Ok(())
}

fn extract_tar<R: io::Read>(target: &Path, reader: R, label: &str) -> Result<()> {
    let mut archive = Archive::new(reader);
    let mut entries = 0_usize;
    let mut total = 0_u64;
    let mut seen = BTreeSet::new();
    for item in archive
        .entries()
        .map_err(|error| PluginError::Invalid(format!("读取{label}失败：{error}")))?
    {
        entries += 1;
        if entries > MAX_MARKETPLACE_ARCHIVE_ENTRIES {
            return Err(PluginError::Invalid(format!("{label}条目数超过限制")));
        }
        let mut entry =
            item.map_err(|error| PluginError::Invalid(format!("读取{label}条目失败：{error}")))?;
        let relative = entry
            .path()
            .map_err(|error| PluginError::Invalid(format!("读取{label}路径失败：{error}")))?
            .to_path_buf();
        let relative = safe_archive_path(&relative, label)?;
        if !seen.insert(relative.clone()) {
            return Err(PluginError::Invalid(format!("{label}包含重复条目")));
        }
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            ensure_directory(target, &target.join(relative), label)?;
            continue;
        }
        if !kind.is_file() {
            return Err(PluginError::Invalid(format!("{label}包含特殊文件")));
        }
        let size = entry.size();
        total = total
            .checked_add(size)
            .ok_or_else(|| PluginError::Invalid(format!("{label}解包大小溢出")))?;
        if total > MAX_MARKETPLACE_EXTRACTED_BYTES {
            return Err(PluginError::Invalid(format!("{label}解包后超过大小限制")));
        }
        let destination = target.join(relative);
        let parent = destination
            .parent()
            .ok_or_else(|| PluginError::Invalid(format!("{label}条目缺少父目录")))?;
        ensure_directory(target, parent, label)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        let copied = io::copy(&mut (&mut entry).take(size.saturating_add(1)), &mut output)?;
        if copied != size {
            return Err(PluginError::Invalid(format!("{label}条目大小不一致")));
        }
        output.flush()?;
    }
    Ok(())
}

fn safe_archive_path(path: &Path, label: &str) -> Result<PathBuf> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(value) => result.push(value),
            std::path::Component::CurDir => {}
            _ => return Err(PluginError::Invalid(format!("{label}条目路径越界"))),
        }
    }
    if result.as_os_str().is_empty() {
        return Err(PluginError::Invalid(format!("{label}条目路径为空")));
    }
    Ok(result)
}

fn ensure_directory(root: &Path, path: &Path, label: &str) -> Result<()> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| PluginError::Invalid(format!("{label}目录越出解包根")))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(PluginError::Invalid(format!("{label}目录路径无效")));
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(PluginError::Invalid(format!(
                    "{label}目录不能是符号链接或文件"
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&current)?,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn validate_http_url(value: &str, label: &str) -> Result<String> {
    let parsed =
        Url::parse(value).map_err(|error| PluginError::Invalid(format!("{label}无效：{error}")))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || invalid_marketplace_host(&parsed)
    {
        return Err(PluginError::Invalid(format!("{label}只允许 HTTP(S)")));
    }
    Ok(value.to_owned())
}

fn invalid_marketplace_host(url: &Url) -> bool {
    use std::net::IpAddr;
    let host = url.host_str().unwrap_or_default();
    matches!(
        host.to_ascii_lowercase().as_str(),
        "metadata" | "metadata.google.internal" | "metadata.azure.com"
    ) || host
        .trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .is_ok_and(|ip| match ip {
            IpAddr::V4(ip) => ip.is_link_local() || ip.is_unspecified() || ip.is_multicast(),
            IpAddr::V6(ip) => {
                ip.is_unicast_link_local()
                    || ip.is_unspecified()
                    || ip.is_multicast()
                    || ip.segments()[0..2] == [0xfd00, 0xec2]
            }
        })
}

fn validate_git_url(value: &str, label: &str) -> Result<String> {
    if value.is_empty() || value.starts_with('-') || value.chars().any(char::is_control) {
        return Err(PluginError::Invalid(format!("{label} Git URL 无效")));
    }
    if value.starts_with("git@") && value.split_once(':').is_some() {
        return Ok(value.to_owned());
    }
    let parsed = Url::parse(value)
        .map_err(|error| PluginError::Invalid(format!("{label} Git URL 无效：{error}")))?;
    if matches!(parsed.scheme(), "http" | "https" | "ssh")
        && parsed.host_str().is_some()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && (parsed.username().is_empty() || parsed.scheme() == "ssh" && parsed.username() == "git")
        && !invalid_marketplace_host(&parsed)
    {
        return Ok(value.to_owned());
    }
    Err(PluginError::Invalid(format!(
        "{label} Git URL 只允许 HTTP/HTTPS/SSH"
    )))
}

fn validate_git_reference(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.starts_with('-')
        || value.contains("..")
        || value
            .chars()
            .any(|char| char.is_whitespace() || char.is_control())
    {
        return Err(PluginError::Invalid(format!("{label} Git ref 无效")));
    }
    Ok(())
}

fn validate_package_name(value: &str, label: &str) -> Result<String> {
    if value.trim().is_empty()
        || value
            .chars()
            .any(|char| char.is_whitespace() || char.is_control())
        || value.contains(';')
    {
        return Err(PluginError::Invalid(format!("{label}无效")));
    }
    Ok(value.to_owned())
}

fn github_url(repo: &str) -> Result<String> {
    let mut parts = repo.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        return Err(PluginError::Invalid(format!(
            "GitHub repo 必须是 owner/repo：{repo}"
        )));
    }
    Ok(format!("https://github.com/{repo}.git"))
}

fn split_ref(value: &str, separator: char) -> (String, Option<String>) {
    match value.rsplit_once(separator) {
        Some((base, reference)) if !base.is_empty() && !reference.is_empty() => {
            (base.to_owned(), Some(reference.to_owned()))
        }
        _ => (value.to_owned(), None),
    }
}

fn split_package_version(value: &str) -> (String, Option<String>) {
    if value.starts_with('@') {
        value
            .rsplit_once('@')
            .filter(|(package, version)| package.contains('/') && !version.is_empty())
            .map(|(package, version)| (package.to_owned(), Some(version.to_owned())))
            .unwrap_or_else(|| (value.to_owned(), None))
    } else {
        value
            .rsplit_once('@')
            .filter(|(package, version)| !package.is_empty() && !version.is_empty())
            .map(|(package, version)| (package.to_owned(), Some(version.to_owned())))
            .unwrap_or_else(|| (value.to_owned(), None))
    }
}

fn looks_like_archive_url(value: &str) -> bool {
    let value = value
        .split(['?', '#'])
        .next()
        .unwrap_or(value)
        .to_ascii_lowercase();
    [".zip", ".tar", ".tar.gz", ".tgz", ".whl"]
        .iter()
        .any(|suffix| value.ends_with(suffix))
}
