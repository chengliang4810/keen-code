#!/usr/bin/env python3
"""裁切原生 PrintWindow 截图并与固定 ZCode PNG 生成可复核的比较证据。

脚本只负责几何裁切、像素统计和证据文件生成，不根据误差阈值宣布产品验收通过。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path
from typing import Any

from PIL import Image, ImageChops, ImageStat


SCHEMA = "keencode/native-zcode-visual-comparison"
SCHEMA_VERSION = 1
DEFAULT_SOURCE_COMMIT = "29628c9acdb81b703bbd4080c207a0e7ce5e276e"


class ComparisonInputError(ValueError):
    """输入证据缺失或无法安全裁切时抛出的错误。"""


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="裁切原生 BMP client 区并与同尺寸 ZCode PNG 生成比较证据。"
    )
    parser.add_argument("--native-bmp", type=Path, required=True, help="原生 PrintWindow BMP")
    parser.add_argument("--source-png", type=Path, required=True, help="固定源基线 PNG")
    parser.add_argument(
        "--metrics",
        type=Path,
        required=True,
        help="与 BMP 对应的 native-live metrics JSON，包含 client/window 矩形",
    )
    parser.add_argument("--output-dir", type=Path, required=True, help="比较产物目录")
    parser.add_argument(
        "--source-commit",
        default=DEFAULT_SOURCE_COMMIT,
        help="源基线提交号；默认使用固定 ZCode 3.14.3 提交",
    )
    parser.add_argument(
        "--diff-scale",
        type=int,
        default=4,
        help="diff 图的通道放大倍数，默认 4；只影响可视化，不影响统计",
    )
    parser.add_argument(
        "--regions-json",
        type=Path,
        help="可选的 client 物理像素分区定义；分区只做辅助统计，不改变全图判定",
    )
    parser.add_argument(
        "--fail-on-difference",
        action="store_true",
        help="仅在脚本调用方需要时，以差异返回退出码 2；默认差异仍返回 0",
    )
    return parser.parse_args()


def require_file(path: Path, label: str) -> None:
    if not path.is_file():
        raise ComparisonInputError(f"{label}不存在：{path}")


def read_json(path: Path, label: str) -> dict[str, Any]:
    require_file(path, label)
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ComparisonInputError(f"{label}不是可读取的 JSON：{path}；{error}") from error
    if not isinstance(value, dict):
        raise ComparisonInputError(f"{label}根节点必须是对象：{path}")
    return value


def integer_field(value: Any, field: str) -> int:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ComparisonInputError(f"metrics 字段 {field} 不是数字")
    integer = int(value)
    if integer != value:
        raise ComparisonInputError(f"metrics 字段 {field} 不是整数")
    return integer


def read_client_geometry(metrics_path: Path) -> dict[str, Any]:
    metrics = read_json(metrics_path, "metrics 文件")
    window = metrics.get("window")
    if not isinstance(window, dict):
        raise ComparisonInputError("metrics 缺少 window 对象")

    client_origin = window.get("clientOriginScreen")
    client_rect = window.get("clientRect")
    window_rect = window.get("windowRect")
    if not all(isinstance(value, dict) for value in (client_origin, client_rect, window_rect)):
        raise ComparisonInputError("metrics 必须同时包含 clientOriginScreen、clientRect、windowRect")

    client_x = integer_field(client_origin.get("x"), "window.clientOriginScreen.x")
    client_y = integer_field(client_origin.get("y"), "window.clientOriginScreen.y")
    window_left = integer_field(window_rect.get("left"), "window.windowRect.left")
    window_top = integer_field(window_rect.get("top"), "window.windowRect.top")
    client_width = integer_field(client_rect.get("width"), "window.clientRect.width")
    client_height = integer_field(client_rect.get("height"), "window.clientRect.height")
    if client_width <= 0 or client_height <= 0:
        raise ComparisonInputError("metrics 的 client 尺寸必须为正数")

    # PrintWindow 的整窗 BMP 原点是 windowRect，clientOriginScreen 给出内容区在屏幕上的原点。
    # 两者相减得到必须裁切的物理像素偏移；不能把逻辑窗口尺寸直接当作 BMP 坐标。
    offset_x = client_x - window_left
    offset_y = client_y - window_top
    if offset_x < 0 or offset_y < 0:
        raise ComparisonInputError(
            f"client 原点位于 windowRect 之外：offset=({offset_x},{offset_y})"
        )

    return {
        "offset": {"x": offset_x, "y": offset_y},
        "client": {
            "width": client_width,
            "height": client_height,
            "origin_screen": {"x": client_x, "y": client_y},
        },
        "window": {
            "left": window_left,
            "top": window_top,
            "width": integer_field(window_rect.get("width"), "window.windowRect.width"),
            "height": integer_field(window_rect.get("height"), "window.windowRect.height"),
        },
        "dpi": window.get("dpi"),
        "dpi_awareness": window.get("dpiAwareness"),
    }


def strict_integer(value: Any, field: str) -> int:
    """读取分区坐标；JSON 中的浮点数也拒绝，避免隐式改变裁切边界。"""
    if isinstance(value, bool) or not isinstance(value, int):
        raise ComparisonInputError(f"分区字段 {field} 必须是整数")
    return value


def read_regions(path: Path) -> dict[str, Any]:
    regions_doc = read_json(path, "分区 JSON")
    image_size = regions_doc.get("image_size")
    if not isinstance(image_size, dict):
        raise ComparisonInputError("分区 JSON 缺少 image_size 对象")
    image_width = strict_integer(image_size.get("width"), "image_size.width")
    image_height = strict_integer(image_size.get("height"), "image_size.height")
    if image_width <= 0 or image_height <= 0:
        raise ComparisonInputError("分区 JSON 的 image_size 必须是正整数")

    raw_regions = regions_doc.get("regions")
    if not isinstance(raw_regions, list) or not raw_regions:
        raise ComparisonInputError("分区 JSON 的 regions 必须是非空数组")

    names: set[str] = set()
    regions: list[dict[str, Any]] = []
    for index, raw_region in enumerate(raw_regions):
        if not isinstance(raw_region, dict):
            raise ComparisonInputError(f"分区 regions[{index}] 必须是对象")
        raw_name = raw_region.get("name")
        if not isinstance(raw_name, str) or not raw_name.strip():
            raise ComparisonInputError(f"分区 regions[{index}].name 必须是非空字符串")
        name = raw_name.strip()
        if name in names:
            raise ComparisonInputError(f"分区名称重复：{name}")
        names.add(name)

        x = strict_integer(raw_region.get("x"), f"regions[{index}].x")
        y = strict_integer(raw_region.get("y"), f"regions[{index}].y")
        width = strict_integer(raw_region.get("width"), f"regions[{index}].width")
        height = strict_integer(raw_region.get("height"), f"regions[{index}].height")
        if x < 0 or y < 0:
            raise ComparisonInputError(f"分区 {name} 的 x/y 必须是非负整数")
        if width <= 0 or height <= 0:
            raise ComparisonInputError(f"分区 {name} 的 width/height 必须是正整数")
        if x + width > image_width or y + height > image_height:
            raise ComparisonInputError(
                f"分区 {name} 超出 image_size："
                f"box=({x},{y},{x + width},{y + height}) "
                f"image={image_width}x{image_height}"
            )

        note = raw_region.get("note")
        if note is not None and not isinstance(note, str):
            raise ComparisonInputError(f"分区 {name} 的 note 必须是字符串")
        region = {"name": name, "x": x, "y": y, "width": width, "height": height}
        if note is not None:
            region["note"] = note
        regions.append(region)

    notes = regions_doc.get("notes")
    if notes is not None and not isinstance(notes, (str, list)):
        raise ComparisonInputError("分区 JSON 的 notes 必须是字符串或数组")
    if isinstance(notes, list) and not all(isinstance(note, str) for note in notes):
        raise ComparisonInputError("分区 JSON 的 notes 数组只能包含字符串")

    result: dict[str, Any] = {
        "path": str(path),
        "image_size": {"width": image_width, "height": image_height},
        "regions": regions,
    }
    if notes is not None:
        result["notes"] = notes
    return result


def load_rgb(path: Path, label: str) -> Image.Image:
    require_file(path, label)
    try:
        with Image.open(path) as image:
            return image.convert("RGB").copy()
    except OSError as error:
        raise ComparisonInputError(f"{label}不是可读取的图像：{path}；{error}") from error


def crop_client(native: Image.Image, geometry: dict[str, Any]) -> Image.Image:
    offset = geometry["offset"]
    client = geometry["client"]
    left = offset["x"]
    top = offset["y"]
    right = left + client["width"]
    bottom = top + client["height"]
    if right > native.width or bottom > native.height:
        raise ComparisonInputError(
            "client 裁切框超出原生 BMP："
            f"box=({left},{top},{right},{bottom}) full={native.width}x{native.height}"
        )
    return native.crop((left, top, right, bottom))


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def image_metrics(native: Image.Image, source: Image.Image) -> dict[str, Any]:
    if native.size != source.size:
        raise ComparisonInputError(
            f"client 区和源 PNG 尺寸不同：native={native.width}x{native.height} "
            f"source={source.width}x{source.height}；不会缩放后比较"
        )

    difference = ImageChops.difference(native, source)
    # 三个通道取最大差异，用于统计“至少一个通道不同”的像素数量。
    max_channel = ImageChops.lighter(
        ImageChops.lighter(difference.getchannel("R"), difference.getchannel("G")),
        difference.getchannel("B"),
    )
    histogram = max_channel.histogram()
    different_pixels = sum(histogram[1:])
    total_pixels = native.width * native.height
    nonzero_values = [index for index, count in enumerate(histogram) if count and index > 0]
    max_abs = max(nonzero_values, default=0)
    mean_abs = ImageStat.Stat(difference).mean
    rms = ImageStat.Stat(difference).rms

    return {
        "size": {"width": native.width, "height": native.height},
        "total_pixels": total_pixels,
        "different_pixels": different_pixels,
        "exact_equal_pixels": total_pixels - different_pixels,
        "difference_ratio": different_pixels / total_pixels,
        "mean_absolute_error": {
            "r": mean_abs[0],
            "g": mean_abs[1],
            "b": mean_abs[2],
        },
        "root_mean_square_error": {"r": rms[0], "g": rms[1], "b": rms[2]},
        "max_absolute_error": max_abs,
    }


def write_images(
    native: Image.Image,
    source: Image.Image,
    difference: Image.Image,
    output_dir: Path,
    diff_scale: int,
) -> dict[str, Path]:
    if diff_scale < 1:
        raise ComparisonInputError("--diff-scale 必须大于等于 1")
    output_dir.mkdir(parents=True, exist_ok=True)
    side_by_side = Image.new("RGB", (native.width * 2, native.height))
    side_by_side.paste(native, (0, 0))
    side_by_side.paste(source, (native.width, 0))
    # 叠加图保留原始 RGB，便于观察边缘和整体位移，不添加文字或标尺。
    overlay = Image.blend(native, source, 0.5)
    amplified_diff = difference.point(lambda value: min(255, value * diff_scale))

    artifacts = {
        "native_client": output_dir / "native-client.png",
        "source": output_dir / "source.png",
        "side_by_side": output_dir / "side-by-side.png",
        "overlay": output_dir / "overlay.png",
        "diff": output_dir / "diff.png",
    }
    native.save(artifacts["native_client"])
    source.save(artifacts["source"])
    side_by_side.save(artifacts["side_by_side"])
    overlay.save(artifacts["overlay"])
    amplified_diff.save(artifacts["diff"])
    return artifacts


def relative_artifact_map(artifacts: dict[str, Path], output_dir: Path) -> dict[str, str]:
    return {key: str(path.relative_to(output_dir)) for key, path in artifacts.items()}


def compare_regions(
    native: Image.Image,
    source: Image.Image,
    region_config: dict[str, Any],
    output_dir: Path,
) -> list[dict[str, Any]]:
    """为每个分区生成独立裁切对照图和像素统计，不改变全图统计。"""
    regions_dir = output_dir / "regions"
    regions_dir.mkdir(parents=True, exist_ok=True)
    comparisons: list[dict[str, Any]] = []
    for index, region in enumerate(region_config["regions"], start=1):
        box = (
            region["x"],
            region["y"],
            region["x"] + region["width"],
            region["y"] + region["height"],
        )
        native_crop = native.crop(box)
        source_crop = source.crop(box)
        comparison = image_metrics(native_crop, source_crop)
        side_by_side = Image.new("RGB", (native_crop.width * 2, native_crop.height))
        side_by_side.paste(native_crop, (0, 0))
        side_by_side.paste(source_crop, (native_crop.width, 0))
        side_by_side_path = regions_dir / f"region-{index:02d}-side-by-side.png"
        side_by_side.save(side_by_side_path)

        record: dict[str, Any] = {
            "name": region["name"],
            "rect": {
                "x": region["x"],
                "y": region["y"],
                "width": region["width"],
                "height": region["height"],
            },
            "comparison": comparison,
            "artifacts": {
                "side_by_side": str(side_by_side_path.relative_to(output_dir)),
            },
        }
        if "note" in region:
            record["note"] = region["note"]
        comparisons.append(record)
    return comparisons


def run(args: argparse.Namespace) -> int:
    geometry = read_client_geometry(args.metrics)
    native_full = load_rgb(args.native_bmp, "原生 BMP")
    source = load_rgb(args.source_png, "源 PNG")
    native_client = crop_client(native_full, geometry)
    comparison = image_metrics(native_client, source)
    difference = ImageChops.difference(native_client, source)
    artifacts = write_images(native_client, source, difference, args.output_dir, args.diff_scale)

    region_config = read_regions(args.regions_json) if args.regions_json else None
    region_comparisons = None
    if region_config is not None:
        expected_size = (
            region_config["image_size"]["width"],
            region_config["image_size"]["height"],
        )
        if expected_size != source.size or expected_size != native_client.size:
            raise ComparisonInputError(
                "分区 JSON 的 image_size 必须同时匹配 native client 和源 PNG："
                f"regions={expected_size[0]}x{expected_size[1]} "
                f"native={native_client.width}x{native_client.height} "
                f"source={source.width}x{source.height}"
            )
        region_comparisons = compare_regions(
            native_client, source, region_config, args.output_dir
        )

    result: dict[str, Any] = {
        "schema": SCHEMA,
        "schema_version": SCHEMA_VERSION,
        # 这是证据生成结果，不是产品验收判定；即使像素完全相同也保持 pending 语义。
        "acceptance": "pending",
        "assessment": "identical" if comparison["different_pixels"] == 0 else "not_identical",
        "source": {
            "path": str(args.source_png),
            "sha256": sha256_file(args.source_png),
            "commit": args.source_commit,
            "size": {"width": source.width, "height": source.height},
        },
        "native": {
            "path": str(args.native_bmp),
            "sha256": sha256_file(args.native_bmp),
            "full_size": {"width": native_full.width, "height": native_full.height},
            "metrics": str(args.metrics),
        },
        "geometry": geometry,
        "comparison": comparison,
        "artifacts": relative_artifact_map(artifacts, args.output_dir),
    }
    if region_config is not None and region_comparisons is not None:
        result["regions"] = {
            "input": region_config["path"],
            "image_size": region_config["image_size"],
            "comparisons": region_comparisons,
        }
        if "notes" in region_config:
            result["regions"]["notes"] = region_config["notes"]
    metrics_path = args.output_dir / "comparison-metrics.json"
    result["artifacts"]["metrics"] = str(metrics_path.relative_to(args.output_dir))
    metrics_path.write_text(
        json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(json.dumps(result, ensure_ascii=False, indent=2))
    if args.fail_on_difference and comparison["different_pixels"]:
        return 2
    return 0


def main() -> int:
    try:
        return run(parse_args())
    except ComparisonInputError as error:
        print(f"输入错误：{error}", file=sys.stderr)
        return 2
    except OSError as error:
        print(f"文件操作失败：{error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
