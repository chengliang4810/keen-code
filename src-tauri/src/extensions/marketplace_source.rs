//! 插件市场来源解析、取得、归档解包与临时目录生命周期实现。

use super::*;

/// Claude marketplace 插件来源的完整本地表示；额外字段不能在 `PluginSource` 归一化时丢失。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MarketplacePluginSourceSpec {
    /// 市场仓库内的相对插件目录。
    Relative { path: String },
    /// Git 仓库来源及可选子目录、稀疏路径和固定版本。
    Git {
        url: String,
        path: Option<String>,
        reference: Option<String>,
        sha: Option<String>,
        sparse_paths: Vec<String>,
    },
    /// npm 包来源。
    Npm {
        package: String,
        version: Option<String>,
        registry: Option<String>,
    },
    /// Claude schema 允许声明 pip；当前官方加载器也会拒绝实际安装，因此保留字段后给出明确错误。
    Pip {
        package: String,
        version: Option<String>,
        registry: Option<String>,
    },
    /// 带请求头的 HTTP 归档来源；主要用于兼容私有 URL 扩展。
    HttpArchive {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

/// 读取 Claude marketplace 原始 JSON 中某个插件的 source，保留 `path`、`sparsePaths`、headers 等未知字段。
pub(super) fn load_raw_marketplace_plugin_source(
    marketplace_manifest: &Path,
    plugin_name: &str,
) -> Result<Option<Value>, String> {
    let bytes =
        fs::read(marketplace_manifest).map_err(|error| format!("无法读取市场清单：{error}"))?;
    if bytes.len() as u64 > MAX_EXTENSION_FILE_BYTES {
        return Err(format!("市场清单超过 {MAX_EXTENSION_FILE_BYTES} 字节"));
    }
    let document = serde_json::from_slice::<Value>(&bytes)
        .map_err(|error| format!("市场清单 JSON 格式无效：{error}"))?;
    let Some(plugins) = document.get("plugins").and_then(Value::as_array) else {
        return Ok(None);
    };
    Ok(plugins
        .iter()
        .find(|entry| {
            entry
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name == plugin_name)
        })
        .and_then(|entry| entry.get("source").cloned()))
}

/// 将原始 marketplace source 解析为不会丢失关键固定字段的来源计划。
pub(super) fn parse_marketplace_plugin_source(
    value: Value,
) -> Result<MarketplacePluginSourceSpec, String> {
    let Value::String(path) = value else {
        let object = value
            .as_object()
            .ok_or_else(|| "插件 source 必须是字符串或对象".to_owned())?;
        let source = object
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| "插件 source 缺少 source 字段".to_owned())?;
        let optional_text = |key: &str| -> Result<Option<String>, String> {
            match object.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
                Some(_) => Err(format!("插件 source.{key} 必须是非空字符串")),
            }
        };
        let sparse_paths = match object.get("sparsePaths") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(paths)) => paths
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .filter(|path| !path.trim().is_empty())
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| "插件 source.sparsePaths 必须是非空字符串数组".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => return Err("插件 source.sparsePaths 必须是字符串数组".to_owned()),
        };
        let path = optional_text("path")?;
        return match source {
            "github" => {
                let repo = object
                    .get("repo")
                    .and_then(Value::as_str)
                    .filter(|repo| !repo.trim().is_empty())
                    .ok_or_else(|| "github 插件 source 缺少 repo".to_owned())?;
                let url = format!("https://github.com/{repo}.git");
                Ok(MarketplacePluginSourceSpec::Git {
                    url,
                    path,
                    reference: optional_text("ref")?,
                    sha: optional_text("sha")?,
                    sparse_paths,
                })
            }
            "url" | "git" | "git-subdir" => {
                let url = object
                    .get("url")
                    .and_then(Value::as_str)
                    .filter(|url| !url.trim().is_empty())
                    .ok_or_else(|| format!("{source} 插件 source 缺少 url"))?;
                let headers = parse_http_headers(object.get("headers"))?;
                if source == "url" && !headers.is_empty() && path.is_none() {
                    return Ok(MarketplacePluginSourceSpec::HttpArchive {
                        url: url.to_owned(),
                        headers,
                    });
                }
                Ok(MarketplacePluginSourceSpec::Git {
                    url: url.to_owned(),
                    path: if source == "git-subdir" {
                        Some(path.ok_or_else(|| "git-subdir 插件 source 缺少 path".to_owned())?)
                    } else {
                        path
                    },
                    reference: optional_text("ref")?,
                    sha: optional_text("sha")?,
                    sparse_paths,
                })
            }
            "npm" => Ok(MarketplacePluginSourceSpec::Npm {
                package: object
                    .get("package")
                    .and_then(Value::as_str)
                    .filter(|package| !package.trim().is_empty())
                    .ok_or_else(|| "npm 插件 source 缺少 package".to_owned())?
                    .to_owned(),
                version: optional_text("version")?,
                registry: optional_text("registry")?
                    .map(|registry| validate_http_source_url(&registry, "npm registry URL"))
                    .transpose()?,
            }),
            "pip" => Ok(MarketplacePluginSourceSpec::Pip {
                package: object
                    .get("package")
                    .and_then(Value::as_str)
                    .filter(|package| !package.trim().is_empty())
                    .ok_or_else(|| "pip 插件 source 缺少 package".to_owned())?
                    .to_owned(),
                version: optional_text("version")?,
                registry: optional_text("registry")?
                    .map(|registry| validate_http_source_url(&registry, "pip registry URL"))
                    .transpose()?,
            }),
            other => Err(format!("不支持的插件 source：{other}")),
        };
    };
    Ok(MarketplacePluginSourceSpec::Relative { path })
}

/// 解析 URL source 的 HTTP headers，并拒绝换行等会污染请求的值。
pub(super) fn parse_http_headers(
    value: Option<&Value>,
) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| "URL source headers 必须是对象".to_owned())?;
    let mut headers = BTreeMap::new();
    for (name, value) in object {
        let value = value
            .as_str()
            .ok_or_else(|| format!("URL source header {name} 必须是字符串"))?;
        if name.trim().is_empty()
            || name.bytes().any(|byte| byte.is_ascii_control())
            || value.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(format!("URL source header {name} 包含非法控制字符"));
        }
        headers.insert(name.clone(), interpolate_source_header(value)?);
    }
    Ok(headers)
}

/// 使用当前环境变量插值 `${NAME}`，不把环境变量值写入错误文本。
pub(super) fn interpolate_source_header(value: &str) -> Result<String, String> {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| "URL source header 变量缺少闭合 }".to_owned())?;
        let name = &after[..end];
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err("URL source header 变量名无效".to_owned());
        }
        let value =
            env::var(name).map_err(|_| format!("URL source header 缺少环境变量：{name}"))?;
        output.push_str(&value);
        rest = &after[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

/// 安装来源的实际物化目录；本地来源直接复用，远程来源使用系统工具取得后校验。
pub(super) fn materialize_claude_source(
    source: &str,
    workspace: &Path,
) -> Result<(PathBuf, Option<String>), String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("插件来源不能为空".to_owned());
    }
    let expanded = expand_tilde(source)?;
    if expanded.exists() {
        let canonical = fs::canonicalize(&expanded)
            .map_err(|error| format!("无法访问插件来源 {}：{error}", expanded.display()))?;
        let root = if canonical.is_file() {
            canonical
                .parent()
                .ok_or_else(|| "插件清单缺少父目录".to_owned())?
                .to_path_buf()
        } else {
            canonical
        };
        return Ok((root, None));
    }
    let parsed = if source.starts_with("http://") || source.starts_with("https://") {
        PluginSource::Url {
            url: source.to_owned(),
            reference: None,
            sha: None,
        }
    } else if let Some(package) = source.strip_prefix("npm:") {
        PluginSource::Npm {
            package: package.to_owned(),
            version: None,
            registry: None,
        }
    } else if let Some(url) = source.strip_prefix("git:") {
        PluginSource::GitSubdir {
            url: url.to_owned(),
            path: ".".to_owned(),
            reference: None,
            sha: None,
        }
    } else if let Some(package) = source.strip_prefix("pip:") {
        PluginSource::Pip {
            package: package.to_owned(),
            version: None,
            registry: None,
        }
    } else {
        serde_json::from_value::<PluginSource>(Value::String(source.to_owned()))
            .map_err(|error| error.to_string())?
    };
    materialize_claude_plugin_source(&parsed, workspace)
}

/// 按插件清单中的完整来源执行物化；保留 `ref`/`sha`，避免先转成字符串时丢失固定版本。
pub(super) fn materialize_claude_plugin_source(
    parsed: &PluginSource,
    workspace: &Path,
) -> Result<(PathBuf, Option<String>), String> {
    if let PluginSource::Relative { path } = parsed {
        let expanded = expand_tilde(path)?;
        if expanded.exists() {
            let canonical = fs::canonicalize(&expanded)
                .map_err(|error| format!("无法访问插件来源 {}：{error}", expanded.display()))?;
            let root = if canonical.is_file() {
                canonical
                    .parent()
                    .ok_or_else(|| "插件清单缺少父目录".to_owned())?
                    .to_path_buf()
            } else {
                canonical
            };
            return Ok((root, None));
        }
        return Err(format!("插件相对路径不存在：{path}"));
    }
    let plan = parsed
        .fetch_plan(workspace)
        .map_err(|error| error.to_string())?;
    let target = create_unique_temp_dir(workspace, "fetch", "创建插件取得目录失败")?;
    match plan {
        crate::claude_plugins::SourceFetchPlan::Directory { path } => Ok((path, None)),
        crate::claude_plugins::SourceFetchPlan::File { path } => Ok((
            path.parent()
                .ok_or_else(|| "插件文件缺少父目录".to_owned())?
                .to_path_buf(),
            None,
        )),
        crate::claude_plugins::SourceFetchPlan::Git {
            url,
            reference,
            sha,
            subdir,
        } => {
            let sparse_paths = subdir
                .as_ref()
                .map(|path| vec![path.to_string_lossy().into_owned()])
                .unwrap_or_default();
            clone_git_source(
                &url,
                reference.as_deref(),
                sha.as_deref(),
                !sparse_paths.is_empty(),
                &target,
                "Git 插件来源",
            )?;
            if let Some(sha) = sha {
                checkout_git_sha(&target, &sha, "Git 插件来源")?;
            }
            apply_git_sparse_paths(&target, &sparse_paths, "Git 插件来源")?;
            let root = resolve_git_plugin_root(&target, subdir.as_deref(), "Git 插件来源")?;
            Ok((root, None))
        }
        crate::claude_plugins::SourceFetchPlan::Npm {
            package_spec,
            registry,
        } => {
            let archive_dir = target.join("npm");
            fs::create_dir_all(&archive_dir)
                .map_err(|error| format!("创建 npm 目录失败：{error}"))?;
            let mut pack = process::Command::new("npm");
            pack.current_dir(&archive_dir)
                .arg("pack")
                .arg("--ignore-scripts")
                .arg(&package_spec);
            if let Some(registry) = registry {
                pack.arg("--registry").arg(registry);
            }
            run_external(&mut pack, "npm 插件来源")?;
            let archive = fs::read_dir(&archive_dir)
                .map_err(|error| format!("读取 npm 归档失败：{error}"))?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .find(|path| path.extension().and_then(|value| value.to_str()) == Some("tgz"))
                .ok_or_else(|| "npm pack 未生成归档".to_owned())?;
            extract_archive(
                &target,
                &archive,
                archive.to_string_lossy().as_ref(),
                "npm 插件来源",
            )?;
            let package_root = target.join("package");
            Ok((package_root, None))
        }
        crate::claude_plugins::SourceFetchPlan::Pip { package_spec, .. } => Err(format!(
            "pip 插件来源已解析为安全计划，但 Claude Code 当前加载器不支持 Python 包插件：{package_spec}"
        )),
        crate::claude_plugins::SourceFetchPlan::Http { url } => {
            let bytes = http_get_with_headers(
                &url,
                &BTreeMap::new(),
                "插件 URL",
                MAX_PLUGIN_HTTP_ARCHIVE_BYTES,
            )?;
            let archive = target.join("plugin.archive");
            fs::write(&archive, &bytes).map_err(|error| format!("保存插件归档失败：{error}"))?;
            extract_archive(&target, &archive, &url, "插件 URL")?;
            let root = find_plugin_root(&target)?;
            Ok((root, None))
        }
    }
}

/// 物化 marketplace 条目原始 source，保留 `path`/`sparsePaths`/URL headers。
pub(super) fn materialize_marketplace_plugin_source(
    spec: MarketplacePluginSourceSpec,
    marketplace_root: &Path,
    workspace: &Path,
) -> Result<PathBuf, String> {
    match spec {
        MarketplacePluginSourceSpec::Relative { path } => {
            resolve_marketplace_relative_path(marketplace_root, &path)
        }
        MarketplacePluginSourceSpec::Git {
            url,
            path,
            reference,
            sha,
            mut sparse_paths,
        } => {
            let subdir = path
                .as_deref()
                .map(|path| validate_source_relative_path(path, "Git 插件 path"))
                .transpose()?;
            if let Some(path) = &subdir {
                let value = path.to_string_lossy().into_owned();
                if !sparse_paths.iter().any(|item| item == &value) {
                    sparse_paths.insert(0, value);
                }
            }
            for path in &sparse_paths {
                validate_source_relative_path(path, "Git 插件 sparsePaths")?;
            }
            let target = create_unique_temp_dir(workspace, "fetch", "创建插件取得目录失败")?;
            clone_git_source(
                &url,
                reference.as_deref(),
                sha.as_deref(),
                !sparse_paths.is_empty(),
                &target,
                "Git 插件来源",
            )?;
            if let Some(sha) = sha.as_deref() {
                checkout_git_sha(&target, sha, "Git 插件来源")?;
            }
            apply_git_sparse_paths(&target, &sparse_paths, "Git 插件来源")?;
            let root = resolve_git_plugin_root(&target, subdir.as_deref(), "Git 插件来源")?;
            Ok(root)
        }
        MarketplacePluginSourceSpec::Npm {
            package,
            version,
            registry,
        } => {
            let package_spec = match version {
                Some(version) => format!("{package}@{version}"),
                None => package,
            };
            let target = create_unique_temp_dir(workspace, "fetch", "创建插件取得目录失败")?;
            let archive_dir = target.join("npm");
            fs::create_dir_all(&archive_dir)
                .map_err(|error| format!("创建 npm 目录失败：{error}"))?;
            let mut pack = process::Command::new("npm");
            pack.current_dir(&archive_dir)
                .arg("pack")
                .arg("--ignore-scripts")
                .arg(&package_spec);
            if let Some(registry) = registry {
                pack.arg("--registry").arg(registry);
            }
            run_external(&mut pack, "npm 插件来源")?;
            let archive = fs::read_dir(&archive_dir)
                .map_err(|error| format!("读取 npm 归档失败：{error}"))?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .find(|path| path.extension().and_then(|value| value.to_str()) == Some("tgz"))
                .ok_or_else(|| "npm pack 未生成归档".to_owned())?;
            extract_archive(
                &target,
                &archive,
                archive.to_string_lossy().as_ref(),
                "npm 插件来源",
            )?;
            Ok(target.join("package"))
        }
        MarketplacePluginSourceSpec::Pip {
            package,
            version,
            registry,
        } => Err(format!(
            "pip 插件来源已解析（包 {package}，版本 {}，registry {}），但 Claude Code 当前加载器不支持 Python 包插件",
            version.as_deref().unwrap_or("latest"),
            if registry.is_some() {
                "已配置"
            } else {
                "默认"
            }
        )),
        MarketplacePluginSourceSpec::HttpArchive { url, headers } => {
            let target = create_unique_temp_dir(workspace, "fetch", "创建插件取得目录失败")?;
            let bytes =
                http_get_with_headers(&url, &headers, "插件 URL", MAX_PLUGIN_HTTP_ARCHIVE_BYTES)?;
            let archive = target.join("plugin.archive");
            fs::write(&archive, &bytes).map_err(|error| format!("保存插件归档失败：{error}"))?;
            extract_archive(&target, &archive, &url, "插件 URL")?;
            find_plugin_root(&target)
        }
    }
}

/// 校验 Git 克隆后的插件子目录仍是克隆根内的真实目录。
///
/// 先检查未跟随符号链接的选中项，再规范化路径并验证目录边界，避免把
/// 仓库中的链接当作插件根目录加载到克隆根外。
pub(super) fn resolve_git_plugin_root(
    target: &Path,
    subdir: Option<&Path>,
    label: &str,
) -> Result<PathBuf, String> {
    let canonical_target =
        fs::canonicalize(target).map_err(|error| format!("无法访问{label}取得目录：{error}"))?;
    let candidate = subdir
        .map(|path| target.join(path))
        .unwrap_or_else(|| target.to_path_buf());
    let metadata = fs::symlink_metadata(&candidate)
        .map_err(|error| format!("{label}缺少目录 {}：{error}", candidate.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "{label}目录不能是符号链接：{}",
            candidate.display()
        ));
    }
    let canonical_candidate = fs::canonicalize(&candidate)
        .map_err(|error| format!("无法访问{label}目录 {}：{error}", candidate.display()))?;
    if !canonical_candidate.starts_with(&canonical_target) || !canonical_candidate.is_dir() {
        return Err(format!(
            "{label}目录必须位于取得目录内：{}",
            candidate.display()
        ));
    }
    Ok(canonical_candidate)
}

/// 物化一个 Claude marketplace 条目，并在官方市场允许省略 plugin.json 时生成
/// 受控的合成清单。该函数只复制/解包来源，不执行插件自身的安装脚本。
pub(super) fn materialize_marketplace_plugin_entry(
    entry: &crate::claude_plugins::MarketplacePlugin,
    marketplace_manifest: &Path,
    marketplace_root: &Path,
    marketplace_plugin_root: Option<&Path>,
    downloads: &Path,
) -> Result<PathBuf, String> {
    let materialized_root = match entry.source.clone() {
        PluginSource::Relative { path } => {
            // `./` 是 Claude marketplace 表示“市场根目录即插件根目录”的合法来源。
            let relative = validate_source_relative_path(&path, "插件相对路径")?;
            let base = marketplace_plugin_root
                .map(|root| {
                    canonical_child_without_symlinks(marketplace_root, root, "市场 pluginRoot")
                })
                .transpose()?;
            canonical_child_without_symlinks(
                base.as_deref().unwrap_or(marketplace_root),
                &relative,
                "市场插件路径",
            )?
        }
        other => {
            let raw_source = load_raw_marketplace_plugin_source(marketplace_manifest, &entry.name)?;
            if let Some(raw_source) = raw_source {
                let spec = parse_marketplace_plugin_source(raw_source)?;
                materialize_marketplace_plugin_source(spec, marketplace_root, downloads)?
            } else {
                materialize_claude_plugin_source(&other, downloads)?.0
            }
        }
    };
    validate_directory_tree_without_symlinks(&materialized_root, "市场插件")?;
    if materialized_root
        .join(crate::claude_plugins::CLAUDE_PLUGIN_MANIFEST)
        .is_file()
    {
        return Ok(materialized_root);
    }

    // 官方市场的部分条目只声明 lspServers/skills 等组件；只在 KeenCode
    // 自有下载缓存中生成清单，绝不改写用户添加的市场源目录。
    let synthetic_workspace =
        create_unique_temp_dir(downloads, "synthetic", "创建插件合成目录失败")?;
    let destination = synthetic_workspace.join("plugin");
    materialize_synthetic_marketplace_plugin(&materialized_root, &destination, entry)
        .map_err(|error| error.to_string())?;
    validate_directory_tree_without_symlinks(&destination, "市场合成插件")?;
    Ok(destination)
}

/// 解析 marketplace 条目的完整依赖闭包，返回依赖在前的物化安装计划。
///
/// 先取得并校验闭包中每个插件的 `.claude-plugin/plugin.json`，再调用共享依赖
/// 拓扑解析器检查缺失/循环；调用方只有在本函数成功后才可写入插件状态。
pub(super) fn resolve_marketplace_plugin_install_plan(
    requested: &PluginId,
    market: &MarketplaceRecord,
    marketplace: &crate::claude_plugins::MarketplaceManifest,
    downloads: &Path,
) -> Result<Vec<MaterializedPlugin>, String> {
    let crate::claude_plugins::ValidatedMarketplaceIndex {
        marketplace_name,
        plugins: entries,
        requested,
    } = crate::claude_plugins::validated_marketplace_index(requested, marketplace)
        .map_err(|error| error.to_string())?;
    let marketplace_root =
        fs::canonicalize(&market.path).map_err(|error| format!("无法访问市场根目录：{error}"))?;
    let marketplace_plugin_root = marketplace
        .metadata
        .get("pluginRoot")
        .and_then(Value::as_str)
        .map(|value| validate_source_relative_path(value, "市场 metadata.pluginRoot"))
        .transpose()?;

    let mut manifests = BTreeMap::new();
    let mut materialized = BTreeMap::new();
    let mut pending = vec![requested.clone()];
    let mut queued = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !queued.insert(id.clone()) {
            continue;
        }
        let Some(entry) = entries.get(&marketplace_name_key(&id.plugin)) else {
            // 由 dependency_closure 统一生成明确的缺失依赖错误。
            continue;
        };
        let source_root = materialize_marketplace_plugin_entry(
            entry,
            Path::new(&market.manifest_path),
            &marketplace_root,
            marketplace_plugin_root.as_deref(),
            downloads,
        )?;
        let manifest = load_plugin_manifest(&source_root).map_err(|error| error.to_string())?;
        if !manifest.name.eq_ignore_ascii_case(&id.plugin) {
            return Err(format!(
                "市场插件 {} 与 plugin.json name {} 不一致",
                id, manifest.name
            ));
        }
        let mut dependencies = entry.dependencies.clone();
        dependencies.extend(manifest.dependencies.clone());
        for dependency in dependencies.keys() {
            let dependency = PluginId::parse(dependency).map_err(|error| error.to_string())?;
            match dependency.marketplace.as_deref() {
                None => {
                    let plugin = entries
                        .get(&marketplace_name_key(&dependency.plugin))
                        .map(|entry| entry.name.clone())
                        .unwrap_or_else(|| dependency.plugin.clone());
                    pending.push(PluginId {
                        plugin,
                        marketplace: Some(marketplace_name.clone()),
                    });
                }
                Some(namespace) if namespace.eq_ignore_ascii_case(&marketplace_name) => {
                    let plugin = entries
                        .get(&marketplace_name_key(&dependency.plugin))
                        .map(|entry| entry.name.clone())
                        .unwrap_or_else(|| dependency.plugin.clone());
                    pending.push(PluginId {
                        plugin,
                        marketplace: Some(marketplace_name.clone()),
                    });
                }
                Some(_) => {
                    // 不尝试取得其他市场；共享拓扑解析器会返回跨市场错误。
                }
            }
        }
        manifests.insert(id.plugin.clone(), manifest);
        materialized.insert(id, source_root);
    }

    let order = crate::claude_plugins::dependency_closure(&requested, marketplace, &manifests)
        .map_err(|error| error.to_string())?;
    order
        .into_iter()
        .map(|id| {
            let source_root = materialized
                .remove(&id)
                .ok_or_else(|| format!("没有已物化的插件来源，无法安装依赖：{id}"))?;
            Ok(MaterializedPlugin { id, source_root })
        })
        .collect()
}

/// 解析并校验 marketplace 根目录下的相对来源路径。
pub(super) fn resolve_marketplace_relative_path(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let relative = validate_source_relative_path(raw, "插件相对路径")?;
    let candidate = canonical_child_without_symlinks(root, &relative, "市场插件路径")?;
    if !candidate.is_dir() {
        return Err("市场插件路径必须位于市场根目录内".to_owned());
    }
    Ok(candidate)
}

/// 仅允许跨平台安全的相对路径；保留 Claude 常见的 `./` 前缀。
pub(super) fn validate_source_relative_path(raw: &str, label: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(format!("{label}不能为空"));
    }
    let path = Path::new(raw);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::Prefix(_)
                    | std::path::Component::RootDir
            )
        })
    {
        return Err(format!("{label}必须是安全相对路径：{raw}"));
    }
    Ok(path.to_path_buf())
}

/// 通过 reqwest 下载带 headers 的 HTTP 内容；错误文本不会回显 header 值。
pub(super) fn http_get_with_headers(
    url: &str,
    headers: &BTreeMap<String, String>,
    label: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(PLUGIN_REMOTE_TIMEOUT)
        .timeout(PLUGIN_REMOTE_TIMEOUT)
        .build()
        .map_err(|error| format!("构建{label}客户端失败：{error}"))?;
    let mut request = client.get(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .map_err(|error| format!("下载{label}失败：{error}"))?
        .error_for_status()
        .map_err(|error| format!("下载{label}返回错误：{error}"))?;
    match crate::http_response::read_http_response_limited(response, max_bytes) {
        Ok(bytes) => Ok(bytes),
        Err(crate::http_response::HttpResponseReadError::TooLarge { max_bytes }) => {
            Err(format!("{label}响应超过 {max_bytes} 字节"))
        }
        Err(crate::http_response::HttpResponseReadError::Read(error)) => {
            Err(format!("读取{label}响应失败：{error}"))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
}

/// 按来源扩展名选择归档格式；未知 URL 延续原有 tar.gz 约定。
pub(super) fn archive_format(source: &str) -> ArchiveFormat {
    let source = source.split(['?', '#']).next().unwrap_or(source);
    let source = source.to_ascii_lowercase();
    if source.ends_with(".zip") {
        ArchiveFormat::Zip
    } else if source.ends_with(".tar") {
        ArchiveFormat::Tar
    } else {
        ArchiveFormat::TarGz
    }
}

/// 归档内路径必须是非空的普通相对路径；任何 `..`、绝对路径或 NUL 都拒绝。
pub(super) fn validate_archive_entry_path(path: &Path, label: &str) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() || path.to_string_lossy().contains('\0') {
        return Err(format!("{label}路径为空或包含 NUL"));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(value) => normalized.push(value),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::Prefix(_)
            | std::path::Component::RootDir => {
                return Err(format!("{label}路径越界：{}", path.display()));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(format!("{label}路径为空"));
    }
    Ok(normalized)
}

/// 归档解包只能写入自己创建的普通目录，且不能以路径组件符号链接穿透边界。
pub(super) fn ensure_archive_directory(
    root: &Path,
    directory: &Path,
    label: &str,
) -> Result<(), String> {
    let relative = directory
        .strip_prefix(root)
        .map_err(|_| format!("{label}目录越出解包根目录"))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(format!("{label}目录路径无效：{}", directory.display()));
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!("{label}目录不能是符号链接：{}", current.display()));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!("{label}目录不是普通目录：{}", current.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|error| {
                    format!("创建{label}目录失败 {}：{error}", current.display())
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "读取{label}目录失败 {}：{error}",
                    current.display()
                ));
            }
        }
    }
    let canonical =
        fs::canonicalize(directory).map_err(|error| format!("无法规范化{label}目录：{error}"))?;
    let canonical_root =
        fs::canonicalize(root).map_err(|error| format!("无法规范化解包根目录：{error}"))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(format!("{label}目录越出解包根目录"));
    }
    Ok(())
}

/// 校验归档文件本身，避免通过链接读取受控临时目录外的文件。
pub(super) fn canonical_archive_file(
    target: &Path,
    archive: &Path,
    label: &str,
) -> Result<PathBuf, String> {
    let target_metadata = fs::symlink_metadata(target)
        .map_err(|error| format!("读取{label}解包根目录失败：{error}"))?;
    if target_metadata.file_type().is_symlink() || !target_metadata.is_dir() {
        return Err(format!("{label}解包根目录必须是普通目录"));
    }
    let canonical_target = fs::canonicalize(target)
        .map_err(|error| format!("无法规范化{label}解包根目录：{error}"))?;
    let metadata =
        fs::symlink_metadata(archive).map_err(|error| format!("读取{label}归档失败：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{label}归档必须是普通文件"));
    }
    let canonical_archive =
        fs::canonicalize(archive).map_err(|error| format!("无法规范化{label}归档：{error}"))?;
    if !canonical_archive.starts_with(&canonical_target) {
        return Err(format!("{label}归档必须位于解包根目录内"));
    }
    Ok(canonical_archive)
}

/// 为归档条目建立一个安全的、不可覆盖既有文件的目标路径。
pub(super) fn archive_file_destination(
    target: &Path,
    relative: &Path,
    archive: &Path,
    label: &str,
) -> Result<PathBuf, String> {
    let destination = target.join(relative);
    let parent = destination
        .parent()
        .ok_or_else(|| format!("{label}条目缺少父目录"))?;
    ensure_archive_directory(target, parent, label)?;
    let canonical_target =
        fs::canonicalize(target).map_err(|error| format!("无法规范化解包根目录：{error}"))?;
    let canonical_parent =
        fs::canonicalize(parent).map_err(|error| format!("无法规范化{label}父目录：{error}"))?;
    if !canonical_parent.starts_with(&canonical_target) {
        return Err(format!("{label}条目越出解包根目录"));
    }
    if destination == archive {
        return Err(format!("{label}条目不能覆盖归档文件"));
    }
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "{label}条目不能是符号链接：{}",
            destination.display()
        )),
        Ok(_) => Err(format!(
            "{label}条目重复或覆盖既有文件：{}",
            destination.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(destination),
        Err(error) => Err(format!("读取{label}条目失败：{error}")),
    }
}

/// 限制 tar 解码器读取的总流量，连同 PAX/GNU 元数据也不能无限膨胀。
pub(super) struct LimitedArchiveReader<R> {
    inner: R,
    remaining: u64,
}

impl<R> LimitedArchiveReader<R> {
    fn new(inner: R, limit: u64) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
}

impl<R: Read> Read for LimitedArchiveReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "tar 解码流超过安全限制",
            ));
        }
        let length = buffer.len().min(self.remaining as usize);
        let read = self.inner.read(&mut buffer[..length])?;
        self.remaining = self.remaining.saturating_sub(read as u64);
        Ok(read)
    }
}

/// 按限额读取 ZIP 归档；所有文件类型、路径和父目录均在写入前校验。
pub(super) fn extract_zip_archive(
    target: &Path,
    archive: &Path,
    label: &str,
    max_entries: usize,
    max_bytes: u64,
) -> Result<(), String> {
    let archive = canonical_archive_file(target, archive, label)?;
    let file = File::open(&archive).map_err(|error| format!("打开{label}失败：{error}"))?;
    let mut zip = ZipArchive::new(file).map_err(|error| format!("读取{label}失败：{error}"))?;
    let mut seen = BTreeSet::new();
    let mut unpacked_bytes = 0_u64;
    for index in 0..zip.len() {
        let entry_number = index + 1;
        if entry_number > max_entries {
            return Err(format!("{label}条目数超过 {max_entries}"));
        }
        let mut entry = zip
            .by_index(index)
            .map_err(|error| format!("读取{label}条目失败：{error}"))?;
        if entry.is_symlink() {
            return Err(format!("{label}不允许包含符号链接：{}", entry.name()));
        }
        let is_directory = entry.is_dir();
        let unix_type = entry.unix_mode().map(|mode| mode & 0o170000);
        let type_is_valid = match unix_type {
            None | Some(0) => true,
            Some(0o040000) => is_directory,
            Some(0o100000) => !is_directory,
            Some(_) => false,
        };
        if !type_is_valid || (!is_directory && !entry.is_file()) {
            return Err(format!("{label}包含不支持的特殊文件：{}", entry.name()));
        }
        let relative =
            validate_archive_entry_path(Path::new(entry.name()), &format!("{label}条目"))?;
        if !seen.insert(relative.clone()) {
            return Err(format!("{label}包含重复条目：{}", relative.display()));
        }
        if is_directory {
            if entry.size() != 0 {
                return Err(format!(
                    "{label}目录条目包含文件数据：{}",
                    relative.display()
                ));
            }
            ensure_archive_directory(target, &target.join(&relative), label)?;
            continue;
        }
        let size = entry.size();
        let next_total = unpacked_bytes
            .checked_add(size)
            .ok_or_else(|| format!("{label}解包字节数溢出"))?;
        if next_total > max_bytes {
            return Err(format!("{label}解包后超过 {max_bytes} 字节"));
        }
        let destination = archive_file_destination(target, &relative, &archive, label)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|error| format!("创建{label}条目失败 {}：{error}", destination.display()))?;
        let mut limited = entry.by_ref().take(size.saturating_add(1));
        let copied = io::copy(&mut limited, &mut output)
            .map_err(|error| format!("写入{label}条目失败 {}：{error}", destination.display()))?;
        output
            .flush()
            .map_err(|error| format!("刷新{label}条目失败 {}：{error}", destination.display()))?;
        if copied != size {
            return Err(format!(
                "{label}条目大小与声明不一致：{}",
                relative.display()
            ));
        }
        unpacked_bytes = next_total;
    }
    Ok(())
}

/// 按限额读取 tar 或 tar.gz 归档；拒绝链接、设备、FIFO 及其他特殊条目。
pub(super) fn extract_tar_reader<R: Read>(
    target: &Path,
    reader: R,
    archive: &Path,
    label: &str,
    max_entries: usize,
    max_bytes: u64,
) -> Result<(), String> {
    let stream_limit = max_bytes
        .saturating_add((max_entries as u64).saturating_mul(1024))
        .saturating_add(1024);
    let mut tar = Archive::new(LimitedArchiveReader::new(reader, stream_limit));
    let mut entries = tar
        .entries()
        .map_err(|error| format!("读取{label}条目失败：{error}"))?;
    let mut seen = BTreeSet::new();
    let mut unpacked_bytes = 0_u64;
    let mut entry_count = 0_usize;
    for entry_result in &mut entries {
        let mut entry = entry_result.map_err(|error| format!("读取{label}条目失败：{error}"))?;
        entry_count = entry_count.saturating_add(1);
        if entry_count > max_entries {
            return Err(format!("{label}条目数超过 {max_entries}"));
        }
        let raw_path = entry
            .path()
            .map_err(|error| format!("读取{label}条目路径失败：{error}"))?
            .into_owned();
        let relative = validate_archive_entry_path(&raw_path, &format!("{label}条目"))?;
        if !seen.insert(relative.clone()) {
            return Err(format!("{label}包含重复条目：{}", relative.display()));
        }
        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(format!("{label}不允许包含链接：{}", relative.display()));
        }
        if entry_type.is_dir() {
            if entry
                .header()
                .size()
                .map_err(|error| format!("读取{label}目录大小失败：{error}"))?
                != 0
            {
                return Err(format!(
                    "{label}目录条目包含文件数据：{}",
                    relative.display()
                ));
            }
            ensure_archive_directory(target, &target.join(&relative), label)?;
            continue;
        }
        if !entry_type.is_file() && !entry_type.is_contiguous() {
            return Err(format!(
                "{label}包含不支持的特殊文件：{}",
                relative.display()
            ));
        }
        let size = entry
            .header()
            .size()
            .map_err(|error| format!("读取{label}条目大小失败：{error}"))?;
        let next_total = unpacked_bytes
            .checked_add(size)
            .ok_or_else(|| format!("{label}解包字节数溢出"))?;
        if next_total > max_bytes {
            return Err(format!("{label}解包后超过 {max_bytes} 字节"));
        }
        let destination = archive_file_destination(target, &relative, archive, label)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|error| format!("创建{label}条目失败 {}：{error}", destination.display()))?;
        let mut limited = entry.by_ref().take(size.saturating_add(1));
        let copied = io::copy(&mut limited, &mut output)
            .map_err(|error| format!("写入{label}条目失败 {}：{error}", destination.display()))?;
        output
            .flush()
            .map_err(|error| format!("刷新{label}条目失败 {}：{error}", destination.display()))?;
        if copied != size {
            return Err(format!(
                "{label}条目大小与声明不一致：{}",
                relative.display()
            ));
        }
        unpacked_bytes = next_total;
    }
    Ok(())
}

/// 解包 ZIP、tar 或 tar.gz；不调用外部解包命令，生产路径统一使用上述限额。
pub(super) fn extract_archive(
    target: &Path,
    archive: &Path,
    source: &str,
    label: &str,
) -> Result<(), String> {
    match archive_format(source) {
        ArchiveFormat::Zip => extract_zip_archive(
            target,
            archive,
            label,
            MAX_PLUGIN_ARCHIVE_ENTRIES,
            MAX_PLUGIN_ARCHIVE_UNPACKED_BYTES,
        ),
        ArchiveFormat::Tar => {
            let archive_path = canonical_archive_file(target, archive, label)?;
            let file =
                File::open(&archive_path).map_err(|error| format!("打开{label}失败：{error}"))?;
            extract_tar_reader(
                target,
                file,
                &archive_path,
                label,
                MAX_PLUGIN_ARCHIVE_ENTRIES,
                MAX_PLUGIN_ARCHIVE_UNPACKED_BYTES,
            )
        }
        ArchiveFormat::TarGz => {
            let archive_path = canonical_archive_file(target, archive, label)?;
            let file =
                File::open(&archive_path).map_err(|error| format!("打开{label}失败：{error}"))?;
            extract_tar_reader(
                target,
                GzDecoder::new(file),
                &archive_path,
                label,
                MAX_PLUGIN_ARCHIVE_ENTRIES,
                MAX_PLUGIN_ARCHIVE_UNPACKED_BYTES,
            )
        }
    }
}

/// 克隆一个 Git 来源；当同时提供 `ref` 与 `sha` 时，按 Claude Code 规则以 `sha` 为准。
pub(super) fn clone_git_source(
    url: &str,
    reference: Option<&str>,
    sha: Option<&str>,
    sparse: bool,
    target: &Path,
    label: &str,
) -> Result<(), String> {
    let mut command = process::Command::new("git");
    command.arg("clone").arg("--depth").arg("1");
    if sparse {
        command.arg("--filter=blob:none").arg("--sparse");
    }
    if sha.is_none()
        && let Some(reference) = reference
    {
        command.arg("--branch").arg(reference);
    }
    command.arg(url).arg(target);
    run_external(&mut command, label)
}

/// 对 Git 克隆启用有限目录检出，避免 monorepo 下载无关文件。
pub(super) fn apply_git_sparse_paths(
    target: &Path,
    paths: &[String],
    label: &str,
) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut command = process::Command::new("git");
    command
        .current_dir(target)
        .arg("sparse-checkout")
        .arg("set")
        .arg("--no-cone");
    for path in paths {
        // 非 cone 模式下未锚定的隐藏目录路径可能被 Git 当作模糊模式，
        // 甚至漏掉 `.claude-plugin/marketplace.json`；所有已校验相对路径
        // 都转换成仓库根锚定模式。
        command.arg(sparse_checkout_pattern(path));
    }
    run_external(&mut command, label)
}

/// 将已校验的仓库相对路径转换成非 cone sparse-checkout 根锚定模式。
pub(super) fn sparse_checkout_pattern(path: &str) -> String {
    let normalized = path
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect::<Vec<_>>()
        .join("/");
    format!("/{normalized}")
}

/// 执行外部取得工具并限制输出，错误中不回显潜在密钥参数。
///
/// `Command::output` 会一直等待子进程结束；Git 在无法访问远端或等待
/// 凭据时可能永不返回。这里统一关闭 stdin、禁用 Git 终端提示并轮询
/// 子进程，在固定时限后杀掉它，让 Tauri 命令能够确定性地结束。
pub(super) fn run_external(command: &mut process::Command, label: &str) -> Result<(), String> {
    run_external_with_timeout(command, label, PLUGIN_COMMAND_TIMEOUT)
}

/// 可注入时限的外部命令执行实现；生产调用使用统一的插件命令时限，
/// 测试可用更短时限验证超时路径而不等待两分钟。
pub(super) fn run_external_with_timeout(
    command: &mut process::Command,
    label: &str,
    timeout: Duration,
) -> Result<(), String> {
    let executable = Path::new(command.get_program())
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if executable == "git" || executable == "git.exe" {
        command.env("GIT_TERMINAL_PROMPT", "0");
        command.env("GCM_INTERACTIVE", "Never");
    }
    if executable == "npm" || executable == "npm.cmd" {
        command.env("NPM_CONFIG_YES", "true");
        command.env("NPM_CONFIG_IGNORE_SCRIPTS", "true");
    }
    let mut child = command
        .stdin(Stdio::null())
        // 标准输出只包含进度信息，不参与错误判断；直接丢弃可避免
        // Git/npm 大量输出填满管道后反向阻塞子进程。
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("{label}执行失败：{error}"))?;
    let stderr_reader = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut output = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let remaining = MAX_EXTERNAL_ERROR_BYTES.saturating_sub(output.len());
                        if remaining > 0 {
                            output.extend_from_slice(&buffer[..read.min(remaining)]);
                        }
                    }
                }
            }
            output
        })
    });
    let deadline = Instant::now() + timeout;
    let mut poll_interval = PLUGIN_COMMAND_POLL_INTERVAL_INITIAL;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                // 不等待超时进程的 stderr 读取线程；丢弃 JoinHandle 可避免
                // 其子进程仍持有管道时再次阻塞当前 Tauri 命令。
                drop(stderr_reader);
                return Err(format!(
                    "{label}执行超时（{:.1} 秒）",
                    timeout.as_secs_f64()
                ));
            }
            Ok(None) => {
                std::thread::sleep(poll_interval);
                poll_interval = (poll_interval * 2).min(PLUGIN_COMMAND_POLL_INTERVAL_MAX);
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                drop(stderr_reader);
                return Err(format!("{label}等待执行结果失败：{error}"));
            }
        }
    };

    let stderr = stderr_reader
        .map(|reader| reader.join().unwrap_or_default())
        .unwrap_or_default();
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr);
        return Err(format!(
            "{label}返回失败状态：{}",
            detail.trim().chars().take(512).collect::<String>()
        ));
    }
    Ok(())
}

/// 在浅克隆后取得并检出 marketplace/plugin 指定的固定提交。
pub(super) fn checkout_git_sha(root: &Path, sha: &str, label: &str) -> Result<(), String> {
    let mut fetch = process::Command::new("git");
    fetch
        .current_dir(root)
        .args(["fetch", "--depth", "1", "origin", sha]);
    run_external(&mut fetch, label)?;
    let mut checkout = process::Command::new("git");
    checkout
        .current_dir(root)
        .args(["checkout", "--detach", sha]);
    run_external(&mut checkout, label)
}

/// 在已有根目录下读取一个不经过任何符号链接的子路径，并确认仍位于根目录内。
pub(super) fn canonical_child_without_symlinks(
    root: &Path,
    relative: &Path,
    label: &str,
) -> Result<PathBuf, String> {
    let root_metadata =
        fs::symlink_metadata(root).map_err(|error| format!("读取{label}根目录失败：{error}"))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(format!("{label}根目录必须是普通目录：{}", root.display()));
    }
    let canonical_root =
        fs::canonicalize(root).map_err(|error| format!("无法规范化{label}根目录：{error}"))?;
    let mut current = root.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty() {
        return Ok(canonical_root);
    }
    for (index, component) in components.iter().enumerate() {
        match component {
            std::path::Component::CurDir => continue,
            std::path::Component::Normal(name) => current.push(name),
            std::path::Component::ParentDir
            | std::path::Component::Prefix(_)
            | std::path::Component::RootDir => {
                return Err(format!("{label}路径越界：{}", relative.display()));
            }
        }
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| format!("读取{label}路径失败 {}：{error}", current.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "{label}路径不能包含符号链接：{}",
                current.display()
            ));
        }
        if index + 1 < components.len() && !metadata.is_dir() {
            return Err(format!("{label}路径父项不是目录：{}", current.display()));
        }
    }
    let canonical =
        fs::canonicalize(&current).map_err(|error| format!("无法规范化{label}路径：{error}"))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(format!("{label}路径越出根目录：{}", current.display()));
    }
    Ok(canonical)
}

/// 递归确认插件目录只包含普通目录、文件或指向根内文件的链接。
pub(super) fn validate_directory_tree_without_symlinks(
    root: &Path,
    label: &str,
) -> Result<(), String> {
    let canonical_root =
        fs::canonicalize(root).map_err(|error| format!("无法规范化{label}根目录：{error}"))?;
    validate_directory_tree(&canonical_root, &canonical_root, label)
}

pub(super) fn validate_directory_tree(
    root: &Path,
    current: &Path,
    label: &str,
) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(current).map_err(|error| format!("读取{label}根目录失败：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "{label}根目录必须是普通目录：{}",
            current.display()
        ));
    }
    for entry in fs::read_dir(current).map_err(|error| format!("遍历{label}失败：{error}"))? {
        let entry = entry.map_err(|error| format!("读取{label}条目失败：{error}"))?;
        let path = entry.path();
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| format!("读取{label}条目失败：{error}"))?;
        if metadata.file_type().is_symlink() {
            resolve_internal_file_symlink(root, &path).map_err(|error| error.to_string())?;
        }
        if metadata.is_dir() {
            validate_directory_tree(root, &path, label)?;
        } else if !metadata.is_file() && !metadata.file_type().is_symlink() {
            return Err(format!("{label}不能包含特殊文件：{}", path.display()));
        }
    }
    Ok(())
}

/// 在远程归档的有限深度内定位唯一 `.claude-plugin/plugin.json` 根目录。
pub(super) fn find_plugin_root(root: &Path) -> Result<PathBuf, String> {
    let canonical_root =
        fs::canonicalize(root).map_err(|error| format!("无法规范化插件归档根目录：{error}"))?;
    let root_metadata =
        fs::symlink_metadata(root).map_err(|error| format!("读取插件归档根目录失败：{error}"))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err("插件归档根目录必须是普通目录".to_owned());
    }
    let has_manifest = |candidate: &Path| -> Result<bool, String> {
        let relative = candidate
            .strip_prefix(root)
            .map_err(|_| "插件归档候选根目录越出解包根目录".to_owned())?;
        let canonical_candidate =
            canonical_child_without_symlinks(root, relative, "插件归档候选根目录")?;
        let manifest_path = canonical_candidate.join(".claude-plugin/plugin.json");
        let manifest_metadata = match fs::symlink_metadata(&manifest_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(format!("读取插件清单失败：{error}")),
        };
        if manifest_metadata.file_type().is_symlink() {
            return Err(format!(
                "插件清单不能是符号链接：{}",
                manifest_path.display()
            ));
        }
        if !manifest_metadata.is_file() {
            return Ok(false);
        }
        let manifest = canonical_child_without_symlinks(
            &canonical_candidate,
            Path::new(".claude-plugin/plugin.json"),
            "插件清单",
        )?;
        let metadata = fs::symlink_metadata(&manifest)
            .map_err(|error| format!("读取插件清单失败：{error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Ok(false);
        }
        Ok(manifest.starts_with(&canonical_root))
    };

    if has_manifest(root)? {
        return Ok(canonical_root);
    }
    let mut matches = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| format!("遍历插件归档失败：{error}"))?
    {
        let entry = entry.map_err(|error| format!("读取插件归档条目失败：{error}"))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("读取插件归档条目失败：{error}"))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("插件归档条目不能是符号链接：{}", path.display()));
        }
        if !metadata.is_dir() {
            continue;
        }
        if has_manifest(&path)? {
            matches.push(canonical_child_without_symlinks(
                root,
                path.strip_prefix(root)
                    .map_err(|_| "插件归档候选根目录越界".to_owned())?,
                "插件归档候选根目录",
            )?);
        }
    }
    match matches.as_slice() {
        [path] => Ok(path.clone()),
        [] => Err("插件归档中缺少 .claude-plugin/plugin.json".to_owned()),
        _ => Err("插件归档包含多个插件根目录，无法安全选择".to_owned()),
    }
}

/// 生成仅用于临时目录的进程内唯一后缀。
pub(super) fn unique_suffix() -> String {
    format!(
        "{}-{}",
        process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    )
}

/// 在指定父目录下独占创建临时工作区；名称冲突时重试，而不是复用已有目录。
///
/// `unique_suffix` 只是降低冲突概率，不能承担并发安全职责。真正的所有权
/// 边界由 `create_dir` 提供：只有成功创建目录的调用方才拥有该工作区。
pub(super) fn create_unique_temp_dir(
    parent: &Path,
    prefix: &str,
    label: &str,
) -> Result<PathBuf, String> {
    const MAX_ATTEMPTS: usize = 16;

    fs::create_dir_all(parent).map_err(|error| format!("{label}：{error}"))?;
    for attempt in 0..MAX_ATTEMPTS {
        let candidate = parent.join(format!("{prefix}-{}-{attempt}", unique_suffix()));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("{label}：{error}")),
        }
    }
    Err(format!(
        "{label}：临时目录名称冲突，重试 {MAX_ATTEMPTS} 次后失败"
    ))
}

/// 临时市场取得目录的失败清理守卫；成功登记后显式释放，避免留下半成品。
pub(super) struct TemporaryMarketplaceDirectory {
    path: Option<PathBuf>,
}

impl TemporaryMarketplaceDirectory {
    pub(super) fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    pub(super) fn keep(&mut self) {
        self.path = None;
    }
}

impl Drop for TemporaryMarketplaceDirectory {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        if let Err(error) = fs::remove_dir_all(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %path.display(), %error, "清理插件市场临时目录失败");
        }
    }
}

/// 清理一次插件安装/更新计划在 downloads 下创建的独占临时工作区。
///
/// 每个操作拥有独立的 plan-* 目录，因此并发安装/更新不会误删其他操作刚创建
/// 的 fetch-/synthetic- 来源，也不会触碰用户市场目录。
pub(super) struct TemporaryPluginDownloads {
    path: PathBuf,
}

impl TemporaryPluginDownloads {
    pub(super) fn new(downloads_root: &Path) -> Result<Self, String> {
        let path = create_unique_temp_dir(downloads_root, "plan", "创建插件临时工作区失败")?;
        Ok(Self { path })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryPluginDownloads {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.path.display(),
                %error,
                "清理插件下载临时工作区失败"
            );
        }
    }
}

/// 已解析的 Claude marketplace 及其取得目录所有权。
///
/// 本地 file/directory 来源没有清理令牌；HTTP/Git/npm 来源的目录只有在
/// 调用方完成登记后才应调用 `keep`，否则离开作用域时自动删除。
pub(super) struct MaterializedMarketplace {
    pub(super) root: PathBuf,
    pub(super) manifest_path: PathBuf,
    pub(super) catalog: crate::claude_plugins::MarketplaceManifest,
    pub(super) cleanup: Option<TemporaryMarketplaceDirectory>,
}

/// Claude marketplace 来源的完整本地表示；兼容 settings 中的 path/sparsePaths/headers。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MarketplaceSourceSpec {
    /// 直接下载 marketplace.json 文件。
    Url {
        url: String,
        headers: BTreeMap<String, String>,
    },
    /// Git 仓库及可选仓库内目录。
    Git {
        url: String,
        reference: Option<String>,
        path: Option<String>,
        sparse_paths: Vec<String>,
    },
    /// npm 市场包。
    Npm {
        package: String,
        version: Option<String>,
    },
    /// 本地 marketplace.json 文件。
    File { path: String },
    /// 本地市场目录。
    Directory { path: String },
}

/// 将扩展命令传入的来源解析为支持目录 path、稀疏路径和 URL headers 的结构。
pub(super) fn parse_marketplace_source_spec(
    source: &str,
) -> Result<Option<MarketplaceSourceSpec>, String> {
    let source = source.trim();
    if expand_tilde(source)?.exists() {
        return Ok(None);
    }
    if source.starts_with('{') {
        let value = serde_json::from_str::<Value>(source)
            .map_err(|error| format!("市场来源 JSON 格式无效：{error}"))?;
        return parse_marketplace_source_value(value).map(Some);
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        return Ok(Some(MarketplaceSourceSpec::Url {
            url: source.to_owned(),
            headers: BTreeMap::new(),
        }));
    }
    if let Some(repo) = source.strip_prefix("github:") {
        let (repo, reference) = split_github_ref(repo);
        return Ok(Some(MarketplaceSourceSpec::Git {
            url: format!("https://github.com/{repo}.git"),
            reference,
            path: None,
            sparse_paths: Vec::new(),
        }));
    }
    if let Some(value) = source.strip_prefix("git:") {
        let (url, reference) = split_git_ref(value);
        return Ok(Some(MarketplaceSourceSpec::Git {
            url,
            reference,
            path: None,
            sparse_paths: Vec::new(),
        }));
    }
    if let Some(package) = source.strip_prefix("npm:") {
        return Ok(Some(MarketplaceSourceSpec::Npm {
            package: package.to_owned(),
            version: None,
        }));
    }
    // Claude Code 接受 owner/repo 形式作为 GitHub 市场简写。
    if source.matches('/').count() == 1
        && !source.starts_with("./")
        && !source.starts_with("../")
        && !source.starts_with('/')
        && !source.starts_with('~')
    {
        let (repo, reference) = split_github_ref(source);
        if repo.split('/').count() == 2 && !repo.contains(char::is_whitespace) {
            return Ok(Some(MarketplaceSourceSpec::Git {
                url: format!("https://github.com/{repo}.git"),
                reference,
                path: None,
                sparse_paths: Vec::new(),
            }));
        }
    }
    Ok(None)
}

/// 解析 JSON 对象形式的 marketplace source。
pub(super) fn parse_marketplace_source_value(
    value: Value,
) -> Result<MarketplaceSourceSpec, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "市场来源必须是 JSON 对象".to_owned())?;
    let source = object
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| "市场来源缺少 source 字段".to_owned())?;
    let required_text = |key: &str| -> Result<String, String> {
        object
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| format!("市场来源缺少 {key}"))
    };
    let optional_text = |key: &str| -> Result<Option<String>, String> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
            Some(_) => Err(format!("市场来源 {key} 必须是非空字符串")),
        }
    };
    let sparse_paths = match object.get("sparsePaths") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(paths)) => paths
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|path| !path.trim().is_empty())
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| "市场来源 sparsePaths 必须是非空字符串数组".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err("市场来源 sparsePaths 必须是字符串数组".to_owned()),
    };
    match source {
        "github" => {
            let repo = required_text("repo")?;
            let (repo, shorthand_ref) = split_github_ref(&repo);
            let reference = optional_text("ref")?.or(shorthand_ref);
            Ok(MarketplaceSourceSpec::Git {
                url: format!("https://github.com/{repo}.git"),
                reference,
                path: optional_text("path")?,
                sparse_paths,
            })
        }
        "git" => {
            let value = required_text("url")?;
            let (url, shorthand_ref) = split_git_ref(&value);
            let reference = optional_text("ref")?.or(shorthand_ref);
            Ok(MarketplaceSourceSpec::Git {
                url,
                reference,
                path: optional_text("path")?,
                sparse_paths,
            })
        }
        "url" => Ok(MarketplaceSourceSpec::Url {
            url: validate_http_source_url(&required_text("url")?, "市场 URL")?,
            headers: parse_http_headers(object.get("headers"))?,
        }),
        "npm" => Ok(MarketplaceSourceSpec::Npm {
            package: required_text("package")?,
            version: optional_text("version")?,
        }),
        "file" => Ok(MarketplaceSourceSpec::File {
            path: required_text("path")?,
        }),
        "directory" => Ok(MarketplaceSourceSpec::Directory {
            path: required_text("path")?,
        }),
        other => Err(format!("不支持的市场 source：{other}")),
    }
}

/// URL 型 marketplace 来源只允许 HTTP(S)，避免把未知协议交给网络层。
pub(super) fn validate_http_source_url(url: &str, label: &str) -> Result<String, String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("{label} 只允许 http 或 https"));
    }
    Ok(url.to_owned())
}

/// 从 GitHub 简写中拆出可选 `@ref`。
pub(super) fn split_github_ref(value: &str) -> (String, Option<String>) {
    match value.rsplit_once('@') {
        Some((repo, reference)) if !repo.is_empty() && !reference.is_empty() => {
            (repo.to_owned(), Some(reference.to_owned()))
        }
        _ => (value.to_owned(), None),
    }
}

/// 从 Git URL 中拆出可选 `#ref`。
pub(super) fn split_git_ref(value: &str) -> (String, Option<String>) {
    match value.rsplit_once('#') {
        Some((url, reference)) if !url.is_empty() && !reference.is_empty() => {
            (url.to_owned(), Some(reference.to_owned()))
        }
        _ => (value.to_owned(), None),
    }
}

/// 按来源 spec 物化市场清单。
pub(super) fn materialize_claude_marketplace_spec(
    spec: MarketplaceSourceSpec,
    workspace: &Path,
) -> Result<MaterializedMarketplace, String> {
    match spec {
        MarketplaceSourceSpec::Url { url, headers } => {
            let target = create_unique_temp_dir(workspace, "market", "创建市场临时目录失败")?;
            let cleanup = TemporaryMarketplaceDirectory::new(target.clone());
            let manifest_dir = target.join(".claude-plugin");
            fs::create_dir_all(&manifest_dir)
                .map_err(|error| format!("创建市场临时目录失败：{error}"))?;
            let bytes =
                http_get_with_headers(&url, &headers, "市场清单", MAX_MARKETPLACE_MANIFEST_BYTES)?;
            let manifest_path = manifest_dir.join("marketplace.json");
            fs::write(&manifest_path, &bytes)
                .map_err(|error| format!("保存市场清单失败：{error}"))?;
            let manifest = crate::claude_plugins::parse_marketplace_manifest(&bytes)
                .map_err(|error| error.to_string())?;
            Ok(MaterializedMarketplace {
                root: target,
                manifest_path,
                catalog: manifest,
                cleanup: Some(cleanup),
            })
        }
        MarketplaceSourceSpec::Git {
            url,
            reference,
            path,
            mut sparse_paths,
        } => {
            // 未配置 sparsePaths 时必须保留 marketplace.json 引用的相对插件目录；
            // 使用完整浅克隆比先只检出清单、再猜测插件路径更可靠。
            let use_sparse_checkout = !sparse_paths.is_empty();
            let manifest_relative = path.as_deref().unwrap_or(".claude-plugin/marketplace.json");
            let manifest_relative = validate_source_relative_path(manifest_relative, "市场 path")?;
            if !manifest_relative
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            {
                return Err("市场 path 必须指向 JSON 清单".to_owned());
            }
            // 非 cone sparse-checkout 不能可靠地仅凭父目录名保留隐藏目录中的清单；
            // 直接加入 marketplace.json 文件路径，避免默认官方市场被检出为空。
            let manifest_path = manifest_relative.to_string_lossy().into_owned();
            if !sparse_paths.iter().any(|item| item == &manifest_path) {
                sparse_paths.insert(0, manifest_path);
            }
            for path in &sparse_paths {
                validate_source_relative_path(path, "市场 sparsePaths")?;
            }
            let target = create_unique_temp_dir(workspace, "market", "创建市场临时目录失败")?;
            let cleanup = TemporaryMarketplaceDirectory::new(target.clone());
            clone_git_source(
                &url,
                reference.as_deref(),
                None,
                use_sparse_checkout,
                &target,
                "Git 市场来源",
            )?;
            if use_sparse_checkout {
                apply_git_sparse_paths(&target, &sparse_paths, "Git 市场来源")?;
            }
            let manifest_path = target.join(&manifest_relative);
            let bytes = fs::read(&manifest_path).map_err(|error| {
                format!("无法读取 Git 市场清单 {}：{error}", manifest_path.display())
            })?;
            let manifest = crate::claude_plugins::parse_marketplace_manifest(&bytes)
                .map_err(|error| error.to_string())?;
            let market_root = manifest_path
                .parent()
                .and_then(|parent| {
                    (parent.file_name().and_then(|name| name.to_str()) == Some(".claude-plugin"))
                        .then(|| parent.parent().unwrap_or(parent))
                })
                .unwrap_or_else(|| manifest_path.parent().unwrap_or(&target))
                .to_path_buf();
            Ok(MaterializedMarketplace {
                root: market_root,
                manifest_path,
                catalog: manifest,
                cleanup: Some(cleanup),
            })
        }
        MarketplaceSourceSpec::Npm { package, version } => {
            let target = create_unique_temp_dir(workspace, "market", "创建市场临时目录失败")?;
            let cleanup = TemporaryMarketplaceDirectory::new(target.clone());
            let package_spec = version
                .map(|version| format!("{package}@{version}"))
                .unwrap_or(package);
            let mut pack = process::Command::new("npm");
            pack.current_dir(&target)
                .arg("pack")
                .arg("--ignore-scripts")
                .arg(package_spec);
            run_external(&mut pack, "npm 市场来源")?;
            let archive = fs::read_dir(&target)
                .map_err(|error| error.to_string())?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .find(|path| path.extension().and_then(|value| value.to_str()) == Some("tgz"))
                .ok_or_else(|| "npm pack 未生成市场归档".to_owned())?;
            extract_archive(
                &target,
                &archive,
                archive.to_string_lossy().as_ref(),
                "npm 市场来源",
            )?;
            let package_root = target.join("package");
            let (manifest_path, root) = locate_claude_marketplace(&package_root)?;
            let manifest = crate::claude_plugins::load_marketplace_manifest(&root)
                .map_err(|error| error.to_string())?;
            Ok(MaterializedMarketplace {
                root,
                manifest_path,
                catalog: manifest,
                cleanup: Some(cleanup),
            })
        }
        MarketplaceSourceSpec::File { path } => {
            let path = expand_tilde(&path)?;
            let bytes = fs::read(&path).map_err(|error| format!("读取市场清单失败：{error}"))?;
            let manifest = crate::claude_plugins::parse_marketplace_manifest(&bytes)
                .map_err(|error| error.to_string())?;
            let canonical =
                fs::canonicalize(&path).map_err(|error| format!("无法访问市场清单：{error}"))?;
            let root = if canonical
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some(".claude-plugin")
            {
                canonical
                    .parent()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
                    .ok_or_else(|| "市场清单缺少市场根目录".to_owned())?
            } else {
                canonical
                    .parent()
                    .map(Path::to_path_buf)
                    .ok_or_else(|| "市场清单缺少父目录".to_owned())?
            };
            Ok(MaterializedMarketplace {
                root,
                manifest_path: canonical,
                catalog: manifest,
                cleanup: None,
            })
        }
        MarketplaceSourceSpec::Directory { path } => {
            let path = expand_tilde(&path)?;
            let (manifest_path, root) = locate_claude_marketplace(&path)?;
            let manifest = crate::claude_plugins::load_marketplace_manifest(&root)
                .map_err(|error| error.to_string())?;
            Ok(MaterializedMarketplace {
                root,
                manifest_path,
                catalog: manifest,
                cleanup: None,
            })
        }
    }
}

/// 在本地或远程来源取得 Claude marketplace.json，并返回其根目录与清单。
pub(super) fn materialize_claude_marketplace(
    source: &str,
    workspace: &Path,
) -> Result<MaterializedMarketplace, String> {
    let source = source.trim();
    if let Some(spec) = parse_marketplace_source_spec(source)? {
        return materialize_claude_marketplace_spec(spec, workspace);
    }
    let expanded = expand_tilde(source)?;
    if expanded.exists() {
        let (manifest_path, root) = locate_claude_marketplace(&expanded)?;
        let manifest = crate::claude_plugins::load_marketplace_manifest(&root)
            .map_err(|error| error.to_string())?;
        return Ok(MaterializedMarketplace {
            root,
            manifest_path,
            catalog: manifest,
            cleanup: None,
        });
    }
    let target = create_unique_temp_dir(workspace, "market", "创建市场临时目录失败")?;
    let cleanup = TemporaryMarketplaceDirectory::new(target.clone());
    let parsed = if source.starts_with("http://") || source.starts_with("https://") {
        crate::claude_plugins::MarketplaceSource::Url {
            url: source.to_owned(),
            headers: BTreeMap::new(),
        }
    } else if let Some(repo) = source.strip_prefix("github:") {
        crate::claude_plugins::MarketplaceSource::Github {
            repo: repo.to_owned(),
            reference: None,
            path: None,
            sparse_paths: Vec::new(),
        }
    } else if let Some(url) = source.strip_prefix("git:") {
        crate::claude_plugins::MarketplaceSource::Git {
            url: url.to_owned(),
            reference: None,
            path: None,
            sparse_paths: Vec::new(),
        }
    } else if let Some(package) = source.strip_prefix("npm:") {
        crate::claude_plugins::MarketplaceSource::Npm {
            package: package.to_owned(),
            version: None,
            registry: None,
        }
    } else {
        serde_json::from_value::<crate::claude_plugins::MarketplaceSource>(Value::String(
            source.to_owned(),
        ))
        .map_err(|error| error.to_string())?
    };
    let plan = parsed
        .fetch_plan(&EmptyMarketplaceSettings)
        .map_err(|error| error.to_string())?;
    match plan {
        crate::claude_plugins::SourceFetchPlan::Http { url } => {
            let bytes = http_get_with_headers(
                &url,
                &BTreeMap::new(),
                "市场清单",
                MAX_MARKETPLACE_MANIFEST_BYTES,
            )?;
            let manifest_dir = target.join(".claude-plugin");
            fs::create_dir_all(&manifest_dir)
                .map_err(|error| format!("创建市场清单目录失败：{error}"))?;
            let manifest_path = manifest_dir.join("marketplace.json");
            fs::write(&manifest_path, &bytes)
                .map_err(|error| format!("保存市场清单失败：{error}"))?;
            let bytes = fs::read(&manifest_path).map_err(|error| error.to_string())?;
            let manifest = crate::claude_plugins::parse_marketplace_manifest(&bytes)
                .map_err(|error| error.to_string())?;
            return Ok(MaterializedMarketplace {
                root: target,
                manifest_path,
                catalog: manifest,
                cleanup: Some(cleanup),
            });
        }
        crate::claude_plugins::SourceFetchPlan::Git { url, reference, .. } => {
            let mut command = process::Command::new("git");
            command.arg("clone").arg("--depth").arg("1");
            if let Some(reference) = reference {
                command.arg("--branch").arg(reference);
            }
            command.arg(url).arg(&target);
            run_external(&mut command, "Git 市场来源")?;
        }
        crate::claude_plugins::SourceFetchPlan::Npm {
            package_spec,
            registry,
        } => {
            let mut pack = process::Command::new("npm");
            pack.current_dir(&target)
                .arg("pack")
                .arg("--ignore-scripts")
                .arg(package_spec);
            if let Some(registry) = registry {
                pack.arg("--registry").arg(registry);
            }
            run_external(&mut pack, "npm 市场来源")?;
            let archive = fs::read_dir(&target)
                .map_err(|error| error.to_string())?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .find(|path| path.extension().and_then(|value| value.to_str()) == Some("tgz"))
                .ok_or_else(|| "npm pack 未生成市场归档".to_owned())?;
            extract_archive(
                &target,
                &archive,
                archive.to_string_lossy().as_ref(),
                "npm 市场来源",
            )?;
        }
        crate::claude_plugins::SourceFetchPlan::Pip { package_spec, .. } => {
            return Err(format!(
                "pip 不是 Claude marketplace 的市场来源：{package_spec}"
            ));
        }
        crate::claude_plugins::SourceFetchPlan::Directory { path } => {
            let manifest = crate::claude_plugins::load_marketplace_manifest(&path)
                .map_err(|error| error.to_string())?;
            return Ok(MaterializedMarketplace {
                root: path.clone(),
                manifest_path: path.join(crate::claude_plugins::CLAUDE_MARKETPLACE_MANIFEST),
                catalog: manifest,
                cleanup: None,
            });
        }
        crate::claude_plugins::SourceFetchPlan::File { path } => {
            let bytes = fs::read(&path).map_err(|error| error.to_string())?;
            let manifest = crate::claude_plugins::parse_marketplace_manifest(&bytes)
                .map_err(|error| error.to_string())?;
            return Ok(MaterializedMarketplace {
                root: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
                manifest_path: path,
                catalog: manifest,
                cleanup: None,
            });
        }
    }
    let (manifest_path, root) = locate_claude_marketplace(&target)?;
    let manifest = crate::claude_plugins::load_marketplace_manifest(&root)
        .map_err(|error| error.to_string())?;
    Ok(MaterializedMarketplace {
        root,
        manifest_path,
        catalog: manifest,
        cleanup: Some(cleanup),
    })
}

/// Claude marketplace settings 来源暂由调用方显式管理，避免读取未知配置键。
pub(super) struct EmptyMarketplaceSettings;

impl crate::claude_plugins::MarketplaceSettings for EmptyMarketplaceSettings {
    /// 当前扩展命令不自动解析 settings 引用。
    fn marketplace_source(&self, _key: &str) -> Option<crate::claude_plugins::MarketplaceSource> {
        None
    }
}

/// 定位 `.claude-plugin/marketplace.json` 所在的市场根目录。
pub(super) fn locate_claude_marketplace(input: &Path) -> Result<(PathBuf, PathBuf), String> {
    let input_metadata =
        fs::symlink_metadata(input).map_err(|error| format!("无法读取市场来源：{error}"))?;
    if input_metadata.file_type().is_symlink() {
        return Err(format!("市场来源不能是符号链接：{}", input.display()));
    }
    let input = if input_metadata.is_file() {
        input.to_path_buf()
    } else if input_metadata.is_dir() {
        input.join(crate::claude_plugins::CLAUDE_MARKETPLACE_MANIFEST)
    } else {
        return Err(format!("市场来源不是目录或清单文件：{}", input.display()));
    };
    if input.file_name().and_then(|name| name.to_str()) != Some("marketplace.json") {
        return Err(format!(
            "市场清单必须位于 {}",
            crate::claude_plugins::CLAUDE_MARKETPLACE_MANIFEST
        ));
    }
    let root = input
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "市场清单缺少市场根目录".to_owned())?
        .to_path_buf();
    let relative = input
        .strip_prefix(&root)
        .map_err(|_| "市场清单不在市场根目录内".to_owned())?;
    let canonical = canonical_child_without_symlinks(&root, relative, "市场清单")?;
    let canonical_root =
        fs::canonicalize(&root).map_err(|error| format!("无法规范化市场根目录：{error}"))?;
    let metadata =
        fs::symlink_metadata(&canonical).map_err(|error| format!("无法读取市场清单：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("市场清单必须是普通文件".to_owned());
    }
    if !canonical.starts_with(&canonical_root)
        || canonical.file_name().and_then(|name| name.to_str()) != Some("marketplace.json")
    {
        return Err("市场清单必须位于市场根目录内".to_owned());
    }
    Ok((canonical, canonical_root))
}
