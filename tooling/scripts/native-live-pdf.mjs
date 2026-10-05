import { createHash } from 'node:crypto';

const ASCII_TRAILING_WHITESPACE = /[\x09\x0a\x0c\x0d\x20]+$/u;
const PDF_NUMBER = '[+-]?(?:\\d+(?:\\.\\d*)?|\\.\\d+)(?:[eE][+-]?\\d+)?';

function stripPdfStreams(body) {
  return body.replace(/\bstream\b[\s\S]*?\bendstream\b/gu, '');
}

function parseIndirectObjects(text) {
  // 这里只解析当前原生打印输出使用的普通间接对象和 PageTree；遇到压缩
  // object stream 或不完整字典会返回缺失尺寸并失败，不能用默认纸张尺寸放行。
  const objects = new Map();
  const objectPattern = /(?:^|\r?\n)\s*(\d+)\s+(\d+)\s+obj\b([\s\S]*?)(?:\r?\n)?endobj\b/gu;
  for (const match of text.matchAll(objectPattern)) {
    objects.set(Number(match[1]), {
      generation: Number(match[2]),
      body: stripPdfStreams(match[3]),
    });
  }
  return objects;
}

function referenceAfterKey(body, key) {
  const pattern = new RegExp(`/${key}\\s+(\\d+)\\s+\\d+\\s+R\\b`, 'u');
  const match = pattern.exec(body);
  return match ? Number(match[1]) : undefined;
}

function bracketContentAfterKey(body, key) {
  const marker = new RegExp(`/${key}\\s*\\[`, 'u');
  const match = marker.exec(body);
  if (!match) return undefined;
  const start = match.index + match[0].length;
  let depth = 1;
  for (let index = start; index < body.length; index += 1) {
    if (body[index] === '[') depth += 1;
    else if (body[index] === ']') {
      depth -= 1;
      if (depth === 0) return body.slice(start, index);
    }
  }
  return undefined;
}

function parseFourNumbers(value) {
  if (typeof value !== 'string') return undefined;
  const trimmed = value.trim();
  const numberPattern = new RegExp(`^${PDF_NUMBER}(?:\\s+${PDF_NUMBER}){3}$`, 'u');
  if (!numberPattern.test(trimmed)) return undefined;
  const numbers = trimmed.split(/\s+/u).map(Number);
  if (numbers.length !== 4 || numbers.some((number) => !Number.isFinite(number))) return undefined;
  const [left, bottom, right, top] = numbers;
  if (!(right > left) || !(top > bottom)) return undefined;
  return { widthPoints: right - left, heightPoints: top - bottom };
}

function mediaBoxFromBody(body, objects, visited) {
  const inline = bracketContentAfterKey(body, 'MediaBox');
  if (inline !== undefined) return parseFourNumbers(inline);
  const reference = referenceAfterKey(body, 'MediaBox');
  if (reference === undefined || visited.has(reference)) return undefined;
  const target = objects.get(reference);
  if (!target) return undefined;
  const nextVisited = new Set(visited);
  nextVisited.add(reference);
  return parseFourNumbers(target.body.replace(/^\s*\[|\]\s*$/gu, ''))
    ?? mediaBoxFromBody(target.body, objects, nextVisited);
}

function parentFromBody(body) {
  return referenceAfterKey(body, 'Parent');
}

function resolvePageMediaBox(objectNumber, objects, visited = new Set()) {
  if (visited.has(objectNumber)) return undefined;
  const object = objects.get(objectNumber);
  if (!object) return undefined;
  const nextVisited = new Set(visited);
  nextVisited.add(objectNumber);
  const ownMediaBox = mediaBoxFromBody(object.body, objects, nextVisited);
  if (ownMediaBox) return ownMediaBox;
  const parent = parentFromBody(object.body);
  return parent === undefined ? undefined : resolvePageMediaBox(parent, objects, nextVisited);
}

function pageMediaBoxes(objects) {
  const pages = [];
  for (const [objectNumber, object] of objects) {
    if (/\/Type\s*\/Page(?!s)\b/u.test(object.body)) pages.push(objectNumber);
  }
  pages.sort((left, right) => left - right);
  return pages.map((objectNumber) => {
    const mediaBox = resolvePageMediaBox(objectNumber, objects);
    if (!mediaBox) throw new Error(`PDF 页面 ${pages.indexOf(objectNumber) + 1} 缺少有效 MediaBox`);
    return mediaBox;
  });
}

function validateExpectedPageSize(pageSizes, expectedPageWidthPoints, expectedPageHeightPoints, tolerance) {
  const hasWidth = expectedPageWidthPoints !== undefined;
  const hasHeight = expectedPageHeightPoints !== undefined;
  if (hasWidth !== hasHeight) throw new Error('PDF 页面尺寸期望值必须同时提供宽度和高度');
  if (!hasWidth) return;
  if (!Number.isFinite(expectedPageWidthPoints) || expectedPageWidthPoints <= 0
      || !Number.isFinite(expectedPageHeightPoints) || expectedPageHeightPoints <= 0) {
    throw new Error('PDF 页面尺寸期望值无效');
  }
  if (!Number.isFinite(tolerance) || tolerance < 0) throw new Error('PDF 页面尺寸容差无效');
  for (const [index, page] of pageSizes.entries()) {
    if (Math.abs(page.widthPoints - expectedPageWidthPoints) > tolerance
        || Math.abs(page.heightPoints - expectedPageHeightPoints) > tolerance) {
      throw new Error(`PDF 页面尺寸不匹配: page=${index + 1}, width=${page.widthPoints}, height=${page.heightPoints}`);
    }
  }
}

/**
 * 只读取已保存的 PDF 字节并检查最小 PDF 对象结构；不接受扩展名、toast 或
 * 文本内容作为成功依据。对象中的 stream 会先剥离，避免把压缩数据里的字样
 * 当成页面对象。
 */
export function validatePdfBytes(bytes, {
  expectedPageCount,
  expectedPageWidthPoints,
  expectedPageHeightPoints,
  pageSizeTolerancePoints = 0.02,
} = {}) {
  if (!(bytes instanceof Uint8Array) || bytes.byteLength === 0) {
    throw new Error('PDF 字节为空');
  }
  if (!Number.isSafeInteger(expectedPageCount) || expectedPageCount < 1 || expectedPageCount > 1000) {
    throw new Error('PDF 页数期望值无效');
  }
  const text = Buffer.from(bytes).toString('latin1');
  if (!text.startsWith('%PDF-')) throw new Error('PDF header 无效');
  if (!ASCII_TRAILING_WHITESPACE.test(text) && !text.endsWith('%%EOF')) {
    throw new Error('PDF EOF 结构无效');
  }
  const withoutTrailingWhitespace = text.replace(ASCII_TRAILING_WHITESPACE, '');
  if (!withoutTrailingWhitespace.endsWith('%%EOF')) throw new Error('PDF EOF 结构无效');
  if (!/\bxref\b/u.test(text) && !/\/Type\s*\/XRef\b/u.test(text)) {
    throw new Error('PDF 缺少 xref 表或 xref stream');
  }
  if (!/\bstartxref\s+\d+/u.test(text)) throw new Error('PDF 缺少 startxref');
  if (!/\btrailer\b/u.test(text) && !/\/Type\s*\/XRef\b/u.test(text)) {
    throw new Error('PDF 缺少 trailer 或等价 xref 结构');
  }

  const objects = parseIndirectObjects(text);
  if (!objects.size) throw new Error('PDF 缺少间接对象');
  const pageSizes = pageMediaBoxes(objects);
  const pageCount = pageSizes.length;
  if (pageCount !== expectedPageCount) {
    throw new Error(`PDF 页数不匹配: expected=${expectedPageCount}, actual=${pageCount}`);
  }
  validateExpectedPageSize(
    pageSizes,
    expectedPageWidthPoints,
    expectedPageHeightPoints,
    pageSizeTolerancePoints,
  );
  return {
    sizeBytes: bytes.byteLength,
    sha256: createHash('sha256').update(bytes).digest('hex'),
    pageCount,
    pageSizes,
  };
}
