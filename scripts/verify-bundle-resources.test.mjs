import assert from "node:assert/strict";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  truncateSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";

import {
  classifyBundleArtifact,
  classifyNestedBundleArtifact,
  bundleArtifactSizeBytes,
  findResourceDirectories,
  findNestedBundleArtifacts,
  findBundleArtifacts,
  MAX_BUNDLE_SIZE_BYTES,
  validateBundleSizes,
  validateExtractedBundle,
  validateBundleConfiguration,
  validateResourceDirectory,
  verifyBundle,
  verifyBundleArtifact,
  sha256File,
  nsisExtractionInvocation,
  verifyNsisArtifact,
} from "./verify-bundle-resources.mjs";

// 解包器把安装包视为纯数据，路径中的空格/分号不得进入 shell 或拆成多个参数。
test("NSIS 验包只构造 7-Zip 解包调用", () => {
  const artifact = join(tmpdir(), "Keen Code;fixture-setup.exe");
  const target = join(tmpdir(), "keencode-nsis-test space");
  const invocation = nsisExtractionInvocation(artifact, target);
  assert.notEqual(invocation.command, artifact);
  assert.deepEqual(invocation.args, ["x", "-y", "-bd", `-o${target}`, "--", artifact]);
  assert(!invocation.args.includes("/S"));
});

// 实际运行完整的校验/清理路径，仅以纯文件副本模拟解包器输出；从不执行合成安装包。
test("NSIS 解包后校验资源且不执行卸载程序", () => {
  let temporary;
  let calls = 0;
  verifyNsisArtifact("KeenCode-setup.exe", process.cwd(), (command, args) => {
    ++calls;
    assert.notEqual(command, "KeenCode-setup.exe");
    temporary = args.find(value => value.startsWith("-o")).slice(2);
    copyFileSync(join(process.cwd(), "LICENSE"), join(temporary, "LICENSE"));
    // 即便解包中带有卸载程序，也只能把它当数据，不能执行。
    writeFileSync(join(temporary, "uninstall.exe"), "synthetic-not-executable");
  });
  assert.equal(calls, 1);
  assert(!existsSync(temporary));
});

test("NSIS 解包失败不回退到安装执行，并清理本次临时目录", () => {
  let temporary;
  let calls = 0;
  assert.throws(() => verifyNsisArtifact("KeenCode-setup.exe", process.cwd(), (_command, args) => {
    ++calls;
    temporary = args.find(value => value.startsWith("-o")).slice(2);
    throw new Error("模拟缺少完整 7-Zip");
  }), /不会回退为安装执行.*模拟缺少完整 7-Zip/);
  assert.equal(calls, 1);
  assert(!existsSync(temporary));
});

test("NSIS 解包成功但缺少法律文件时仍然失败", () => {
  let temporary;
  assert.throws(() => verifyNsisArtifact("KeenCode-setup.exe", process.cwd(), (_command, args) => {
    temporary = args.find(value => value.startsWith("-o")).slice(2);
    writeFileSync(join(temporary, "icon.ico"), "no-project-license");
  }), /解包后未找到完整法律资源目录/);
  assert(!existsSync(temporary));
});

// 对象形式的成品路径必须先解析为本机绝对路径，避免 Windows msiexec 收到跨平台分隔符。
test("对象形式的 bundle 成品路径在验证入口中规范化", () => {
  const missingPath = `${tmpdir().replaceAll("\\", "/")}/keencode-missing-bundle-path/KeenCode.msi`;
  const expectedPath = resolve(missingPath);
  assert.throws(
    () => verifyBundleArtifact({ kind: "msi", path: missingPath }, process.cwd()),
    (error) => error instanceof Error && error.message === `bundle 成品不存在：${expectedPath}`,
  );
});

// updater archive 中显式声明的 installer.exe 仍按嵌套 NSIS 处理，不经过根 bundle 文件名分类。
test("对象形式的嵌套 installer.exe 保留 NSIS 类型", () => {
  const root = mkdtempSync(join(tmpdir(), "keencode-nested-nsis-kind-"));
  try {
    const installer = join(root, "installer.exe");
    writeFileSync(installer, "synthetic-nsis-installer");
    const pathWithForwardSlashes = installer.replaceAll("\\", "/");
    const context = {
      verifiedPayloads: new Map([
        [`nsis:${sha256File(installer)}`, ["cached-nsis-result"]],
      ]),
    };
    assert.deepEqual(
      verifyBundleArtifact(
        { kind: "nsis", path: pathWithForwardSlashes },
        process.cwd(),
        context,
      ),
      ["cached-nsis-result"],
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// 读取当前 Tauri 配置并确认只要求项目自身许可证，不依赖第三方汇总。
test("Tauri 配置携带法律资源", () => {
  const config = readFileSync(join(process.cwd(), "src-tauri/tauri.conf.json"), "utf8");
  assert.deepEqual(validateBundleConfiguration(JSON.parse(config)), [
    { source: join(process.cwd(), "LICENSE"), target: "LICENSE" },
  ]);
});

// 仅识别最终 bundle，避免把 target/debug 下的应用可执行文件误当作安装包。
test("识别最终发行成品类型", () => {
  assert.equal(classifyBundleArtifact("C:/work/target/release/bundle/msi/KeenCode.msi"), "msi");
  assert.equal(classifyBundleArtifact("C:/work/target/release/bundle/nsis/KeenCode-setup.exe"), "nsis");
  assert.equal(classifyBundleArtifact("C:/work tree/target/release/bundle/msi/KeenCode.msi"), "msi");
  assert.equal(classifyBundleArtifact("C:/work tree/target/release/bundle/nsis/KeenCode-setup.exe"), "nsis");
  assert.equal(classifyBundleArtifact("C:/work/target/release/keencode-desktop.exe"), null);
  assert.equal(classifyBundleArtifact("C:/work/target/release/bundle/macos/KeenCode.app"), "app");
  assert.equal(classifyBundleArtifact("C:/work/target/release/bundle/nsis/KeenCode.nsis.zip"), "zip");
  assert.equal(classifyBundleArtifact("C:/work/target/release/bundle/macos/KeenCode.app.tar.gz"), "tar");
});

// Tauri updater archive 只携带安装器时，必须能从解包树识别嵌套 NSIS 成品，
// 同时识别 macOS app 目录，供归档校验递归复用最终成品校验入口。
test("识别 updater archive 中的嵌套最终成品", () => {
  const root = mkdtempSync(join(tmpdir(), "keencode-updater-archive-"));
  try {
    const installer = join(root, "KeenCode_1.0.0_x64-setup.exe");
    writeFileSync(installer, "synthetic-nsis-installer");
    const app = join(root, "KeenCode.app");
    mkdirSync(join(app, "Contents", "Resources"), { recursive: true });
    assert.equal(classifyNestedBundleArtifact(installer), "nsis");
    assert.equal(classifyNestedBundleArtifact(join(root, "installer.exe")), "nsis");
    assert.equal(classifyNestedBundleArtifact(app), "app");
    assert.deepEqual(
      findNestedBundleArtifacts(root).map(({ kind }) => kind).sort(),
      ["app", "nsis"],
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// 归档中同时存在有效裸 resources 和损坏安装器时，不能因为前者通过便短路返回。
test("混合 updater archive 仍校验嵌套成品", () => {
  const root = mkdtempSync(join(tmpdir(), "keencode-mixed-updater-archive-"));
  try {
    const resources = join(root, "resources");
    mkdirSync(resources);
    copyFileSync(join(process.cwd(), "LICENSE"), join(resources, "LICENSE"));
    writeFileSync(join(root, "KeenCode_1.0.0_x64-setup.exe"), "corrupted-installer");
    assert.throws(() =>
      validateExtractedBundle(root, "synthetic-updater", process.cwd(), { verifiedPayloads: new Map() }),
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// 用源文件的字节副本模拟 Tauri resources，验证资源校验的完整路径。
test("资源目录必须与源文件一致", (context) => {
  const root = mkdtempSync(join(tmpdir(), "keencode-license-resources-"));
  // 即使断言失败也清理夹具，避免在系统临时目录遗留许可证副本。
  context.after(() => rmSync(root, { recursive: true, force: true }));
  const resources = join(root, "resources");
  mkdirSync(resources);
  copyFileSync(join(process.cwd(), "LICENSE"), join(resources, "LICENSE"));
  assert.deepEqual(validateResourceDirectory(resources, process.cwd()), []);
  assert.deepEqual(findResourceDirectories(root), [resources]);

  // 项目许可证缺失必须阻断安装包校验。
  const missingResources = join(root, "missing-resources");
  mkdirSync(missingResources);
  assert.match(validateResourceDirectory(missingResources, process.cwd()).join("\n"), /缺少 LICENSE/);

  // 文件存在但被替换时，必须通过摘要而不是文件名检查出来。
  const tamperedResources = join(root, "tampered-resources");
  mkdirSync(tamperedResources);
  writeFileSync(join(tamperedResources, "LICENSE"), "tampered\n");
  assert.match(
    validateResourceDirectory(tamperedResources, process.cwd()).join("\n"),
    /LICENSE 与源文件摘要不一致/,
  );

  // tauri-build 在打包前会把资源暂存到 target/debug 或 target/release 根目录。
  const staging = join(root, "release");
  mkdirSync(staging);
  copyFileSync(join(process.cwd(), "LICENSE"), join(staging, "LICENSE"));
  assert.deepEqual(findResourceDirectories(root).sort(), [resources, staging].sort());

  // 最终成品必须单独被发现；裸资源目录不应满足发布门禁。
  const artifactRoot = join(root, "artifact-target");
  const msiDirectory = join(artifactRoot, "release", "bundle", "msi");
  const nsisDirectory = join(artifactRoot, "release", "bundle", "nsis");
  mkdirSync(msiDirectory, { recursive: true });
  mkdirSync(nsisDirectory, { recursive: true });
  writeFileSync(join(msiDirectory, "KeenCode.msi"), "test-msi");
  writeFileSync(join(nsisDirectory, "KeenCode-setup.exe"), "test-nsis");
  writeFileSync(join(nsisDirectory, "KeenCode.nsis.zip"), "test-nsis-updater");
  writeFileSync(join(nsisDirectory, "KeenCode.app.tar.gz"), "test-macos-updater");
  assert.deepEqual(
    findBundleArtifacts(artifactRoot).map((item) => item.kind).sort(),
    ["msi", "nsis", "tar", "zip"],
  );
});

// 目标文件名冲突会使后写入的资源覆盖前一个资源，配置门禁必须提前拒绝。
test("拒绝 bundle 资源目标路径冲突", () => {
  const config = JSON.parse(readFileSync(join(process.cwd(), "src-tauri/tauri.conf.json"), "utf8"));
  config.bundle.resources["../README.md"] = "LICENSE";
  assert.throws(() => validateBundleConfiguration(config), /bundle 目标路径冲突/);
});

// 构建输出可能带有只存放图标的 resources/ 目录；它不应遮蔽真正的法律资源根。
test("忽略不包含法律文件的通用 resources 目录", () => {
  const root = mkdtempSync(join(tmpdir(), "keencode-resource-noise-"));
  try {
    const noise = join(root, "resources");
    const install = join(root, "install");
    mkdirSync(noise);
    mkdirSync(install);
    writeFileSync(join(noise, "icon.ico"), "runtime icon");
    copyFileSync(join(process.cwd(), "LICENSE"), join(install, "LICENSE"));
    assert.deepEqual(findResourceDirectories(root, true), [install]);
    assert.deepEqual(validateExtractedBundle(root, "synthetic-install", process.cwd(), { verifiedPayloads: new Map() }), [install]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// 每个平台实际安装或分发文件都必须经过 50 MB 硬门禁，不能只生成大小报告。
test("安装包体积门禁覆盖实际类型、目录和精确阈值", () => {
  const root = mkdtempSync(join(tmpdir(), "keencode-bundle-size-"));
  try {
    const bundleRoot = join(root, "release", "bundle");
    const msi = join(bundleRoot, "msi", "KeenCode.msi");
    const nsis = join(bundleRoot, "nsis", "KeenCode-setup.exe");
    const dmg = join(bundleRoot, "dmg", "KeenCode.dmg");
    const deb = join(bundleRoot, "deb", "KeenCode.deb");
    const appImage = join(bundleRoot, "appimage", "KeenCode.AppImage");
    const app = join(bundleRoot, "macos", "KeenCode.app");
    mkdirSync(join(app, "Contents", "Resources"), { recursive: true });
    for (const path of [msi, nsis, dmg, deb, appImage]) {
      mkdirSync(join(path, ".."), { recursive: true });
      writeFileSync(path, "small bundle");
    }
    writeFileSync(join(app, "Contents", "Resources", "KeenCode"), "app bundle");

    const artifacts = findBundleArtifacts(root);
    assert.deepEqual(
      artifacts.map(({ kind }) => kind).sort(),
      ["app", "appimage", "deb", "dmg", "msi", "nsis"],
    );
    const sizeReport = validateBundleSizes(artifacts);
    assert.deepEqual(
      sizeReport.artifacts.map(({ kind }) => kind).sort(),
      ["appimage", "deb", "dmg", "msi", "nsis"],
    );
    assert.deepEqual(sizeReport.expandedBytes.map(({ path }) => path), [app]);
    assert.equal(bundleArtifactSizeBytes({ kind: "app", path: app }), "app bundle".length);

    // 恰好达到上限仍然允许，只有多一个字节才阻断发布。
    truncateSync(msi, MAX_BUNDLE_SIZE_BYTES);
    assert.equal(
      bundleArtifactSizeBytes({ kind: "msi", path: msi }),
      MAX_BUNDLE_SIZE_BYTES,
    );
    assert.doesNotThrow(() => validateBundleSizes([{ kind: "msi", path: msi }]));
    truncateSync(msi, MAX_BUNDLE_SIZE_BYTES + 1);
    assert.throws(
      () => validateBundleSizes([{ kind: "msi", path: msi }]),
      /最终 bundle 体积校验失败.*超过 50000000 字节上限/s,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// 没有实际安装包、只有展开 .app 或只有普通可执行文件时必须失败，不能误报通过。
test("空目录和不支持的目标类型不能通过体积门禁", () => {
  const root = mkdtempSync(join(tmpdir(), "keencode-empty-bundle-"));
  try {
    assert.throws(
      () => verifyBundle(root, join(process.cwd(), "src-tauri/tauri.conf.json")),
      /未找到实际安装\/分发包成品.*空目录/s,
    );
    assert.throws(() => validateBundleSizes(findBundleArtifacts(root)), /空目录不能通过体积校验/);
    const app = join(root, "release", "bundle", "macos", "KeenCode.app");
    mkdirSync(join(app, "Contents", "Resources"), { recursive: true });
    writeFileSync(join(app, "Contents", "Resources", "KeenCode"), "expanded app");
    assert.throws(
      () => validateBundleSizes(findBundleArtifacts(root)),
      /仅有 \.app 展开目录不能通过体积校验/,
    );
    const ordinaryExe = join(root, "release", "KeenCode.exe");
    mkdirSync(join(ordinaryExe, ".."), { recursive: true });
    writeFileSync(ordinaryExe, "not an installer");
    assert.deepEqual(
      findBundleArtifacts(root).filter(({ kind }) => kind !== "app"),
      [],
    );
    assert.throws(
      () => validateBundleSizes([{ kind: "exe", path: ordinaryExe }]),
      /bundle 成品类型与路径分类不一致/,
    );

    const zeroByteMsi = join(root, "release", "bundle", "msi", "KeenCode.msi");
    mkdirSync(join(zeroByteMsi, ".."), { recursive: true });
    writeFileSync(zeroByteMsi, "");
    assert.throws(
      () => validateBundleSizes([{ kind: "msi", path: zeroByteMsi }]),
      /大小为 0 字节/,
    );

    const wrongKindPath = join(root, "release", "bundle", "nsis", "KeenCode-setup.exe");
    mkdirSync(join(wrongKindPath, ".."), { recursive: true });
    writeFileSync(wrongKindPath, "installer");
    assert.throws(
      () => validateBundleSizes([{ kind: "msi", path: wrongKindPath }]),
      /类型与路径分类不一致/,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
